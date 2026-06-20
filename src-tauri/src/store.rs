use crate::codex_alias;
use crate::key_store::KeyStore;
use crate::request_log::RequestLog;
use crate::types::{
    default_config, reasoning_defaults_for_protocol, AppConfig, AppSnapshot, CodexInjectionMode,
    KeyStatus, OfficialAccountQuota, Provider, ProviderKind, ProviderLocalUsage,
    ProviderUsageStatus, RequestLogPage, RequestRecord, ServerStatus, Settings, TokenStats,
    TokenUsage,
};
use crate::{catalog, claude_auth, official_auth};
use serde_json::to_string_pretty;
use std::{
    collections::{HashMap, HashSet},
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::RwLock;

const CURRENT_CONFIG_VERSION: u32 = 10;
const DASHBOARD_RECENT_REQUESTS: usize = 6;

#[derive(Clone)]
pub struct AppStore {
    inner: Arc<AppStoreInner>,
}

struct AppStoreInner {
    config_path: PathBuf,
    config: RwLock<AppConfig>,
    log: Arc<RequestLog>,
    server_status: RwLock<ServerStatus>,
    key_store: KeyStore,
}

impl AppStore {
    pub fn load(app_data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&app_data_dir).map_err(|error| error.to_string())?;
        let config_path = app_data_dir.join("config.json");
        let config = if config_path.exists() {
            let raw = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;
            match serde_json::from_str::<AppConfig>(&raw) {
                Ok(parsed) => normalize_config(parsed),
                Err(_) => normalize_config(reset_config_preserving_settings(&raw)),
            }
        } else {
            let config = normalize_config(default_config());
            fs::write(&config_path, to_string_pretty(&config).unwrap())
                .map_err(|error| error.to_string())?;
            config
        };
        fs::write(&config_path, to_string_pretty(&config).unwrap())
            .map_err(|error| error.to_string())?;

        let log = RequestLog::open(&app_data_dir.join("requests.db"))?;

        let bind_url = format!(
            "http://{}:{}/v1",
            config.settings.bind_host, config.settings.port
        );
        Ok(Self {
            inner: Arc::new(AppStoreInner {
                config_path,
                config: RwLock::new(config),
                log: Arc::new(log),
                server_status: RwLock::new(ServerStatus {
                    bind_url,
                    running: false,
                    error: None,
                }),
                key_store: KeyStore::new(&app_data_dir),
            }),
        })
    }

    pub async fn config(&self) -> AppConfig {
        self.inner.config.read().await.clone()
    }

    pub async fn replace_config(&self, config: AppConfig) -> Result<(), String> {
        let previous = self.config().await;
        let config = normalize_config(config);
        validate_bind_settings(&config)?;
        validate_provider_urls(&config)?;
        validate_enabled_model_ids(&config)?;
        self.delete_removed_provider_keys(&previous, &config)?;
        self.write_config_file(&config)?;
        *self.inner.config.write().await = config.clone();

        // Auto-inject: whenever auto_inject is on, re-write the Codex config so
        // model changes (toggles, deletions, fallback) take effect immediately.
        if config.settings.auto_inject {
            let default_model = config
                .settings
                .codex_default_model
                .clone()
                .filter(|value| !value.trim().is_empty());
            // Best-effort: never fail a config save because injection had trouble.
            let _ = self.inject_codex_config_for(&config, default_model.as_deref());
        }
        Ok(())
    }

    pub fn write_config_file(&self, config: &AppConfig) -> Result<(), String> {
        fs::write(
            &self.inner.config_path,
            to_string_pretty(config).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }

    pub async fn snapshot(&self) -> AppSnapshot {
        let config = self.config().await;
        let keys = self.key_statuses(&config);
        let log = self.inner.log.clone();
        let (requests, stats, request_log_count, local_usage, usage_snapshots) =
            tokio::task::spawn_blocking(move || {
                (
                    log.recent(DASHBOARD_RECENT_REQUESTS),
                    log.stats(),
                    log.count(),
                    log.provider_local_usage(),
                    log.provider_usage_snapshots(),
                )
            })
            .await
            .unwrap_or_else(|_| (Vec::new(), TokenStats::default(), 0, Vec::new(), Vec::new()));
        let server = self.inner.server_status.read().await.clone();
        let provider_usage = merge_provider_usage(&config.providers, local_usage, usage_snapshots);
        AppSnapshot {
            config,
            keys,
            server,
            requests,
            request_log_count,
            stats,
            provider_usage,
            codex_home: crate::codex_config::resolve_codex_home()
                .display()
                .to_string(),
        }
    }

    pub fn key_store(&self) -> &KeyStore {
        &self.inner.key_store
    }

    pub fn key_statuses(&self, config: &AppConfig) -> Vec<KeyStatus> {
        config
            .providers
            .iter()
            .map(|provider| self.inner.key_store.status_for_provider(provider))
            .collect()
    }

    pub async fn push_request(&self, record: RequestRecord) {
        let log = self.inner.log.clone();
        let _ = tokio::task::spawn_blocking(move || log.insert(&record)).await;
    }

    pub async fn update_request_stream_progress(
        &self,
        id: String,
        stream_bytes: u64,
        usage: Option<TokenUsage>,
    ) {
        let usage = usage.filter(|usage| !usage.is_empty());
        if stream_bytes == 0 && usage.is_none() {
            return;
        }
        let log = self.inner.log.clone();
        let _ = tokio::task::spawn_blocking(move || {
            log.update_stream_progress(&id, stream_bytes, usage.as_ref())
        })
        .await;
    }

    pub async fn update_request_stream(
        &self,
        id: String,
        stream_state: String,
        stream_error: Option<String>,
        last_event: Option<String>,
    ) {
        let log = self.inner.log.clone();
        let _ = tokio::task::spawn_blocking(move || {
            log.update_stream_status(
                &id,
                &stream_state,
                stream_error.as_deref(),
                last_event.as_deref(),
            )
        })
        .await;
    }

    pub async fn clear_requests(&self) {
        let log = self.inner.log.clone();
        let _ = tokio::task::spawn_blocking(move || log.clear()).await;
    }

    pub async fn request_log_page(&self, page: usize, page_size: usize) -> RequestLogPage {
        let log = self.inner.log.clone();
        tokio::task::spawn_blocking(move || log.page(page, page_size))
            .await
            .unwrap_or_else(|_| RequestLogPage {
                records: Vec::new(),
                total: 0,
                page: page.max(1),
                page_size: page_size.clamp(1, 200),
            })
    }

    pub async fn update_provider_usage_snapshot(
        &self,
        provider_id: String,
        source: String,
        quota: Option<OfficialAccountQuota>,
        error: Option<String>,
    ) {
        let log = self.inner.log.clone();
        let _ = tokio::task::spawn_blocking(move || {
            log.upsert_provider_usage_snapshot(
                &provider_id,
                &source,
                quota.as_ref(),
                error.as_deref(),
            )
        })
        .await;
    }

    pub async fn set_server_running(&self, bind_url: String) {
        *self.inner.server_status.write().await = ServerStatus {
            bind_url,
            running: true,
            error: None,
        };
    }

    pub async fn set_server_error(&self, bind_url: String, error: String) {
        *self.inner.server_status.write().await = ServerStatus {
            bind_url,
            running: false,
            error: Some(error),
        };
    }

    pub async fn export_catalog_to(&self, codex_home: &Path) -> Result<PathBuf, String> {
        let config = self.config().await;
        match self.codex_allowed_model_ids(&config)? {
            Some(allowed) => catalog::write_catalog_for_models(codex_home, &config, Some(&allowed)),
            None => catalog::write_catalog(codex_home, &config),
        }
    }

    pub fn inject_codex_config_for(
        &self,
        config: &AppConfig,
        default_model: Option<&str>,
    ) -> Result<crate::codex_config::InjectionResult, String> {
        let allowed = self.codex_allowed_model_ids(config)?;
        crate::codex_config::inject_with_model_filter(config, default_model, allowed.as_ref())
    }

    pub fn codex_allowed_model_ids(
        &self,
        config: &AppConfig,
    ) -> Result<Option<HashSet<String>>, String> {
        if config.settings.codex_injection_mode != CodexInjectionMode::ThirdPartyApi {
            return Ok(None);
        }

        let mut provider_ids = HashSet::new();
        for provider in &config.providers {
            if self.third_party_provider_available(provider)? {
                provider_ids.insert(provider.id.clone());
            }
        }

        let model_ids = config
            .models
            .iter()
            .filter(|model| model.enabled && provider_ids.contains(&model.provider_id))
            .map(|model| model.id.clone())
            .collect::<HashSet<_>>();

        Ok(Some(model_ids))
    }

    fn third_party_provider_available(&self, provider: &Provider) -> Result<bool, String> {
        match provider.kind {
            ProviderKind::OfficialOpenAi => Ok(false),
            ProviderKind::OfficialOpenAiAccount | ProviderKind::OfficialAnthropicAccount => {
                let status = official_auth::status_for_provider(provider);
                Ok(status.present && status.available)
            }
            ProviderKind::OfficialAnthropicCli => Ok(claude_auth::cli_auth().is_ok()),
            ProviderKind::OfficialAnthropicDesktop => Ok(claude_auth::desktop_auth().is_ok()),
            ProviderKind::Custom => {
                let Some(key_ref) = provider.key_ref.as_deref() else {
                    return Ok(true);
                };
                self.inner
                    .key_store
                    .get_secret(key_ref)
                    .map(|secret| secret.is_some())
            }
        }
    }

    fn delete_removed_provider_keys(
        &self,
        previous: &AppConfig,
        next: &AppConfig,
    ) -> Result<(), String> {
        let next_refs = next
            .providers
            .iter()
            .filter_map(|provider| provider.key_ref.as_deref())
            .collect::<HashSet<_>>();
        for key_ref in previous
            .providers
            .iter()
            .filter_map(|provider| provider.key_ref.as_deref())
        {
            if !next_refs.contains(key_ref) {
                self.inner.key_store.delete_secret(key_ref)?;
            }
        }
        let next_provider_ids = next
            .providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect::<HashSet<_>>();
        for provider in previous.providers.iter().filter(|provider| {
            matches!(
                provider.kind,
                ProviderKind::OfficialOpenAiAccount | ProviderKind::OfficialAnthropicAccount
            ) && !next_provider_ids.contains(provider.id.as_str())
        }) {
            official_auth::delete_provider_token(&provider.id)?;
        }
        Ok(())
    }
}

fn merge_provider_usage(
    providers: &[Provider],
    local_usage: Vec<ProviderLocalUsage>,
    snapshots: Vec<ProviderUsageStatus>,
) -> Vec<ProviderUsageStatus> {
    let mut local = local_usage
        .into_iter()
        .map(|usage| (usage.provider_id.clone(), usage))
        .collect::<HashMap<_, _>>();
    let mut snapshots = snapshots
        .into_iter()
        .map(|snapshot| (snapshot.provider_id.clone(), snapshot))
        .collect::<HashMap<_, _>>();

    providers
        .iter()
        .map(|provider| {
            let usage = local
                .remove(&provider.id)
                .unwrap_or_else(|| ProviderLocalUsage {
                    provider_id: provider.id.clone(),
                    ..ProviderLocalUsage::default()
                });
            let mut snapshot =
                snapshots
                    .remove(&provider.id)
                    .unwrap_or_else(|| ProviderUsageStatus {
                        provider_id: provider.id.clone(),
                        source: "local".into(),
                        ..ProviderUsageStatus::default()
                    });
            snapshot.local_usage = usage;
            snapshot
        })
        .collect()
}

pub fn validate_bind_settings(config: &AppConfig) -> Result<(), String> {
    let ip: IpAddr = config
        .settings
        .bind_host
        .parse()
        .map_err(|_| "Bind host must be an IP address".to_string())?;
    if !config.settings.allow_lan && !ip.is_loopback() {
        return Err("Non-localhost listening requires Allow LAN to be enabled".into());
    }
    Ok(())
}

pub fn normalize_config(mut config: AppConfig) -> AppConfig {
    config.version = CURRENT_CONFIG_VERSION;

    let official = official_providers();
    let official_ids = official
        .iter()
        .map(|provider| provider.id.clone())
        .collect::<HashSet<_>>();
    let mut used_ids = official_ids.clone();
    let mut providers = official;

    for mut provider in config.providers.into_iter() {
        if official_ids.contains(provider.id.as_str()) {
            continue;
        }
        match provider.kind {
            ProviderKind::Custom => {
                provider.id = unique_provider_id(&provider.id, &mut used_ids);
                provider.name = provider.name.trim().to_string();
                if provider.name.is_empty() {
                    provider.name = "Custom Provider".into();
                }
                provider.base_url = provider.base_url.trim().to_string();
                if provider.key_ref.is_some() {
                    provider.key_ref = Some(format!("provider:{}", provider.id));
                }
                providers.push(provider);
            }
            ProviderKind::OfficialOpenAiAccount => {
                provider.id = unique_provider_id(&provider.id, &mut used_ids);
                provider.name = provider.name.trim().to_string();
                if provider.name.is_empty() {
                    provider.name = "OpenAI Account".into();
                }
                provider.protocol = crate::types::ProviderProtocol::OpenAiResponses;
                provider.base_url = "https://api.openai.com/v1".into();
                provider.key_ref = Some(official_auth::token_ref(&provider.id));
                providers.push(provider);
            }
            ProviderKind::OfficialAnthropicAccount => {
                provider.id = unique_provider_id(&provider.id, &mut used_ids);
                provider.name = provider.name.trim().to_string();
                if provider.name.is_empty() {
                    provider.name = "Claude Account".into();
                }
                provider.protocol = crate::types::ProviderProtocol::AnthropicMessages;
                provider.base_url = "https://api.anthropic.com/v1".into();
                provider.key_ref = Some(official_auth::token_ref(&provider.id));
                providers.push(provider);
            }
            ProviderKind::OfficialOpenAi
            | ProviderKind::OfficialAnthropicCli
            | ProviderKind::OfficialAnthropicDesktop => {}
        }
    }

    let provider_ids = providers
        .iter()
        .map(|provider| provider.id.as_str())
        .collect::<HashSet<_>>();
    for model in &mut config.models {
        if !provider_ids.contains(model.provider_id.as_str()) {
            model.provider_id = "openai-official".into();
            model.upstream_model = None;
        }
        let provider = providers
            .iter()
            .find(|provider| provider.id == model.provider_id);
        let Some(provider) = provider else {
            continue;
        };
        let (enabled, default_level, supported_levels) =
            reasoning_defaults_for_protocol(&provider.protocol);
        model.timeout_ms = 0;
        model.retry_count = 0;
        if supported_levels.is_empty() {
            model.reasoning_enabled = false;
            model.default_reasoning_level = default_level;
            model.supported_reasoning_levels.clear();
            continue;
        }
        model.reasoning_enabled = enabled;
        model.default_reasoning_level = default_level;
        model.supported_reasoning_levels = supported_levels;
    }
    codex_alias::normalize_model_aliases(&mut config.models);
    let codex_setting_allowed = |model: &crate::types::ModelEntry| {
        if !model.enabled {
            return false;
        }
        if config.settings.codex_injection_mode != CodexInjectionMode::ThirdPartyApi {
            return true;
        }
        providers
            .iter()
            .find(|provider| provider.id == model.provider_id)
            .is_some_and(|provider| provider.kind != ProviderKind::OfficialOpenAi)
    };
    let codex_setting_model_ids = config
        .models
        .iter()
        .filter(|model| codex_setting_allowed(model))
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    let first_codex_setting_model = codex_setting_model_ids.first().cloned();
    let normalize_codex_setting = |selected: Option<String>| {
        selected
            .as_deref()
            .map(str::trim)
            .filter(|selected| {
                codex_setting_model_ids
                    .iter()
                    .any(|model| model == *selected)
            })
            .map(str::to_string)
            .or_else(|| first_codex_setting_model.clone())
    };
    config.settings.codex_default_model =
        normalize_codex_setting(config.settings.codex_default_model.clone());
    config.settings.fallback_model =
        normalize_codex_setting(config.settings.fallback_model.clone());
    config.providers = providers;
    config
}

pub fn validate_provider_urls(config: &AppConfig) -> Result<(), String> {
    for provider in &config.providers {
        if matches!(
            provider.kind,
            ProviderKind::OfficialAnthropicCli | ProviderKind::OfficialAnthropicDesktop
        ) {
            continue;
        }
        let url = url::Url::parse(&provider.base_url)
            .map_err(|_| format!("Provider '{}' has an invalid API address", provider.name))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!(
                "Provider '{}' API address must start with http:// or https://",
                provider.name
            ));
        }
    }
    Ok(())
}

