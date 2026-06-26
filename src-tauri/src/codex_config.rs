use crate::{
    catalog,
    catalog::CatalogModel,
    codex_alias,
    types::{
        codex_catalog_reasoning_level, AppConfig, CodexInjectionMode, ProviderProtocol, Settings,
    },
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};
use toml_edit::{value, DocumentMut, Item, Table};

/// Codex's catalog menu exposes a single context window for every model entry.
/// Pin it to 1M (and auto-compact to 90% of that) so selecting a smaller
/// default model never caps the usable context of the other models.
const CODEX_CONTEXT_WINDOW: u64 = 1_000_000;
/// 直连模式：Codex 连的是上游真实模型(gpt-5.x 约 272K)，按 258K 上报窗口、
/// 90% 自动压缩(232200)，避免 Codex 以为有 1M 而迟迟不压缩、撑爆上游 context。
const DIRECT_PROVIDER_CONTEXT_WINDOW: u64 = 258_000;

const RESTORE_FILE_NAME: &str = "neko-route-restore.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectionResult {
    pub codex_home: String,
    pub config_path: String,
    pub catalog_path: String,
    pub backup_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResult {
    pub codex_home: String,
    pub config_path: String,
    pub catalog_deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexConfigContent {
    pub codex_home: String,
    pub config_path: String,
    pub content: String,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManualSaveResult {
    pub codex_home: String,
    pub config_path: String,
    pub backup_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RestoreManifest {
    config_path: PathBuf,
    backup_path: PathBuf,
    catalog_path: PathBuf,
    config_existed: bool,
    created_at: String,
}

pub fn resolve_codex_home() -> PathBuf {
    if let Ok(value) = env::var("CODEX_HOME") {
        if !value.trim().is_empty() {
            return PathBuf::from(value);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
}

pub fn inject_with_model_filter(
    config: &AppConfig,
    default_model: Option<&str>,
    allowed_model_ids: Option<&HashSet<String>>,
) -> Result<InjectionResult, String> {
    inject_into_with_model_filter(
        config,
        &resolve_codex_home(),
        default_model,
        allowed_model_ids,
    )
}

pub fn inject_lan_share_config(
    settings: &Settings,
    default_model: Option<&str>,
    models: &[CatalogModel],
) -> Result<InjectionResult, String> {
    inject_lan_share_config_into(&resolve_codex_home(), settings, default_model, models)
}

pub fn inject_lan_share_config_into(
    codex_home: &Path,
    settings: &Settings,
    default_model: Option<&str>,
    models: &[CatalogModel],
) -> Result<InjectionResult, String> {
    if models.is_empty() {
        return Err("No LAN shared models are available for Codex injection".into());
    }
    fs::create_dir_all(codex_home).map_err(|error| error.to_string())?;
    let config_path = codex_home.join("config.toml");
    let backup_dir = codex_home.join("config-backups");
    fs::create_dir_all(&backup_dir).map_err(|error| error.to_string())?;

    let config_existed = config_path.exists();
    let original = if config_existed {
        fs::read_to_string(&config_path).map_err(|error| error.to_string())?
    } else {
        String::new()
    };

    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let backup_path = backup_dir.join(format!("neko-route-{timestamp}.toml"));
    fs::write(&backup_path, &original).map_err(|error| error.to_string())?;

    let mut document = original
        .parse::<DocumentMut>()
        .map_err(|error| format!("Failed to parse Codex config TOML: {error}"))?;
    let requested_default = default_model
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let selected = match requested_default {
        Some(model) => models
            .iter()
            .find(|entry| entry.slug == model || entry.real_target_model_id() == model)
            .ok_or_else(|| format!("LAN shared default model '{model}' is not available"))?,
        None => models
            .first()
            .ok_or_else(|| "No LAN shared models are available for Codex injection".to_string())?,
    };
    let catalog_path = catalog::write_catalog_models(codex_home, models)?;

    document["model_provider"] = value("neko-route");
    document["model_catalog_json"] = value(catalog_path.display().to_string());
    document["model"] = value(selected.slug.clone());
    document["model_reasoning_summary"] = value("none");
    if selected.reasoning_enabled {
        document["model_reasoning_effort"] = value(selected.default_reasoning_level.clone());
    } else {
        document.as_table_mut().remove("model_reasoning_effort");
    }
    write_fixed_context_window(&mut document, CODEX_CONTEXT_WINDOW)?;

    ensure_neko_route_provider(&mut document, settings, false)?;

    fs::write(&config_path, document.to_string()).map_err(|error| error.to_string())?;

    let manifest = RestoreManifest {
        config_path: config_path.clone(),
        backup_path: backup_path.clone(),
        catalog_path: catalog_path.clone(),
        config_existed,
        created_at: Utc::now().to_rfc3339(),
    };
    fs::write(
        restore_manifest_path(codex_home),
        serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    Ok(InjectionResult {
        codex_home: codex_home.display().to_string(),
        config_path: config_path.display().to_string(),
        catalog_path: catalog_path.display().to_string(),
        backup_path: backup_path.display().to_string(),
    })
}

pub fn restore(delete_catalog: bool) -> Result<RestoreResult, String> {
    restore_from(&resolve_codex_home(), delete_catalog)
}

pub fn read_codex_config_file() -> Result<CodexConfigContent, String> {
    read_codex_config_file_from(&resolve_codex_home())
}

pub fn save_codex_config_file(content: &str) -> Result<ManualSaveResult, String> {
    save_codex_config_file_to(&resolve_codex_home(), content)
}

pub fn read_codex_config_file_from(codex_home: &Path) -> Result<CodexConfigContent, String> {
    let config_path = codex_home.join("config.toml");
    let exists = config_path.exists();
    let content = if exists {
        fs::read_to_string(&config_path).map_err(|error| error.to_string())?
    } else {
        String::new()
    };

    Ok(CodexConfigContent {
        codex_home: codex_home.display().to_string(),
        config_path: config_path.display().to_string(),
        content,
        exists,
    })
}

pub fn save_codex_config_file_to(
    codex_home: &Path,
    content: &str,
) -> Result<ManualSaveResult, String> {
    content
        .parse::<DocumentMut>()
        .map_err(|error| format!("Failed to parse Codex config TOML: {error}"))?;

    fs::create_dir_all(codex_home).map_err(|error| error.to_string())?;
    let config_path = codex_home.join("config.toml");
    let backup_dir = codex_home.join("config-backups");
    fs::create_dir_all(&backup_dir).map_err(|error| error.to_string())?;

    let original = if config_path.exists() {
        fs::read_to_string(&config_path).map_err(|error| error.to_string())?
    } else {
        String::new()
    };
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let backup_path = backup_dir.join(format!("neko-route-manual-{timestamp}.toml"));
    fs::write(&backup_path, original).map_err(|error| error.to_string())?;
    fs::write(&config_path, content).map_err(|error| error.to_string())?;

    Ok(ManualSaveResult {
        codex_home: codex_home.display().to_string(),
        config_path: config_path.display().to_string(),
        backup_path: backup_path.display().to_string(),
    })
}

#[cfg(test)]
pub fn inject_into(
    config: &AppConfig,
    codex_home: &Path,
    default_model: Option<&str>,
) -> Result<InjectionResult, String> {
    inject_into_with_model_filter(config, codex_home, default_model, None)
}

pub fn inject_into_with_model_filter(
    config: &AppConfig,
    codex_home: &Path,
    default_model: Option<&str>,
    allowed_model_ids: Option<&HashSet<String>>,
) -> Result<InjectionResult, String> {
    let third_party = config.settings.codex_injection_mode == CodexInjectionMode::ThirdPartyApi;
    if third_party && allowed_model_ids.is_none() {
        return Err("Third-party API injection needs a verified model list".into());
    }
    if third_party && allowed_model_ids.map(HashSet::is_empty).unwrap_or(false) {
        return Err("No third-party API models are available for Codex injection".into());
    }

    fs::create_dir_all(codex_home).map_err(|error| error.to_string())?;
    let config_path = codex_home.join("config.toml");
    let backup_dir = codex_home.join("config-backups");
    fs::create_dir_all(&backup_dir).map_err(|error| error.to_string())?;

    let config_existed = config_path.exists();
    let original = if config_existed {
        fs::read_to_string(&config_path).map_err(|error| error.to_string())?
    } else {
        String::new()
    };

    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let backup_path = backup_dir.join(format!("neko-route-{timestamp}.toml"));
    fs::write(&backup_path, &original).map_err(|error| error.to_string())?;

    let mut document = original
        .parse::<DocumentMut>()
        .map_err(|error| format!("Failed to parse Codex config TOML: {error}"))?;

    let selected_model_raw = default_model
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            document
                .get("model")
                .and_then(Item::as_str)
                .map(str::to_string)
        });
    let selected_model = selected_model_raw
        .as_deref()
        .and_then(|model| resolve_config_model_id(config, model, third_party));
    if third_party && selected_model.is_none() {
        return Err("Codex default model is required for third-party API injection".into());
    }
    if let Some(model) = selected_model {
        validate_model_injectable(config, allowed_model_ids, model, "default model")?;
    }
    if let Some(fallback) = config
        .settings
        .fallback_model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        validate_model_injectable(config, allowed_model_ids, fallback, "fallback model")?;
    }

    let catalog_path = catalog::write_catalog_for_models(codex_home, config, allowed_model_ids)?;
    let direct_provider =
        config.settings.codex_injection_mode == CodexInjectionMode::DirectProvider;

    document["model_provider"] = value("neko-route");
    if direct_provider {
        // 直连模式：不写模型目录，让 Codex 用上游服务商的真实模型列表（我们的模型不参与）。
        document.as_table_mut().remove("model_catalog_json");
    } else {
        document["model_catalog_json"] = value(catalog_path.display().to_string());
    }
    document["model_reasoning_summary"] = value("none");
    if direct_provider {
        // 直连模式：default_model 是上游 /v1/models 的真实 slug（install 时拉取第一个），
        // 原样写入（不经 catalog 解析）；推理等级统一拉到超高(xhigh)。
        if let Some(slug) = default_model.filter(|value| !value.trim().is_empty()) {
            document["model"] = value(slug);
        }
        document["model_reasoning_effort"] = value("xhigh");
    } else {
        if let Some(model) = default_model
            .filter(|value| !value.trim().is_empty())
            .and_then(|model| resolve_config_model_id(config, model, third_party))
            .or_else(|| third_party.then_some(selected_model).flatten())
        {
            let export_slug = catalog::export_slug_for_model(config, allowed_model_ids, model)?;
            document["model"] = value(export_slug);
        }
        if let Some(reasoning_effort) = default_reasoning_effort(config, selected_model) {
            document["model_reasoning_effort"] = value(reasoning_effort);
        } else {
            document.as_table_mut().remove("model_reasoning_effort");
        }
    }
    write_fixed_context_window(
        &mut document,
        if direct_provider {
            DIRECT_PROVIDER_CONTEXT_WINDOW
        } else {
            CODEX_CONTEXT_WINDOW
        },
    )?;

    ensure_neko_route_provider(&mut document, &config.settings, !third_party)?;

    fs::write(&config_path, document.to_string()).map_err(|error| error.to_string())?;

    let manifest = RestoreManifest {
        config_path: config_path.clone(),
        backup_path: backup_path.clone(),
        catalog_path: catalog_path.clone(),
        config_existed,
        created_at: Utc::now().to_rfc3339(),
    };
    fs::write(
        restore_manifest_path(codex_home),
        serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    write_neko_image_skill(codex_home, &config.settings);

    Ok(InjectionResult {
        codex_home: codex_home.display().to_string(),
        config_path: config_path.display().to_string(),
        catalog_path: catalog_path.display().to_string(),
        backup_path: backup_path.display().to_string(),
    })
}

/// 往 `~/.codex/skills/neko-image/SKILL.md` 写画图 skill。配了 image_gen 模型才写,否则清理掉。
fn write_neko_image_skill(codex_home: &Path, settings: &Settings) {
    let skill_dir = codex_home.join("skills").join("neko-image");
    let configured = settings
        .image_gen_model
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty());
    if !configured {
        let _ = fs::remove_dir_all(&skill_dir);
        return;
    }
    if fs::create_dir_all(&skill_dir).is_err() {
        return;
    }
    let base = local_codex_base_url(settings);
    let endpoint = format!("{}/images/generations", base.trim_end_matches('/'));
    let _ = fs::write(
        skill_dir.join("SKILL.md"),
        neko_image_skill_markdown(&endpoint),
    );
}

fn neko_image_skill_markdown(endpoint: &str) -> String {
    let edits_endpoint = endpoint.replace("/images/generations", "/images/edits");
    format!(
        r#"---
name: neko-image
description: 通过 Neko Route 创建或编辑图片。当用户要求画图、改图时使用。
---

# neko-image

当用户要求**创建或编辑图片**(如 `/neko-image <描述>`)时，**不要用内置画图工具**，改用 Neko Route 的本地画图端点(会走用户在 Neko Route 配的画图模型)。

## 创建图片
把 <描述> 换成用户要画的内容，按需调整 size / n：

```bash
curl -s {endpoint} \
  -H "Content-Type: application/json" \
  -d '{{"prompt": "<描述>", "size": "1024x1024"}}' \
  | python3 -c "import sys,json,base64,time;d=json.load(sys.stdin)['data'][0];p=f'neko-image-{{int(time.time())}}.png';open(p,'wb').write(base64.b64decode(d['b64_json']));print('saved:',p)"
```

- `size`：方图 `1024x1024`、横图 `1536x1024`、竖图 `1024x1536`(按用户想要的比例选)；
- `n`：生成几张(默认 1)；
- **不要传 `quality` / `model`**，Neko Route 会用配置好的。

把保存的 PNG 路径展示给用户。

## 编辑图片
用户要"在上一张图基础上改 X"时，用编辑端点(把原图和描述一起发)：

```bash
curl -s {edits_endpoint} \
  -F image=@<上一张图路径> \
  -F prompt="<修改描述>" \
  | python3 -c "import sys,json,base64,time;d=json.load(sys.stdin)['data'][0];p=f'neko-image-{{int(time.time())}}.png';open(p,'wb').write(base64.b64decode(d['b64_json']));print('saved:',p)"
```

**记住上一次保存的图片路径**，编辑时传给 `image=@`。

## 注意
- 若返回 "image_gen not configured"，提示用户去 Neko Route 的 Codex 配置里选一个 image_gen 模型。
"#,
        endpoint = endpoint,
        edits_endpoint = edits_endpoint,
    )
}

fn ensure_neko_route_provider(
    document: &mut DocumentMut,
    settings: &Settings,
    requires_openai_auth: bool,
) -> Result<(), String> {
    ensure_table(document, "model_providers");
    let providers = document["model_providers"]
        .as_table_mut()
        .ok_or_else(|| "Failed to create model_providers table".to_string())?;
    let provider_is_table = providers
        .get("neko-route")
        .map(Item::is_table)
        .unwrap_or(false);
    if !provider_is_table {
        providers.insert("neko-route", Item::Table(Table::new()));
    }
    let provider = providers
        .get_mut("neko-route")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| "Failed to create neko-route provider table".to_string())?;
    provider["name"] = value("neko-route");
    provider["base_url"] = value(local_codex_base_url(settings));
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(requires_openai_auth);
    Ok(())
}

pub fn local_codex_base_url(settings: &Settings) -> String {
    let host = settings.bind_host.trim();
    let host = match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(addr)) if addr.is_unspecified() => "127.0.0.1".to_string(),
        Ok(std::net::IpAddr::V4(addr)) => addr.to_string(),
        Ok(std::net::IpAddr::V6(addr)) if addr.is_unspecified() => "127.0.0.1".to_string(),
        Ok(std::net::IpAddr::V6(addr)) => format!("[{addr}]"),
        Err(_) if host.is_empty() => "127.0.0.1".to_string(),
        Err(_) => host.to_string(),
    };
    format!("http://{}:{}/v1", host, settings.port)
}

