use crate::{
    catalog,
    types::{AppConfig, CodexInjectionMode},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};
use toml_edit::{value, DocumentMut, Item, Table};

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

    document["model_provider"] = value("neko-route");
    document["model_catalog_json"] = value(catalog_path.display().to_string());
    if let Some(model) = default_model
        .filter(|value| !value.trim().is_empty())
        .and_then(|model| resolve_config_model_id(config, model, third_party))
        .or_else(|| third_party.then_some(selected_model).flatten())
    {
        let export_slug = catalog::export_slug_for_model(config, allowed_model_ids, model)?;
        document["model"] = value(export_slug);
    }
    document["model_reasoning_summary"] = value("none");
    if let Some(reasoning_effort) = default_reasoning_effort(config, selected_model) {
        document["model_reasoning_effort"] = value(reasoning_effort);
    } else {
        document.as_table_mut().remove("model_reasoning_effort");
    }

    ensure_table(&mut document, "model_providers");
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
    provider["base_url"] = value("http://127.0.0.1:8787/v1");
    provider["wire_api"] = value("responses");
    provider["requires_openai_auth"] = value(!third_party);

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
        Some(selected.default_reasoning_level.trim().to_ascii_lowercase())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        inject_into, inject_into_with_model_filter, read_codex_config_file_from, restore_from,
        save_codex_config_file_to,
    };
    use crate::{
        store::normalize_config,
        types::{default_config, CodexInjectionMode, Provider, ProviderKind, ProviderProtocol},
    };
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn inject_and_restore_preserves_auth_file() {
        let dir = tempfile::tempdir().unwrap();
        let codex_home = dir.path();
        fs::write(codex_home.join("config.toml"), "model = \"gpt-5.5\"\n").unwrap();
        fs::write(codex_home.join("auth.json"), "{\"token\":\"keep\"}").unwrap();

        let result = inject_into(&default_config(), codex_home, Some("gpt-5.5")).unwrap();
        let config = fs::read_to_string(&result.config_path).unwrap();
        assert!(config.contains("model_provider = \"neko-route\""));
        assert!(config.contains("requires_openai_auth = true"));
        assert!(config.contains("model_reasoning_effort = \"xhigh\""));
        assert!(config.contains("model_reasoning_summary = \"none\""));
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
    fn inject_uses_selected_claude_reasoning_default() {
        let dir = tempfile::tempdir().unwrap();
        let result = inject_into(&default_config(), dir.path(), Some("claude-opus-4-8")).unwrap();
        let config = fs::read_to_string(&result.config_path).unwrap();

        assert!(config.contains("model = \"claude-opus-4-8\""));
        assert!(config.contains("model_reasoning_effort = \"max\""));
        assert!(config.contains("model_reasoning_summary = \"none\""));
    }

    #[test]
    fn inject_uses_chat_completions_reasoning_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = default_config();
        config.providers.push(Provider {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: "https://deepseek.example/v1".into(),
            enabled: true,
            key_ref: Some("provider:deepseek".into()),
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

        let result = inject_into(&default_config(), dir.path(), None).unwrap();
        let config = fs::read_to_string(&result.config_path).unwrap();

        assert!(config.contains("model = \"claude-opus-4-8\""));
        assert!(config.contains("model_reasoning_effort = \"max\""));
    }

    #[test]
    fn third_party_injection_writes_no_openai_auth_and_filtered_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = normalize_config(default_config());
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        config.settings.fallback_model = Some("claude-opus-4-8".into());
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
        assert!(config_toml.contains("model = \"gpt-5.4-mini\""));
        assert!(catalog.contains("\"slug\": \"gpt-5.4-mini\""));
        assert!(catalog.contains("\"display_name\": \"Claude Opus 4.8\""));
        assert!(!catalog.contains("gpt-5.5"));
    }

    #[test]
    fn third_party_injection_resolves_existing_alias_model() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.toml"), "model = \"gpt-5.4-mini\"\n").unwrap();
        let mut config = normalize_config(default_config());
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        config.settings.fallback_model = Some("claude-opus-4-8".into());
        let allowed = HashSet::from(["claude-opus-4-8".to_string()]);

        let result =
            inject_into_with_model_filter(&config, dir.path(), None, Some(&allowed)).unwrap();
        let config_toml = fs::read_to_string(&result.config_path).unwrap();

        assert!(config_toml.contains("model = \"gpt-5.4-mini\""));
        assert!(config_toml.contains("model_reasoning_effort = \"max\""));
    }

    #[test]
    fn third_party_injection_rejects_unavailable_default_or_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = default_config();
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
        let mut config = default_config();
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