pub fn validate_enabled_model_ids(config: &AppConfig) -> Result<(), String> {
    let mut enabled = HashMap::<String, String>::new();
    for model in config.models.iter().filter(|model| model.enabled) {
        let id = model.id.trim();
        if id.is_empty() {
            continue;
        }
        let label = model_conflict_label(model);
        if let Some(existing) = enabled.insert(id.to_string(), label.clone()) {
            return Err(format!(
                "Model ID '{id}' is already enabled by {existing}. Disable it before enabling {label}."
            ));
        }
    }
    Ok(())
}

fn model_conflict_label(model: &crate::types::ModelEntry) -> String {
    let name = model.display_name.trim();
    let name = if name.is_empty() {
        model.id.trim()
    } else {
        name
    };
    format!("'{name}' on provider '{}'", model.provider_id)
}

fn reset_config_preserving_settings(raw: &str) -> AppConfig {
    let mut config = default_config();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(settings) = value
            .get("settings")
            .and_then(|settings| serde_json::from_value::<Settings>(settings.clone()).ok())
        {
            config.settings = settings;
        }
    }
    config
}

fn official_providers() -> Vec<Provider> {
    default_config()
        .providers
        .into_iter()
        .filter(|provider| provider.kind != ProviderKind::Custom)
        .collect()
}