fn validate_model_injectable(
    config: &AppConfig,
    allowed_model_ids: Option<&HashSet<String>>,
    model_id: &str,
    label: &str,
) -> Result<(), String> {
    let exists = config.models.iter().any(|model| {
        model.enabled
            && model.id == model_id
            && allowed_model_ids
                .map(|allowed| allowed.contains(&model.id))
                .unwrap_or(true)
    });
    if exists {
        Ok(())
    } else {
        Err(format!(
            "Codex {label} '{model_id}' is not available in the selected injection mode"
        ))
    }
}

fn resolve_config_model_id<'a>(
    config: &'a AppConfig,
    value: &str,
    include_aliases: bool,
) -> Option<&'a str> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if include_aliases {
        if let Some(model) = codex_alias::resolve_slot_model(config, value) {
            return Some(model.id.as_str());
        }
    }
    config
        .models
        .iter()
        .find(|model| model.id == value)
        .or_else(|| {
            if !include_aliases {
                return None;
            }
            config
                .models
                .iter()
                .find(|model| model.codex_alias.as_deref() == Some(value))
        })
        .map(|model| model.id.as_str())
}

pub fn restore_from(codex_home: &Path, delete_catalog: bool) -> Result<RestoreResult, String> {
    let manifest_path = restore_manifest_path(codex_home);
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|_| "No Neko Route restore manifest was found".to_string())?;
    let manifest: RestoreManifest =
        serde_json::from_str(&raw).map_err(|error| error.to_string())?;

    if manifest.config_existed {
        let backup =
            fs::read_to_string(&manifest.backup_path).map_err(|error| error.to_string())?;
        fs::write(&manifest.config_path, backup).map_err(|error| error.to_string())?;
    } else if manifest.config_path.exists() {
        fs::remove_file(&manifest.config_path).map_err(|error| error.to_string())?;
    }

    let mut catalog_deleted = false;
    if delete_catalog && manifest.catalog_path.exists() {
        fs::remove_file(&manifest.catalog_path).map_err(|error| error.to_string())?;
        catalog_deleted = true;
    }
    let _ = fs::remove_file(&manifest_path);

    Ok(RestoreResult {
        codex_home: codex_home.display().to_string(),
        config_path: manifest.config_path.display().to_string(),
        catalog_deleted,
    })
}

