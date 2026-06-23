use crate::codex_alias;
use crate::key_store::KeyStore;
use crate::provider_proxy;
use crate::request_log::RequestLog;
use crate::types::{
    default_config, reasoning_defaults_for_protocol, AppConfig, AppSnapshot,
    ClaudeContextPressureSample, CodexInjectionMode, KeyStatus, OfficialAccountQuota, Provider,
    ProviderKind, ProviderLocalUsage, ProviderUsageStatus, RequestLogPage, RequestRecord,
    ServerStatus, Settings, TokenStats, TokenUsage,
};
use crate::{catalog, claude_auth, lan_share, official_auth};
use serde_json::to_string_pretty;
use std::{
    collections::{HashMap, HashSet},
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::RwLock;

const CURRENT_CONFIG_VERSION: u32 = 14;
const DASHBOARD_RECENT_REQUESTS: usize = 6;

struct CodexApplicationPlan {
    config: AppConfig,
    models: Vec<catalog::CatalogModel>,
    allowed_model_ids: Option<HashSet<String>>,
    default_slug: Option<String>,
}

#[derive(Clone)]
pub struct AppStore {
    inner: Arc<AppStoreInner>,
}

struct AppStoreInner {
    config_path: PathBuf,
    config: RwLock<AppConfig>,
    log: Arc<RequestLog>,
    server_status: RwLock<ServerStatus>,
    codex_apply_error: RwLock<Option<String>>,
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
                codex_apply_error: RwLock::new(None),
                key_store: KeyStore::new(&app_data_dir),
            }),
        })
    }

    pub async fn config(&self) -> AppConfig {
        self.inner.config.read().await.clone()
    }

    pub async fn replace_config(&self, config: AppConfig) -> Result<(), String> {
        let previous = self.config().await;
        let mut config = config;
        self.extract_provider_proxy_url_credentials(&mut config)?;
        let mut config = normalize_config(config);
        validate_bind_settings(&config)?;
        validate_provider_urls(&config)?;
        validate_enabled_model_ids(&config)?;
        if config.settings.codex_injection_mode == CodexInjectionMode::LanShare
            && !lan_remote_configured(&config.settings)
        {
            config.settings.codex_default_model = None;
            config.settings.fallback_model = None;
        }
        self.delete_removed_provider_keys(&previous, &config)?;
        self.write_config_file(&config)?;
        *self.inner.config.write().await = config.clone();
        Ok(())
    }

    fn extract_provider_proxy_url_credentials(&self, config: &mut AppConfig) -> Result<(), String> {
        for provider in &mut config.providers {
            let raw = provider.http_proxy.url.trim().to_string();
            if raw.is_empty() || !provider.http_proxy.enabled {
                continue;
            }
            let (clean_url, username, password) =
                provider_proxy::split_proxy_url_credentials(&raw)?;
            provider.http_proxy.url = clean_url;
            if provider.http_proxy.username.trim().is_empty() {
                if let Some(username) = username {
                    provider.http_proxy.username = username;
                }
            }
            if let Some(password) = password {
                let key_ref = provider_proxy::proxy_password_ref(&provider.id);
                self.inner.key_store.set_secret(&key_ref, &password)?;
                provider.http_proxy.password_ref = Some(key_ref);
            }
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
        let codex_apply_error = self.inner.codex_apply_error.read().await.clone();
        let provider_usage = merge_provider_usage(&config.providers, local_usage, usage_snapshots);
        AppSnapshot {
            config,
            keys,
            server,
            codex_apply_error,
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

    /// 写入「上下文体积」(清理前)并按上游模型重算 cost；流式结束时调。
    pub async fn finalize_request_breakdown(&self, id: String, context_usage: TokenUsage) {
        let log = self.inner.log.clone();
        let _ = tokio::task::spawn_blocking(move || {
            log.finalize_request_breakdown(&id, &context_usage)
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

    pub async fn upsert_claude_context_pressure(
        &self,
        provider_id: String,
        model: String,
        context_key: String,
        input_tokens: u64,
        body_bytes: u64,
    ) {
        let log = self.inner.log.clone();
        let _ = tokio::task::spawn_blocking(move || {
            log.upsert_claude_context_pressure(
                &provider_id,
                &model,
                &context_key,
                input_tokens,
                body_bytes,
            )
        })
        .await;
    }

    pub async fn claude_context_pressure(
        &self,
        provider_id: String,
        model: String,
        context_key: String,
    ) -> Option<ClaudeContextPressureSample> {
        let log = self.inner.log.clone();
        tokio::task::spawn_blocking(move || {
            log.claude_context_pressure(&provider_id, &model, &context_key)
        })
        .await
        .ok()
        .flatten()
    }

    pub async fn upsert_claude_compaction(
        &self,
        provider_id: String,
        model: String,
        context_key: String,
        summary: String,
    ) {
        let log = self.inner.log.clone();
        let _ = tokio::task::spawn_blocking(move || {
            log.upsert_claude_compaction(&provider_id, &model, &context_key, &summary)
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

    pub async fn apply_auto_codex_config_if_enabled(&self) {
        let config = self.config().await;
        if !config.settings.auto_inject {
            *self.inner.codex_apply_error.write().await = None;
            return;
        }
        let default_model = config
            .settings
            .codex_default_model
            .clone()
            .filter(|value| !value.trim().is_empty());
        match self
            .inject_codex_config_for(&config, default_model.as_deref())
            .await
        {
            Ok(_) => {
                *self.inner.codex_apply_error.write().await = None;
            }
            Err(error) => {
                *self.inner.codex_apply_error.write().await =
                    Some(enhance_codex_apply_error(error));
            }
        }
    }

    pub async fn clear_codex_apply_error(&self) {
        *self.inner.codex_apply_error.write().await = None;
    }

    pub async fn set_codex_apply_error(&self, error: String) {
        *self.inner.codex_apply_error.write().await = Some(enhance_codex_apply_error(error));
    }

    pub async fn export_catalog_to(&self, codex_home: &Path) -> Result<PathBuf, String> {
        let config = self.config().await;
        let plan = self.prepare_codex_application(&config, None).await?;
        self.persist_prepared_config(&plan.config).await?;
        catalog::write_catalog_models(codex_home, &plan.models)
    }

    pub async fn inject_codex_config_for(
        &self,
        config: &AppConfig,
        default_model: Option<&str>,
    ) -> Result<crate::codex_config::InjectionResult, String> {
        let plan = self
            .prepare_codex_application(config, default_model)
            .await?;
        self.persist_prepared_config(&plan.config).await?;
        self.check_local_codex_route(plan.default_slug.as_deref())
            .await?;
        let result = if plan.config.settings.codex_injection_mode == CodexInjectionMode::LanShare {
            crate::codex_config::inject_lan_share_config(
                &plan.config.settings,
                default_model,
                &plan.models,
            )
        } else {
            crate::codex_config::inject_with_model_filter(
                &plan.config,
                default_model,
                plan.allowed_model_ids.as_ref(),
            )
        };
        result.map_err(enhance_codex_apply_error)
    }

    pub async fn lan_catalog_models(&self) -> Result<Vec<catalog::CatalogModel>, String> {
        let config = self.config().await;
        self.lan_catalog_models_for_config(&config).await
    }

    pub async fn lan_codex_catalog_models(&self) -> Result<Vec<catalog::CatalogModel>, String> {
        let config = self.config().await;
        self.lan_codex_catalog_models_for_config(&config).await
    }

    pub async fn resolve_lan_codex_model(
        &self,
        requested_model: &str,
    ) -> Result<catalog::CatalogModel, String> {
        let requested_model = requested_model.trim();
        if requested_model.is_empty() {
            return Err("Missing LAN model".into());
        }
        let models = self.lan_codex_catalog_models().await?;
        models
            .into_iter()
            .find(|model| {
                model.slug == requested_model || model.real_target_model_id() == requested_model
            })
            .ok_or_else(|| {
                format!(
                    "LAN Codex slot '{requested_model}' is not mapped to a remote model; refresh LAN models and re-apply the Codex config."
                )
            })
    }

    pub async fn regenerate_lan_api_key(&self) -> Result<(), String> {
        let mut config = self.config().await;
        config.settings.lan_api_key = lan_share::generate_api_key();
        let config = normalize_config(config);
        validate_bind_settings(&config)?;
        validate_provider_urls(&config)?;
        validate_enabled_model_ids(&config)?;
        self.write_config_file(&config)?;
        *self.inner.config.write().await = config;
        Ok(())
    }

    async fn lan_catalog_models_for_config(
        &self,
        config: &AppConfig,
    ) -> Result<Vec<catalog::CatalogModel>, String> {
        let client = reqwest::Client::new();
        lan_share::fetch_remote_catalog_models(&client, &config.settings).await
    }

    async fn lan_codex_catalog_models_for_config(
        &self,
        config: &AppConfig,
    ) -> Result<Vec<catalog::CatalogModel>, String> {
        let remote_models = self.lan_catalog_models_for_config(config).await?;
        let (models, assignments) =
            catalog::lan_slot_catalog_models(&config.settings, &remote_models)?;
        self.remember_codex_slots(CodexInjectionMode::LanShare, assignments)
            .await?;
        Ok(models)
    }

    async fn remember_codex_slots(
        &self,
        mode: CodexInjectionMode,
        assignments: Vec<crate::types::CodexSlotAssignment>,
    ) -> Result<(), String> {
        let mut current = self.inner.config.write().await;
        codex_alias::replace_mode_assignments(&mut current.settings.codex_slots, mode, assignments);
        self.write_config_file(&current)
    }

    async fn prepare_codex_application(
        &self,
        config: &AppConfig,
        default_model: Option<&str>,
    ) -> Result<CodexApplicationPlan, String> {
        let mut config = normalize_config(config.clone());
        validate_bind_settings(&config)?;
        validate_provider_urls(&config)?;
        validate_enabled_model_ids(&config)?;

        if config.settings.codex_injection_mode == CodexInjectionMode::LanShare {
            let remote_models = self.lan_catalog_models_for_config(&config).await?;
            let (models, assignments) =
                catalog::lan_slot_catalog_models(&config.settings, &remote_models)?;
            codex_alias::replace_mode_assignments(
                &mut config.settings.codex_slots,
                CodexInjectionMode::LanShare,
                assignments,
            );
            let default_slug = selected_catalog_slug(default_model, &models);
            return Ok(CodexApplicationPlan {
                config,
                models,
                allowed_model_ids: None,
                default_slug,
            });
        }

        let allowed_model_ids = self.codex_allowed_model_ids(&config)?;
        let models = catalog::catalog_models_for_config(&config, allowed_model_ids.as_ref())?;
        if config.settings.codex_injection_mode == CodexInjectionMode::ThirdPartyApi
            && models.is_empty()
        {
            return Err("No third-party API models are available for Codex application".into());
        }
        let selected_model = default_model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or(config.settings.codex_default_model.as_deref());
        let default_slug = selected_model
            .map(|model| catalog::export_slug_for_model(&config, allowed_model_ids.as_ref(), model))
            .transpose()?;
        Ok(CodexApplicationPlan {
            config,
            models,
            allowed_model_ids,
            default_slug,
        })
    }

    async fn persist_prepared_config(&self, config: &AppConfig) -> Result<(), String> {
        self.write_config_file(config)?;
        *self.inner.config.write().await = config.clone();
        Ok(())
    }

    async fn check_local_codex_route(&self, expected_model: Option<&str>) -> Result<(), String> {
        let config = self.config().await;
        let base_url = crate::codex_config::local_codex_base_url(&config.settings);
        let service_url = base_url.trim_end_matches("/v1");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .map_err(|error| format!("Could not prepare local route check: {error}"))?;

        let health_url = format!("{service_url}/health");
        let health = client.get(&health_url).send().await.map_err(|error| {
            format!("Neko Route local service is not reachable at {health_url}: {error}")
        })?;
        let status = health.status();
        let body = health
            .bytes()
            .await
            .map_err(|error| format!("Could not read local health response: {error}"))?;
        if !status.is_success() {
            return Err(format!(
                "Neko Route local service returned {} for /health",
                status.as_u16()
            ));
        }
        let health_value = serde_json::from_slice::<serde_json::Value>(&body)
            .map_err(|error| format!("Invalid Neko Route /health response: {error}"))?;
        if health_value
            .get("service")
            .and_then(serde_json::Value::as_str)
            != Some("neko-route")
        {
            return Err(
                "Local route check did not reach Neko Route; verify the configured local port."
                    .into(),
            );
        }
        let running_config_version = health_value
            .get("config_version")
            .and_then(serde_json::Value::as_u64);
        if health_value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .is_none()
            || running_config_version.is_none()
            || health_value.get("codex_slot_count").is_none()
        {
            return Err("Neko Route local service looks like an older build. Restart Neko Route with the current build and apply the Codex config again.".into());
        }
        if running_config_version.unwrap_or_default() < u64::from(CURRENT_CONFIG_VERSION) {
            return Err(format!(
                "Neko Route local service is using config version {}, but this build requires version {}. Restart Neko Route and apply the Codex config again.",
                running_config_version.unwrap_or_default(),
                CURRENT_CONFIG_VERSION
            ));
        }

        let models_url = format!("{base_url}/models");
        let models = client.get(&models_url).send().await.map_err(|error| {
            format!("Neko Route local models endpoint is not reachable at {models_url}: {error}")
        })?;
        let status = models.status();
        let body = models
            .bytes()
            .await
            .map_err(|error| format!("Could not read local models response: {error}"))?;
        if !status.is_success() {
            return Err(format!(
                "Neko Route local models endpoint returned {}: {}",
                status.as_u16(),
                String::from_utf8_lossy(&body)
            ));
        }
        if let Some(expected_model) = expected_model {
            let value = serde_json::from_slice::<serde_json::Value>(&body)
                .map_err(|error| format!("Invalid local models response: {error}"))?;
            let found = value
                .get("data")
                .and_then(serde_json::Value::as_array)
                .map(|models| {
                    models.iter().any(|model| {
                        model
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|id| id == expected_model)
                    })
                })
                .unwrap_or(false);
            if !found {
                return Err(format!(
                    "Neko Route local models endpoint does not expose Codex model '{expected_model}'"
                ));
            }
        }
        Ok(())
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
            ProviderKind::OfficialAnthropicCli => {
                let (present, available, _) = claude_auth::cli_status();
                Ok(present && available)
            }
            ProviderKind::OfficialAnthropicDesktop => {
                let (present, available, _) = claude_auth::desktop_status();
                Ok(present && available)
            }
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
            .flat_map(|provider| {
                [
                    provider.key_ref.as_deref(),
                    provider.http_proxy.password_ref.as_deref(),
                ]
                .into_iter()
                .flatten()
            })
            .collect::<HashSet<_>>();
        for key_ref in previous.providers.iter().flat_map(|provider| {
            [
                provider.key_ref.as_deref(),
                provider.http_proxy.password_ref.as_deref(),
            ]
            .into_iter()
            .flatten()
        }) {
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

fn selected_catalog_slug(
    default_model: Option<&str>,
    models: &[catalog::CatalogModel],
) -> Option<String> {
    let requested = default_model
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match requested {
        Some(model) => models
            .iter()
            .find(|entry| entry.slug == model || entry.real_target_model_id() == model)
            .map(|entry| entry.slug.clone()),
        None => models.first().map(|entry| entry.slug.clone()),
    }
}

fn enhance_codex_apply_error(error: String) -> String {
    if error.contains("needs a Codex-compatible menu alias") {
        format!(
            "{error}\n\nThis is the old Codex alias path. Restart Neko Route with the current build and apply the Codex config again."
        )
    } else {
        error
    }
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
    if config.settings.lan_api_key.trim().is_empty() {
        config.settings.lan_api_key = lan_share::generate_api_key();
    } else {
        config.settings.lan_api_key = config.settings.lan_api_key.trim().to_string();
    }
    config.settings.lan_remote_host = config.settings.lan_remote_host.trim().to_string();
    config.settings.lan_remote_api_key = config.settings.lan_remote_api_key.trim().to_string();

    let mut official = official_providers();
    let official_ids = official
        .iter()
        .map(|provider| provider.id.clone())
        .collect::<HashSet<_>>();
    let official_proxy_by_id = config
        .providers
        .iter()
        .filter(|provider| official_ids.contains(provider.id.as_str()))
        .map(|provider| (provider.id.clone(), provider.http_proxy.clone()))
        .collect::<HashMap<_, _>>();
    for provider in &mut official {
        if let Some(proxy) = official_proxy_by_id.get(&provider.id) {
            provider.http_proxy =
                provider_proxy::normalize_provider_http_proxy(&provider.id, proxy.clone());
        }
    }
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
                provider.http_proxy = provider_proxy::normalize_provider_http_proxy(
                    &provider.id,
                    provider.http_proxy,
                );
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
                provider.http_proxy = provider_proxy::normalize_provider_http_proxy(
                    &provider.id,
                    provider.http_proxy,
                );
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
                provider.http_proxy = provider_proxy::normalize_provider_http_proxy(
                    &provider.id,
                    provider.http_proxy,
                );
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
    if config.settings.codex_injection_mode == CodexInjectionMode::LanShare {
        config.settings.codex_default_model = config
            .settings
            .codex_default_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        config.settings.fallback_model = config
            .settings
            .fallback_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        config.providers = providers;
        return config;
    }
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
    codex_alias::normalize_local_codex_slots(&mut config);
    config
}

fn lan_remote_configured(settings: &Settings) -> bool {
    !settings.lan_remote_host.trim().is_empty()
        && settings.lan_remote_port > 0
        && !settings.lan_remote_api_key.trim().is_empty()
}

pub fn validate_provider_urls(config: &AppConfig) -> Result<(), String> {
    for provider in &config.providers {
        provider_proxy::validate_provider_http_proxy(provider)?;
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
        validate_enabled_model_ids, validate_provider_urls, AppStore, CURRENT_CONFIG_VERSION,
    };
    use crate::types::{
        default_config, CodexInjectionMode, Provider, ProviderHttpProxy, ProviderKind,
        ProviderProtocol,
    };
    use serde_json::json;
    use std::{
        fs,
        net::TcpListener,
        time::{Duration, Instant},
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
    fn normalize_config_generates_lan_api_key() {
        let mut config = default_config();
        config.settings.lan_api_key = "   ".into();

        let normalized = normalize_config(config);

        assert!(normalized.settings.lan_api_key.starts_with("nr_"));
    }

    #[test]
    fn lan_share_mode_does_not_select_local_models() {
        let mut config = default_config();
        config.settings.codex_injection_mode = CodexInjectionMode::LanShare;
        config.settings.codex_default_model = None;
        config.settings.fallback_model = None;

        let normalized = normalize_config(config);

        assert!(normalized.settings.codex_default_model.is_none());
        assert!(normalized.settings.fallback_model.is_none());
    }

    #[tokio::test]
    async fn lan_share_save_does_not_fetch_remote_models() {
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = store.config().await;
        config.settings.codex_injection_mode = CodexInjectionMode::LanShare;
        config.settings.lan_remote_host = "192.0.2.1".into();
        config.settings.lan_remote_port = 8787;
        config.settings.lan_remote_api_key = "remote-key".into();
        config.settings.codex_default_model = Some("remote-default".into());
        config.settings.fallback_model = Some("remote-fallback".into());

        let started = Instant::now();
        store.replace_config(config).await.unwrap();

        assert!(started.elapsed() < Duration::from_secs(1));
        let saved = store.config().await;
        assert_eq!(
            saved.settings.codex_default_model.as_deref(),
            Some("remote-default")
        );
        assert_eq!(
            saved.settings.fallback_model.as_deref(),
            Some("remote-fallback")
        );
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
    fn missing_proxy_config_defaults_to_disabled_and_version_13() {
        let config = normalize_config(default_config());

        assert_eq!(config.version, CURRENT_CONFIG_VERSION);
        assert_eq!(CURRENT_CONFIG_VERSION, 14);
        assert!(config.providers.iter().all(|provider| {
            !provider.http_proxy.enabled
                && provider.http_proxy.url.is_empty()
                && provider.http_proxy.password_ref.is_none()
        }));
    }

    #[test]
    fn preserves_builtin_official_proxy_settings() {
        let mut config = default_config();
        let openai = config
            .providers
            .iter_mut()
            .find(|provider| provider.id == "openai-official")
            .unwrap();
        openai.http_proxy = ProviderHttpProxy {
            enabled: true,
            url: "127.0.0.1:7890".into(),
            username: "proxy-user".into(),
            password_ref: Some("provider-proxy:openai-official".into()),
        };

        let normalized = normalize_config(config);
        let openai = normalized
            .providers
            .iter()
            .find(|provider| provider.id == "openai-official")
            .unwrap();

        assert!(openai.http_proxy.enabled);
        assert_eq!(openai.http_proxy.url, "http://127.0.0.1:7890");
        assert_eq!(openai.http_proxy.username, "proxy-user");
        assert_eq!(
            openai.http_proxy.password_ref.as_deref(),
            Some("provider-proxy:openai-official")
        );
    }

    #[test]
    fn validates_http_proxy_address() {
        let mut config = default_config();
        config.providers[0].http_proxy = ProviderHttpProxy {
            enabled: true,
            url: "127.0.0.1:7890".into(),
            username: String::new(),
            password_ref: None,
        };
        let config = normalize_config(config);
        assert!(validate_provider_urls(&config).is_ok());

        let mut config = default_config();
        config.providers[0].http_proxy = ProviderHttpProxy {
            enabled: true,
            url: "socks5://127.0.0.1:7890".into(),
            username: String::new(),
            password_ref: None,
        };
        let config = normalize_config(config);
        let error = validate_provider_urls(&config).unwrap_err();
        assert!(error.contains("http:// or https://"));
    }

    #[tokio::test]
    async fn proxy_url_credentials_are_saved_as_secret_not_config() {
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = store.config().await;
        let provider = config
            .providers
            .iter_mut()
            .find(|provider| provider.id == "openai-official")
            .unwrap();
        provider.http_proxy = ProviderHttpProxy {
            enabled: true,
            url: "http://proxy-user:proxy-pass@127.0.0.1:7890".into(),
            username: String::new(),
            password_ref: None,
        };

        store.replace_config(config).await.unwrap();
        let config = store.config().await;
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == "openai-official")
            .unwrap();

        assert_eq!(provider.http_proxy.url, "http://127.0.0.1:7890");
        assert_eq!(provider.http_proxy.username, "proxy-user");
        assert_eq!(
            provider.http_proxy.password_ref.as_deref(),
            Some("provider-proxy:openai-official")
        );
        assert_eq!(
            store
                .key_store()
                .get_secret("provider-proxy:openai-official")
                .unwrap()
                .as_deref(),
            Some("proxy-pass")
        );
        let raw = fs::read_to_string(temp.path().join("config.json")).unwrap();
        assert!(!raw.contains("proxy-pass"));
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
            http_proxy: Default::default(),
        });
        config.providers.push(Provider {
            id: "claude-account-main".into(),
            name: "Personal Claude".into(),
            kind: ProviderKind::OfficialAnthropicAccount,
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: "https://bad.example/v1".into(),
            key_ref: None,
            http_proxy: Default::default(),
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
        assert!(config.settings.lan_api_key.starts_with("nr_"));
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

    #[test]
    fn old_codex_alias_config_migrates_to_codex_slots() {
        let mut config = config_with_gpt_54_pro();
        config.version = 11;
        config.settings.codex_slots.clear();
        config
            .models
            .iter_mut()
            .find(|model| model.id == "gpt-5.4-pro")
            .unwrap()
            .codex_alias = Some("gpt-5.4-mini".into());

        let normalized = normalize_config(config);
        let assignment = normalized
            .settings
            .codex_slots
            .iter()
            .find(|assignment| assignment.target_model_id == "gpt-5.4-pro")
            .unwrap();

        assert_eq!(normalized.version, CURRENT_CONFIG_VERSION);
        assert_eq!(assignment.slot, "gpt-5.4-mini");
        assert!(normalized
            .models
            .iter()
            .all(|model| model.codex_alias.is_none()));
    }

    #[tokio::test]
    async fn third_party_gpt_54_pro_gets_codex_slot_without_alias_error() {
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let config = config_with_gpt_54_pro();

        let plan = store
            .prepare_codex_application(&config, Some("gpt-5.4-pro"))
            .await
            .unwrap();

        assert_eq!(plan.default_slug.as_deref(), Some("gpt-5.5"));
        assert!(plan.models.iter().any(|model| {
            model.slug == "gpt-5.5" && model.real_target_model_id() == "gpt-5.4-pro"
        }));
    }

    #[tokio::test]
    async fn catalog_export_and_application_plan_use_same_slot_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        store
            .replace_config(config_with_gpt_54_pro())
            .await
            .unwrap();
        let config = store.config().await;
        let plan = store
            .prepare_codex_application(&config, Some("gpt-5.4-pro"))
            .await
            .unwrap();

        let path = store.export_catalog_to(temp.path()).await.unwrap();
        let catalog = fs::read_to_string(path).unwrap();

        for model in plan.models {
            assert!(catalog.contains(&format!("\"slug\": \"{}\"", model.slug)));
        }
        assert!(catalog.contains("\"slug\": \"gpt-5.5\""));
        assert!(!catalog.contains("\"slug\": \"gpt-5.4-pro\""));
    }

    #[tokio::test]
    async fn auto_apply_failure_is_visible_in_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = config_with_gpt_54_pro();
        config.settings.port = unused_loopback_port();
        config.settings.auto_inject = true;
        store.replace_config(config).await.unwrap();

        store.apply_auto_codex_config_if_enabled().await;
        let snapshot = store.snapshot().await;

        assert!(snapshot
            .codex_apply_error
            .as_deref()
            .unwrap_or_default()
            .contains("Neko Route local service is not reachable"));
    }

    #[tokio::test]
    async fn local_route_check_reports_unreachable_service() {
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = store.config().await;
        config.settings.port = unused_loopback_port();
        store.replace_config(config).await.unwrap();

        let error = store
            .check_local_codex_route(Some("gpt-5.5"))
            .await
            .unwrap_err();

        assert!(error.contains("Neko Route local service is not reachable"));
    }

    #[tokio::test]
    async fn local_route_check_reports_old_runtime_health() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new()
            .route(
                "/health",
                axum::routing::get(|| async {
                    axum::Json(json!({
                        "ok": true,
                        "service": "neko-route"
                    }))
                }),
            )
            .route(
                "/v1/models",
                axum::routing::get(|| async {
                    axum::Json(json!({
                        "object": "list",
                        "data": [{"id": "gpt-5.5"}]
                    }))
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = store.config().await;
        config.settings.port = port;
        store.replace_config(config).await.unwrap();

        let error = store
            .check_local_codex_route(Some("gpt-5.5"))
            .await
            .unwrap_err();

        assert!(error.contains("older build"));
        server.abort();
    }

    fn unused_loopback_port() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    fn config_with_gpt_54_pro() -> crate::types::AppConfig {
        let mut config = default_config();
        config.providers.push(custom_provider(
            "custom-chat",
            ProviderProtocol::OpenAiChatCompletions,
            "https://proxy.example/v1",
            false,
        ));
        let mut model = config.models[0].clone();
        model.id = "gpt-5.4-pro".into();
        model.display_name = "GPT-5.4 Pro".into();
        model.provider_id = "custom-chat".into();
        model.upstream_model = Some("real-gpt-5.4-pro".into());
        model.enabled = true;
        config.models.push(model);
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        config.settings.codex_default_model = Some("gpt-5.4-pro".into());
        config.settings.fallback_model = Some("gpt-5.4-pro".into());
        normalize_config(config)
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
            http_proxy: Default::default(),
        }
    }
}