fn unique_provider_id(candidate: &str, used_ids: &mut HashSet<String>) -> String {
    let base = sanitize_provider_id(candidate);
    if !used_ids.contains(&base) {
        used_ids.insert(base.clone());
        return base;
    }

    for index in 2.. {
        let next = format!("{base}-{index}");
        if !used_ids.contains(&next) {
            used_ids.insert(next.clone());
            return next;
        }
    }
    unreachable!()
}

fn sanitize_provider_id(value: &str) -> String {
    let id = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if id.is_empty()
        || matches!(
            id.as_str(),
            "openai-official" | "anthropic-cli" | "anthropic-desktop"
        )
    {
        "custom-provider".into()
    } else {
        id
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_config, reset_config_preserving_settings, validate_bind_settings,
        validate_enabled_model_ids, validate_provider_urls, CURRENT_CONFIG_VERSION,
    };
    use crate::types::{
        default_config, CodexInjectionMode, Provider, ProviderKind, ProviderProtocol,
    };

    #[test]
    fn rejects_public_bind_without_allow_lan() {
        let mut config = default_config();
        config.settings.bind_host = "0.0.0.0".into();
        config.settings.allow_lan = false;
        assert!(validate_bind_settings(&config).is_err());
    }

    #[test]
    fn accepts_public_bind_with_allow_lan() {
        let mut config = default_config();
        config.settings.bind_host = "0.0.0.0".into();
        config.settings.allow_lan = true;
        assert!(validate_bind_settings(&config).is_ok());
    }

    #[test]
    fn locks_official_openai_base_url() {
        let mut config = default_config();
        let official = config
            .providers
            .iter_mut()
            .find(|provider| provider.kind == ProviderKind::OfficialOpenAi)
            .unwrap();
        official.base_url = "https://proxy.example/v1".into();
        official.key_ref = Some("provider:bad".into());

        let normalized = normalize_config(config);
        let official = normalized
            .providers
            .iter()
            .find(|provider| provider.kind == ProviderKind::OfficialOpenAi)
            .unwrap();
        assert_eq!(official.base_url, "https://api.openai.com/v1");
        assert_eq!(official.protocol, ProviderProtocol::OpenAiResponses);
        assert!(official.key_ref.is_none());
    }

    #[test]
    fn locks_official_anthropic_cli_to_local_credentials() {
        let mut config = default_config();
        let official = config
            .providers
            .iter_mut()
            .find(|provider| provider.kind == ProviderKind::OfficialAnthropicCli)
            .unwrap();
        official.base_url = "https://proxy.example/v1".into();
        official.key_ref = Some("provider:bad".into());

        let normalized = normalize_config(config);
        let official = normalized
            .providers
            .iter()
            .find(|provider| provider.kind == ProviderKind::OfficialAnthropicCli)
            .unwrap();
        assert_eq!(official.base_url, "local://claude-code");
        assert_eq!(official.protocol, ProviderProtocol::AnthropicMessages);
        assert!(official.key_ref.is_none());
    }

    #[test]
    fn locks_official_anthropic_desktop_to_local_credentials() {
        let mut config = default_config();
        let official = config
            .providers
            .iter_mut()
            .find(|provider| provider.kind == ProviderKind::OfficialAnthropicDesktop)
            .unwrap();
        official.base_url = "https://proxy.example/v1".into();
        official.key_ref = Some("provider:bad".into());

        let normalized = normalize_config(config);
        let official = normalized
            .providers
            .iter()
            .find(|provider| provider.kind == ProviderKind::OfficialAnthropicDesktop)
            .unwrap();
        assert_eq!(official.base_url, "local://claude-desktop");
        assert_eq!(official.protocol, ProviderProtocol::AnthropicMessages);
        assert!(official.key_ref.is_none());
    }

    #[test]
    fn validates_custom_provider_base_urls() {
        let mut config = default_config();
        config.providers.push(custom_provider(
            "third-party",
            ProviderProtocol::OpenAiChatCompletions,
            "https://proxy.example/v1",
            true,
        ));
        assert!(validate_provider_urls(&config).is_ok());
        config.providers.last_mut().unwrap().base_url = "notaurl".into();
        assert!(validate_provider_urls(&config).is_err());
    }

    #[test]
    fn rejects_duplicate_enabled_model_ids() {
        let mut config = default_config();
        let mut duplicate = config.models[0].clone();
        duplicate.id = " gpt-5.5 ".into();
        duplicate.display_name = "Second GPT-5.5".into();
        config.models.push(duplicate);
        let config = normalize_config(config);

        let error = validate_enabled_model_ids(&config).unwrap_err();

        assert!(error.contains("Model ID 'gpt-5.5' is already enabled"));
        assert!(error.contains("Second GPT-5.5"));
    }

    #[test]
    fn allows_duplicate_model_ids_when_only_one_is_enabled() {
        let mut config = default_config();
        let mut duplicate = config.models[0].clone();
        duplicate.display_name = "Second GPT-5.5".into();
        duplicate.enabled = false;
        config.models.push(duplicate);
        let config = normalize_config(config);

        assert!(validate_enabled_model_ids(&config).is_ok());
    }

    #[test]
    fn default_config_contains_only_official_providers() {
        let config = default_config();
        assert_eq!(config.providers.len(), 3);
        assert!(config
            .providers
            .iter()
            .all(|provider| provider.kind != ProviderKind::Custom));
        assert!(config.models.iter().all(|model| config
            .providers
            .iter()
            .any(|provider| provider.id == model.provider_id)));
    }

    #[test]
    fn restores_deleted_official_providers_and_repoints_models() {
        let mut config = default_config();
        config
            .providers
            .retain(|provider| provider.id != "openai-official");
        config.models[0].provider_id = "missing-provider".into();

        let normalized = normalize_config(config);

        assert_eq!(normalized.providers.len(), 3);
        assert!(normalized
            .providers
            .iter()
            .any(|provider| provider.id == "openai-official"));
        assert_eq!(normalized.models[0].provider_id, "openai-official");
    }

    #[test]
    fn keeps_custom_provider_protocol_and_normalizes_key_ref() {
        let mut config = default_config();
        config.providers.push(custom_provider(
            "My Provider!",
            ProviderProtocol::AnthropicMessages,
            "https://anthropic.example/v1",
            true,
        ));

        let normalized = normalize_config(config);
        let custom = normalized
            .providers
            .iter()
            .find(|provider| provider.kind == ProviderKind::Custom)
            .unwrap();

        assert_eq!(custom.id, "my-provider");
        assert_eq!(custom.protocol, ProviderProtocol::AnthropicMessages);
        assert_eq!(custom.key_ref.as_deref(), Some("provider:my-provider"));
    }

    #[test]
    fn keeps_user_added_official_account_providers() {
        let mut config = default_config();
        config.providers.push(Provider {
            id: "openai-account-main".into(),
            name: "Personal OpenAI".into(),
            kind: ProviderKind::OfficialOpenAiAccount,
            protocol: ProviderProtocol::AnthropicMessages,
            base_url: "https://bad.example/v1".into(),
            key_ref: None,
        });
        config.providers.push(Provider {
            id: "claude-account-main".into(),
            name: "Personal Claude".into(),
            kind: ProviderKind::OfficialAnthropicAccount,
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: "https://bad.example/v1".into(),
            key_ref: None,
        });

        let normalized = normalize_config(config);
        let openai = normalized
            .providers
            .iter()
            .find(|provider| provider.id == "openai-account-main")
            .unwrap();
        let claude = normalized
            .providers
            .iter()
            .find(|provider| provider.id == "claude-account-main")
            .unwrap();

        assert_eq!(openai.kind, ProviderKind::OfficialOpenAiAccount);
        assert_eq!(openai.protocol, ProviderProtocol::OpenAiResponses);
        assert_eq!(openai.base_url, "https://api.openai.com/v1");
        assert_eq!(
            openai.key_ref.as_deref(),
            Some("official-token:openai-account-main")
        );
        assert_eq!(claude.kind, ProviderKind::OfficialAnthropicAccount);
        assert_eq!(claude.protocol, ProviderProtocol::AnthropicMessages);
        assert_eq!(claude.base_url, "https://api.anthropic.com/v1");
        assert_eq!(
            claude.key_ref.as_deref(),
            Some("official-token:claude-account-main")
        );
    }

    #[test]
    fn same_base_url_custom_providers_keep_distinct_key_refs() {
        let mut config = default_config();
        config.providers.push(custom_provider(
            "same-url-a",
            ProviderProtocol::OpenAiResponses,
            "https://relay.example/v1",
            true,
        ));
        config.providers.push(custom_provider(
            "same-url-b",
            ProviderProtocol::OpenAiResponses,
            "https://relay.example/v1",
            true,
        ));

        let normalized = normalize_config(config);
        let first = normalized
            .providers
            .iter()
            .find(|provider| provider.id == "same-url-a")
            .unwrap();
        let second = normalized
            .providers
            .iter()
            .find(|provider| provider.id == "same-url-b")
            .unwrap();

        assert_eq!(first.base_url, second.base_url);
        assert_eq!(first.key_ref.as_deref(), Some("provider:same-url-a"));
        assert_eq!(second.key_ref.as_deref(), Some("provider:same-url-b"));
    }

    #[test]
    fn reset_preserves_server_settings_only() {
        let raw = r#"{
            "version": 4,
            "providers": [{"id":"arm-moe","kind":"open_ai_compatible"}],
            "models": [],
            "settings": {
                "bind_host": "127.0.0.1",
                "port": 8788,
                "allow_lan": false,
                "request_log_limit": 123
            }
        }"#;

        let config = reset_config_preserving_settings(raw);

        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert_eq!(config.providers.len(), 3);
        assert_eq!(config.settings.port, 8788);
        assert_eq!(config.settings.request_log_limit, 123);
    }

    #[test]
    fn normalizes_reasoning_defaults_by_provider_protocol() {
        let mut config = default_config();
        config.providers.push(custom_provider(
            "deepseek",
            ProviderProtocol::OpenAiChatCompletions,
            "https://deepseek.example/v1",
            true,
        ));
        let mut deepseek = config.models[0].clone();
        deepseek.id = "deepseek-v4-pro".into();
        deepseek.provider_id = "deepseek".into();
        deepseek.reasoning_enabled = false;
        deepseek.default_reasoning_level = "medium".into();
        deepseek.supported_reasoning_levels.clear();
        config.models.push(deepseek);

        let config = normalize_config(config);
        let gpt = config
            .models
            .iter()
            .find(|model| model.id == "gpt-5.5")
            .unwrap();
        let claude = config
            .models
            .iter()
            .find(|model| model.id == "claude-opus-4-8")
            .unwrap();
        let deepseek = config
            .models
            .iter()
            .find(|model| model.id == "deepseek-v4-pro")
            .unwrap();

        assert!(gpt.reasoning_enabled);
        assert_eq!(gpt.timeout_ms, 0);
        assert_eq!(gpt.retry_count, 0);
        assert_eq!(gpt.default_reasoning_level, "xhigh");
        assert_eq!(
            gpt.supported_reasoning_levels,
            ["low", "medium", "high", "xhigh"]
        );
        assert!(claude.reasoning_enabled);
        assert_eq!(claude.timeout_ms, 0);
        assert_eq!(claude.retry_count, 0);
        assert_eq!(claude.default_reasoning_level, "max");
        assert_eq!(
            claude.supported_reasoning_levels,
            ["low", "medium", "high", "xhigh", "max"]
        );
        assert!(deepseek.reasoning_enabled);
        assert_eq!(deepseek.default_reasoning_level, "xhigh");
        assert_eq!(
            deepseek.supported_reasoning_levels,
            ["low", "medium", "high", "xhigh"]
        );
    }

    #[test]
    fn normalizes_runtime_defaults_and_required_fallback() {
        let mut config = default_config();
        config.settings.fallback_model = None;
        config.models[0].timeout_ms = 30_000;
        config.models[0].retry_count = 3;
        config.models[0].reasoning_enabled = false;
        config.models[0].default_reasoning_level = "low".into();
        config.models[0].supported_reasoning_levels = vec!["low".into()];

        let normalized = normalize_config(config);
        let gpt = normalized
            .models
            .iter()
            .find(|model| model.id == "gpt-5.5")
            .unwrap();

        assert_eq!(
            normalized.settings.fallback_model.as_deref(),
            Some("gpt-5.5")
        );
        assert_eq!(
            normalized.settings.codex_default_model.as_deref(),
            Some("gpt-5.5")
        );
        assert_eq!(gpt.timeout_ms, 0);
        assert_eq!(gpt.retry_count, 0);
        assert!(gpt.reasoning_enabled);
        assert_eq!(gpt.default_reasoning_level, "xhigh");
        assert_eq!(
            gpt.supported_reasoning_levels,
            ["low", "medium", "high", "xhigh"]
        );
    }

    #[test]
    fn disabled_codex_default_and_fallback_switch_to_available_model() {
        let mut config = default_config();
        config.settings.codex_default_model = Some("gpt-5.5".into());
        config.settings.fallback_model = Some("gpt-5.5".into());
        config.models[0].enabled = false;

        let normalized = normalize_config(config);

        assert_eq!(
            normalized.settings.codex_default_model.as_deref(),
            Some("claude-opus-4-8")
        );
        assert_eq!(
            normalized.settings.fallback_model.as_deref(),
            Some("claude-opus-4-8")
        );
    }

    #[test]
    fn codex_default_and_fallback_empty_until_model_is_enabled_again() {
        let mut config = default_config();
        for model in &mut config.models {
            model.enabled = false;
        }

        let normalized = normalize_config(config);

        assert_eq!(normalized.settings.codex_default_model, None);
        assert_eq!(normalized.settings.fallback_model, None);

        let mut config = normalized;
        let model = config
            .models
            .iter_mut()
            .find(|model| model.id == "claude-sonnet-4-5")
            .unwrap();
        model.enabled = true;

        let normalized = normalize_config(config);

        assert_eq!(
            normalized.settings.codex_default_model.as_deref(),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            normalized.settings.fallback_model.as_deref(),
            Some("claude-sonnet-4-5")
        );
    }

    #[test]
    fn third_party_mode_codex_settings_skip_openai_official() {
        let mut config = default_config();
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        config.settings.codex_default_model = Some("gpt-5.5".into());
        config.settings.fallback_model = Some("gpt-5.5".into());

        let normalized = normalize_config(config);

        assert_eq!(
            normalized.settings.codex_default_model.as_deref(),
            Some("claude-opus-4-8")
        );
        assert_eq!(
            normalized.settings.fallback_model.as_deref(),
            Some("claude-opus-4-8")
        );
    }

    fn custom_provider(
        id: &str,
        protocol: ProviderProtocol,
        base_url: &str,
        uses_key: bool,
    ) -> Provider {
        Provider {
            id: id.into(),
            name: "Custom Provider".into(),
            kind: ProviderKind::Custom,
            protocol,
            base_url: base_url.into(),
            key_ref: uses_key.then(|| format!("provider:{id}")),
        }
    }
}