fn restore_manifest_path(codex_home: &Path) -> PathBuf {
    codex_home.join(RESTORE_FILE_NAME)
}

fn ensure_table(document: &mut DocumentMut, key: &str) {
    let is_table = document
        .as_table()
        .get(key)
        .map(Item::is_table)
        .unwrap_or(false);
    if !is_table {
        document[key] = Item::Table(Table::new());
    }
}

fn default_reasoning_effort(config: &AppConfig, default_model: Option<&str>) -> Option<String> {
    let selected = default_model
        .and_then(|model| {
            config
                .models
                .iter()
                .find(|entry| entry.enabled && entry.id == model)
        })
        .or_else(|| config.models.iter().find(|entry| entry.enabled))?;

    if selected.reasoning_enabled && !selected.default_reasoning_level.trim().is_empty() {
        // Codex catalog 不支持 max：Claude 默认档下移一档(请求转发时再上移回真实档)。
        let anthropic = config
            .providers
            .iter()
            .find(|provider| provider.id == selected.provider_id)
            .map(|provider| provider.protocol == ProviderProtocol::AnthropicMessages)
            .unwrap_or(false);
        Some(
            codex_catalog_reasoning_level(&selected.default_reasoning_level, anthropic).to_string(),
        )
    } else {
        None
    }
}

fn write_fixed_context_window(
    document: &mut DocumentMut,
    context_window: u64,
) -> Result<(), String> {
    let context_window_value = i64::try_from(context_window)
        .map_err(|_| "Codex model context window is too large".to_string())?;
    let auto_compact_value = i64::try_from(catalog::auto_compact_token_limit(context_window))
        .map_err(|_| "Codex auto compact token limit is too large".to_string())?;
    document["model_context_window"] = value(context_window_value);
    document["model_auto_compact_token_limit"] = value(auto_compact_value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        inject_into, inject_into_with_model_filter, inject_lan_share_config_into,
        local_codex_base_url, read_codex_config_file_from, restore_from, save_codex_config_file_to,
    };
    use crate::{
        catalog::CatalogModel,
        store::normalize_config,
        types::{seeded_config, CodexInjectionMode, Provider, ProviderKind, ProviderProtocol},
    };
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn inject_and_restore_preserves_auth_file() {
        let dir = tempfile::tempdir().unwrap();
        let codex_home = dir.path();
        fs::write(codex_home.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();
        fs::write(codex_home.join("auth.json"), "{\"token\":\"keep\"}").unwrap();

        let result = inject_into(&seeded_config(), codex_home, Some("gpt-5.5")).unwrap();
        let config = fs::read_to_string(&result.config_path).unwrap();
        assert!(config.contains("model_provider = \"neko-route\""));
        assert!(config.contains("requires_openai_auth = true"));
        assert!(config.contains("model_reasoning_effort = \"xhigh\""));
        assert!(config.contains("model_reasoning_summary = \"none\""));
        assert!(config.contains("model_context_window = 1000000"));
        assert!(config.contains("model_auto_compact_token_limit = 900000"));
        assert_eq!(
            fs::read_to_string(codex_home.join("auth.json")).unwrap(),
            "{\"token\":\"keep\"}"
        );

        restore_from(codex_home, true).unwrap();
        assert_eq!(
            fs::read_to_string(codex_home.join("config.toml")).unwrap(),
            "model = \"gpt-5.5\"\n"
        );
        assert_eq!(
            fs::read_to_string(codex_home.join("auth.json")).unwrap(),
            "{\"token\":\"keep\"}"
        );
    }

    #[test]
    fn direct_provider_mode_writes_upstream_model_xhigh_and_258k() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = seeded_config();
        config.settings.codex_injection_mode = CodexInjectionMode::DirectProvider;
        config.settings.direct_provider_id = Some(config.providers[0].id.clone());
        // 直连模式下 default_model 由 install 传入上游 /v1/models 第一个真实 slug。
        let result = inject_into(&config, dir.path(), Some("gpt-5.5")).unwrap();
        let toml = fs::read_to_string(&result.config_path).unwrap();
        // model 行用上游真实 slug 原样写入（不经 catalog 解析成 neko-model id）。
        assert!(toml.contains("model = \"gpt-5.5\""), "{toml}");
        // 推理等级统一拉到超高(xhigh)。
        assert!(
            toml.contains("model_reasoning_effort = \"xhigh\""),
            "{toml}"
        );
        // 按上游真实窗口 258K 上报、90% 自动压缩(232200)，避免 Codex 以为有 1M 撑爆上游。
        assert!(toml.contains("model_context_window = 258000"), "{toml}");
        assert!(
            toml.contains("model_auto_compact_token_limit = 232200"),
            "{toml}"
        );
        // 直连不写 catalog，让 Codex 用上游真实模型列表。
        assert!(!toml.contains("model_catalog_json"), "{toml}");
    }

    #[test]
    fn inject_uses_selected_claude_reasoning_default() {
        let dir = tempfile::tempdir().unwrap();
        let result = inject_into(&seeded_config(), dir.path(), Some("claude-opus-4-8")).unwrap();
        let config = fs::read_to_string(&result.config_path).unwrap();

        assert!(config.contains("model = \"claude-opus-4-8\""));
        assert!(config.contains("model_reasoning_effort = \"xhigh\""));
        assert!(config.contains("model_reasoning_summary = \"none\""));
        assert!(config.contains("model_context_window = 1000000"));
        assert!(config.contains("model_auto_compact_token_limit = 900000"));
    }

    #[test]
    fn inject_pins_context_window_to_one_million_regardless_of_default_model() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "model = \"gpt-5.5\"\nmodel_context_window = 200000\nmodel_auto_compact_token_limit = 180000\n",
        )
        .unwrap();
        let mut config = seeded_config();
        config
            .models
            .iter_mut()
            .find(|model| model.id == "claude-opus-4-8")
            .unwrap()
            .context_window = 258_000;

        let result = inject_into(&config, dir.path(), Some("claude-opus-4-8")).unwrap();
        let config_toml = fs::read_to_string(&result.config_path).unwrap();

        assert!(config_toml.contains("model = \"claude-opus-4-8\""));
        // The pinned window must not follow the selected model's smaller context.
        assert!(config_toml.contains("model_context_window = 1000000"));
        assert!(config_toml.contains("model_auto_compact_token_limit = 900000"));
        assert!(!config_toml.contains("model_context_window = 258000"));
        assert!(!config_toml.contains("model_context_window = 200000"));
    }

    #[test]
    fn inject_writes_fixed_context_window_when_no_default_model_is_available() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "model_context_window = 200000\nmodel_auto_compact_token_limit = 180000\n",
        )
        .unwrap();

        let result = inject_into(&seeded_config(), dir.path(), None).unwrap();
        let config_toml = fs::read_to_string(&result.config_path).unwrap();

        assert!(config_toml.contains("model_context_window = 1000000"));
        assert!(config_toml.contains("model_auto_compact_token_limit = 900000"));
    }

    #[test]
    fn inject_uses_chat_completions_reasoning_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = seeded_config();
        config.providers.push(Provider {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: "https://deepseek.example/v1".into(),
            key_ref: Some("provider:deepseek".into()),
            http_proxy: Default::default(),
        });
        let mut model = config.models[0].clone();
        model.id = "deepseek-v4-pro".into();
        model.display_name = "DeepSeek V4 Pro".into();
        model.provider_id = "deepseek".into();
        config.models.push(model);
        let config = normalize_config(config);

        let result = inject_into(&config, dir.path(), Some("deepseek-v4-pro")).unwrap();
        let config = fs::read_to_string(&result.config_path).unwrap();

        assert!(config.contains("model = \"deepseek-v4-pro\""));
        assert!(config.contains("model_reasoning_effort = \"xhigh\""));
        assert!(config.contains("model_reasoning_summary = \"none\""));
    }

    #[test]
    fn inject_preserves_existing_model_reasoning_default() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "model = \"claude-opus-4-8\"\n",
        )
        .unwrap();

        let result = inject_into(&seeded_config(), dir.path(), None).unwrap();
        let config = fs::read_to_string(&result.config_path).unwrap();

        assert!(config.contains("model = \"claude-opus-4-8\""));
        assert!(config.contains("model_reasoning_effort = \"xhigh\""));
    }

    #[test]
    fn third_party_injection_writes_no_openai_auth_and_filtered_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = normalize_config(seeded_config());
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        config.settings.fallback_model = Some("claude-opus-4-8".into());
        let config = normalize_config(config);
        let allowed = HashSet::from(["claude-opus-4-8".to_string()]);

        let result = inject_into_with_model_filter(
            &config,
            dir.path(),
            Some("claude-opus-4-8"),
            Some(&allowed),
        )
        .unwrap();
        let config_toml = fs::read_to_string(&result.config_path).unwrap();
        let catalog = fs::read_to_string(&result.catalog_path).unwrap();

        assert!(config_toml.contains("requires_openai_auth = false"));
        assert!(config_toml.contains("model = \"gpt-5.5\""));
        assert!(config_toml.contains("model_context_window = 1000000"));
        assert!(config_toml.contains("model_auto_compact_token_limit = 900000"));
        assert!(catalog.contains("\"slug\": \"gpt-5.5\""));
        assert!(catalog.contains("\"display_name\": \"Claude Opus 4.8\""));
        assert!(!catalog.contains("\"slug\": \"claude-opus-4-8\""));
    }

    #[test]
    fn lan_share_injection_writes_slot_model_with_remote_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = seeded_config().settings;
        settings.port = 9898;
        let models = vec![CatalogModel {
            slug: "gpt-5.5".into(),
            target_model_id: Some("remote-gpt".into()),
            display_name: "Remote GPT".into(),
            description: "LAN model".into(),
            context_window: 258_000,
            reasoning_enabled: true,
            default_reasoning_level: "high".into(),
            supported_reasoning_levels: vec!["low".into(), "high".into()],
            provider_protocol: None,
        }];

        let result =
            inject_lan_share_config_into(dir.path(), &settings, Some("remote-gpt"), &models)
                .unwrap();
        let config_toml = fs::read_to_string(&result.config_path).unwrap();
        let catalog = fs::read_to_string(&result.catalog_path).unwrap();

        assert!(config_toml.contains("requires_openai_auth = false"));
        assert!(config_toml.contains("base_url = \"http://127.0.0.1:9898/v1\""));
        assert!(config_toml.contains("model = \"gpt-5.5\""));
        assert!(config_toml.contains("model_context_window = 1000000"));
        assert!(config_toml.contains("model_auto_compact_token_limit = 900000"));
        assert!(catalog.contains("\"slug\": \"gpt-5.5\""));
        assert!(catalog.contains("\"display_name\": \"Remote GPT\""));
        assert!(!catalog.contains("\"slug\": \"remote-gpt\""));
    }

    #[test]
    fn local_codex_base_url_uses_current_port_and_loopback_for_public_bind() {
        let mut settings = seeded_config().settings;
        settings.bind_host = "0.0.0.0".into();
        settings.port = 9898;

        assert_eq!(local_codex_base_url(&settings), "http://127.0.0.1:9898/v1");
    }

    #[test]
    fn third_party_injection_resolves_existing_slot_model() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();
        let mut config = normalize_config(seeded_config());
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        config.settings.fallback_model = Some("claude-opus-4-8".into());
        let mut config = normalize_config(config);
        config
            .models
            .iter_mut()
            .find(|model| model.id == "claude-opus-4-8")
            .unwrap()
            .context_window = 258_000;
        let allowed = HashSet::from(["claude-opus-4-8".to_string()]);

        let result =
            inject_into_with_model_filter(&config, dir.path(), None, Some(&allowed)).unwrap();
        let config_toml = fs::read_to_string(&result.config_path).unwrap();

        assert!(config_toml.contains("model = \"gpt-5.5\""));
        assert!(config_toml.contains("model_reasoning_effort = \"xhigh\""));
        assert!(config_toml.contains("model_context_window = 1000000"));
        assert!(config_toml.contains("model_auto_compact_token_limit = 900000"));
    }

    #[test]
    fn third_party_injection_rejects_unavailable_default_or_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = seeded_config();
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        let allowed = HashSet::from(["claude-opus-4-8".to_string()]);

        let error =
            inject_into_with_model_filter(&config, dir.path(), Some("gpt-5.5"), Some(&allowed))
                .unwrap_err();
        assert!(error.contains("default model"));

        config.settings.fallback_model = Some("gpt-5.5".into());
        let error = inject_into_with_model_filter(
            &config,
            dir.path(),
            Some("claude-opus-4-8"),
            Some(&allowed),
        )
        .unwrap_err();
        assert!(error.contains("fallback model"));
    }

    #[test]
    fn third_party_injection_requires_default_model() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = seeded_config();
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        let allowed = HashSet::from(["claude-opus-4-8".to_string()]);

        let error =
            inject_into_with_model_filter(&config, dir.path(), None, Some(&allowed)).unwrap_err();
        assert!(error.contains("default model"));
    }

    #[test]
    fn manual_config_save_validates_toml_and_creates_backup() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "model = \"old\"\n").unwrap();

        let result = save_codex_config_file_to(dir.path(), "model = \"new\"\n").unwrap();
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "model = \"new\"\n"
        );
        assert_eq!(
            fs::read_to_string(&result.backup_path).unwrap(),
            "model = \"old\"\n"
        );

        let loaded = read_codex_config_file_from(dir.path()).unwrap();
        assert!(loaded.exists);
        assert_eq!(loaded.content, "model = \"new\"\n");

        assert!(save_codex_config_file_to(dir.path(), "model = ").is_err());
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            "model = \"new\"\n"
        );
    }
}
