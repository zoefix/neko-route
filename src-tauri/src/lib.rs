mod catalog;
mod claude_auth;
mod codex_alias;
mod codex_config;
mod key_store;
mod lan_share;
mod official_auth;
mod official_usage;
mod pricing;
mod provider_proxy;
mod redact;
mod request_log;
mod router;
mod server;
mod session_import;
mod store;
mod types;
mod usage;

use crate::{
    router::{match_route, match_route_for_provider, RouteMatch},
    store::AppStore,
    types::{
        AppConfig, AppSnapshot, ModelHealth, Provider, ProviderKind, ProviderProtocol,
        RequestLogPage, Settings, TokenUsage,
    },
    usage::parse_usage,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(any(test, target_os = "windows"))]
use std::path::PathBuf;
use std::{
    collections::HashMap,
    fs,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
#[cfg(target_os = "windows")]
use std::{env, ffi::OsStr, path::Path};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

#[derive(Clone)]
struct ServerRuntime {
    inner: Arc<ServerRuntimeInner>,
}

struct ServerRuntimeInner {
    store: AppStore,
    task: Mutex<Option<ServerTask>>,
}

struct ServerTask {
    shutdown: Option<oneshot::Sender<()>>,
    handle: tauri::async_runtime::JoinHandle<()>,
}

impl ServerRuntime {
    fn new(store: AppStore) -> Self {
        Self {
            inner: Arc::new(ServerRuntimeInner {
                store,
                task: Mutex::new(None),
            }),
        }
    }

    async fn start(&self) {
        let mut task = self.inner.task.lock().await;
        if task.is_some() {
            return;
        }
        *task = Some(spawn_managed_server(self.inner.store.clone()));
    }

    async fn restart(&self) {
        self.stop().await;
        self.start().await;
    }

    async fn stop(&self) {
        let task = {
            let mut guard = self.inner.task.lock().await;
            guard.take()
        };
        let Some(mut task) = task else {
            return;
        };
        if let Some(shutdown) = task.shutdown.take() {
            let _ = shutdown.send(());
        }
        if tokio::time::timeout(Duration::from_secs(2), &mut task.handle)
            .await
            .is_err()
        {
            task.handle.abort();
            let _ = task.handle.await;
        }
    }
}

fn spawn_managed_server(store: AppStore) -> ServerTask {
    let (shutdown, shutdown_rx) = oneshot::channel::<()>();
    let store_for_task = store.clone();
    let handle = tauri::async_runtime::spawn(async move {
        if let Err(error) = server::run_with_shutdown(store_for_task.clone(), async move {
            let _ = shutdown_rx.await;
        })
        .await
        {
            let config = store_for_task.config().await;
            store_for_task
                .set_server_error(server_bind_url(&config.settings), error)
                .await;
        }
    });

    ServerTask {
        shutdown: Some(shutdown),
        handle,
    }
}

fn server_bind_url(settings: &Settings) -> String {
    format!("http://{}:{}/v1", settings.bind_host, settings.port)
}

fn server_bind_settings_changed(previous: &Settings, next: &Settings) -> bool {
    previous.bind_host != next.bind_host || previous.port != next.port
}

#[tauri::command]
async fn get_snapshot(store: tauri::State<'_, AppStore>) -> Result<AppSnapshot, String> {
    Ok(store.snapshot().await)
}

/// 健康页：每个模型从 DB 取最近 60 条请求的健康格子（不受 snapshot 小窗口限制）。
#[tauri::command]
async fn model_health(
    store: tauri::State<'_, AppStore>,
    models: Vec<String>,
) -> Result<Vec<ModelHealth>, String> {
    Ok(store.model_health(models, 60).await)
}

#[tauri::command]
async fn save_config(
    store: tauri::State<'_, AppStore>,
    server_runtime: tauri::State<'_, ServerRuntime>,
    config: AppConfig,
) -> Result<AppSnapshot, String> {
    let previous = store.config().await;
    store.replace_config(config).await?;
    let next = store.config().await;
    if server_bind_settings_changed(&previous.settings, &next.settings) {
        server_runtime.restart().await;
    }
    store.apply_auto_codex_config_if_enabled().await;
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn regenerate_lan_api_key(store: tauri::State<'_, AppStore>) -> Result<AppSnapshot, String> {
    store.regenerate_lan_api_key().await?;
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn list_lan_models(store: tauri::State<'_, AppStore>) -> Result<LanModelList, String> {
    let models = store
        .lan_catalog_models()
        .await?
        .into_iter()
        .map(|model| LanModelInfo {
            id: model.slug,
            display_name: model.display_name,
            description: model.description,
            context_window: model.context_window,
        })
        .collect();
    Ok(LanModelList { models })
}

fn default_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("NekoRoute/0.1")
        .build()
        .map_err(|error| error.to_string())
}

fn client_for_provider(
    store: &AppStore,
    default_client: &reqwest::Client,
    provider: &Provider,
) -> Result<reqwest::Client, String> {
    provider_proxy::client_for_provider(default_client, store.key_store(), provider)
}

#[tauri::command]
async fn set_provider_key(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
    secret: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    let key_ref = provider
        .key_ref
        .as_deref()
        .ok_or_else(|| "This provider does not use a stored API key".to_string())?;
    store.key_store().set_secret(key_ref, &secret)?;
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn delete_provider_key(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    let key_ref = provider
        .key_ref
        .as_deref()
        .ok_or_else(|| "This provider does not use a stored API key".to_string())?;
    store.key_store().delete_secret(key_ref)?;
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn read_provider_proxy_password(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
) -> Result<String, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    let Some(key_ref) = provider.http_proxy.password_ref.as_deref() else {
        return Ok(String::new());
    };
    store
        .key_store()
        .get_secret(key_ref)
        .map(|value| value.unwrap_or_default())
}

#[tauri::command]
async fn set_provider_proxy_password(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
    password: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    let key_ref = provider
        .http_proxy
        .password_ref
        .clone()
        .unwrap_or_else(|| provider_proxy::proxy_password_ref(&provider.id));
    if password.is_empty() {
        store.key_store().delete_secret(&key_ref)?;
    } else {
        store.key_store().set_secret(&key_ref, &password)?;
    }
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn delete_provider_proxy_password(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    let key_ref = provider
        .http_proxy
        .password_ref
        .clone()
        .unwrap_or_else(|| provider_proxy::proxy_password_ref(&provider.id));
    store.key_store().delete_secret(&key_ref)?;
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn set_official_provider_token(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
    token_json: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    if official_auth::account_kind(provider).is_none() {
        return Err("This provider does not use official account token JSON".into());
    }
    official_auth::set_provider_token(&provider.id, &token_json)?;
    if matches!(
        provider.kind,
        ProviderKind::OfficialOpenAiAccount | ProviderKind::OfficialAnthropicAccount
    ) {
        let _ = refresh_official_usage_for_provider(&store, provider).await;
    }
    Ok(store.snapshot().await)
}

#[derive(Clone, Default)]
struct OpenAiOAuthSessions {
    sessions: Arc<Mutex<HashMap<String, official_auth::OpenAiOAuthSession>>>,
}

#[derive(Clone, Default)]
struct ClaudeOAuthSessions {
    sessions: Arc<Mutex<HashMap<String, official_auth::ClaudeOAuthSession>>>,
}

#[derive(Serialize)]
struct OAuthStart {
    session_id: String,
    auth_url: String,
    expires_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct CodexAppStatus {
    running: bool,
}

#[derive(Serialize)]
struct CodexAppRestartResult {
    action: &'static str,
}

#[derive(Serialize)]
struct LanModelInfo {
    id: String,
    display_name: String,
    description: String,
    context_window: u64,
}

#[derive(Serialize)]
struct LanModelList {
    models: Vec<LanModelInfo>,
}

#[tauri::command]
async fn start_openai_oauth(
    sessions: tauri::State<'_, OpenAiOAuthSessions>,
) -> Result<OAuthStart, String> {
    let session = official_auth::start_openai_oauth_session();
    let result = OAuthStart {
        session_id: session.session_id.clone(),
        auth_url: session.auth_url.clone(),
        expires_at: session.expires_at,
    };
    sessions
        .sessions
        .lock()
        .await
        .insert(session.session_id.clone(), session);
    Ok(result)
}

#[tauri::command]
async fn finish_openai_oauth(
    store: tauri::State<'_, AppStore>,
    sessions: tauri::State<'_, OpenAiOAuthSessions>,
    provider_id: String,
    session_id: String,
    callback_or_code: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    if provider.kind != ProviderKind::OfficialOpenAiAccount {
        return Err("This provider does not use OpenAI OAuth".into());
    }
    let session = sessions
        .sessions
        .lock()
        .await
        .remove(&session_id)
        .ok_or_else(|| "OpenAI OAuth session not found or expired".to_string())?;
    if session.expires_at <= Utc::now() {
        return Err("OpenAI OAuth session expired. Generate a new authorization link.".into());
    }
    let code = official_auth::extract_openai_oauth_code(&callback_or_code, &session.state)?;
    let default_client = default_http_client()?;
    let client = client_for_provider(&store, &default_client, provider)?;
    let token =
        official_auth::exchange_openai_oauth_code(&client, &code, &session.code_verifier).await?;
    official_auth::set_provider_token_value(&provider.id, token)?;
    let _ = refresh_official_usage_for_provider(&store, provider).await;
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn start_claude_oauth(
    sessions: tauri::State<'_, ClaudeOAuthSessions>,
) -> Result<OAuthStart, String> {
    let session = official_auth::start_claude_oauth_session();
    let result = OAuthStart {
        session_id: session.session_id.clone(),
        auth_url: session.auth_url.clone(),
        expires_at: session.expires_at,
    };
    sessions
        .sessions
        .lock()
        .await
        .insert(session.session_id.clone(), session);
    Ok(result)
}

#[tauri::command]
async fn finish_claude_oauth(
    store: tauri::State<'_, AppStore>,
    sessions: tauri::State<'_, ClaudeOAuthSessions>,
    provider_id: String,
    session_id: String,
    callback_or_code: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    if provider.kind != ProviderKind::OfficialAnthropicAccount {
        return Err("This provider does not use Claude OAuth".into());
    }
    let session = sessions
        .sessions
        .lock()
        .await
        .remove(&session_id)
        .ok_or_else(|| "Claude OAuth session not found or expired".to_string())?;
    if session.expires_at <= Utc::now() {
        return Err("Claude OAuth session expired. Generate a new authorization link.".into());
    }
    let code = official_auth::extract_claude_oauth_code(&callback_or_code, &session.state)?;
    let default_client = default_http_client()?;
    let client = client_for_provider(&store, &default_client, provider)?;
    let token =
        official_auth::exchange_claude_oauth_code(&client, &code, &session.code_verifier).await?;
    official_auth::set_provider_token_value(&provider.id, token)?;
    let _ = refresh_official_usage_for_provider(&store, provider).await;
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn finish_claude_cookie_oauth(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
    session_key: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    if provider.kind != ProviderKind::OfficialAnthropicAccount {
        return Err("This provider does not use Claude OAuth".into());
    }
    let default_client = default_http_client()?;
    let client = client_for_provider(&store, &default_client, provider)?;
    let token = official_auth::exchange_claude_cookie_session_key(&client, &session_key).await?;
    official_auth::set_provider_token_value(&provider.id, token)?;
    let _ = refresh_official_usage_for_provider(&store, provider).await;
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn delete_official_provider_token(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    if official_auth::account_kind(provider).is_none() {
        return Err("This provider does not use official account token JSON".into());
    }
    official_auth::delete_provider_token(&provider.id)?;
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn refresh_official_provider_token(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    let default_client = default_http_client()?;
    let client = client_for_provider(&store, &default_client, provider)?;
    official_auth::refresh_provider_token(&client, provider).await?;
    if matches!(
        provider.kind,
        ProviderKind::OfficialOpenAiAccount | ProviderKind::OfficialAnthropicAccount
    ) {
        let _ = refresh_official_usage_for_provider(&store, provider).await;
    }
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn codex_app_status() -> Result<CodexAppStatus, String> {
    tokio::task::spawn_blocking(|| CodexAppStatus {
        running: codex_desktop_running(),
    })
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
async fn restart_codex_app() -> Result<CodexAppRestartResult, String> {
    tokio::task::spawn_blocking(|| {
        #[cfg(target_os = "windows")]
        {
            return restart_or_start_codex_desktop();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let running = codex_desktop_running();
            if running {
                stop_codex_desktop()?;
            }
            start_codex_desktop()?;
            Ok(CodexAppRestartResult {
                action: codex_restart_action(running),
            })
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn test_route(store: tauri::State<'_, AppStore>, model: String) -> Result<Value, String> {
    let config = store.config().await;
    let matched = match_route(&config, &model)?;
    Ok(json!({
        "model": model,
        "model_config": matched.model,
        "provider": matched.provider,
        "upstream_model": matched.upstream_model,
        "route_reason": matched.route_reason,
        "requested_model": matched.requested_model,
        "locked_from_model": matched.locked_from_model
    }))
}

#[derive(Serialize, Clone)]
struct TestModelResult {
    ok: bool,
    status: u16,
    latency_ms: u128,
    reply: String,
    error: Option<String>,
    usage: TokenUsage,
    provider_name: String,
    image_preview: Option<String>,
}

#[derive(Default, Clone)]
struct ModelTestSessions {
    sessions: Arc<Mutex<HashMap<String, ModelTestSession>>>,
}

struct ModelTestSession {
    status: Arc<Mutex<ModelTestStatus>>,
    cancelled: Arc<AtomicBool>,
    handle: tauri::async_runtime::JoinHandle<()>,
}

#[derive(Serialize)]
struct StartModelTestResult {
    test_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelTestMode {
    Connectivity,
    Image,
    Context400K,
    Context1M,
}

impl ModelTestMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "connectivity" => Ok(Self::Connectivity),
            "image" => Ok(Self::Image),
            "context_400k" => Ok(Self::Context400K),
            "context_1m" => Ok(Self::Context1M),
            _ => Err("Unknown model test mode".into()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Connectivity => "connectivity",
            Self::Image => "image",
            Self::Context400K => "context_400k",
            Self::Context1M => "context_1m",
        }
    }

    fn target_tokens(self) -> u64 {
        match self {
            Self::Connectivity => 0,
            Self::Image => 0,
            Self::Context400K => 400_000,
            Self::Context1M => 1_000_000,
        }
    }

    fn pass_threshold_tokens(self) -> u64 {
        self.target_tokens() * 95 / 100
    }

    fn probe_steps(self) -> &'static [u64] {
        match self {
            Self::Connectivity => &[],
            Self::Image => &[],
            Self::Context400K => &[128_000, 258_000, 380_000, 420_000],
            Self::Context1M => &[128_000, 258_000, 400_000, 700_000, 950_000, 1_050_000],
        }
    }

    fn target_label(self) -> &'static str {
        match self {
            Self::Connectivity => "connectivity",
            Self::Image => "image",
            Self::Context400K => "400K",
            Self::Context1M => "1M",
        }
    }
}

#[derive(Serialize, Clone)]
struct ModelTestStatus {
    test_id: String,
    mode: String,
    state: String,
    model: String,
    provider_name: String,
    stage: String,
    target_tokens: u64,
    pass_threshold_tokens: u64,
    current_tokens: u64,
    current_estimated: bool,
    confirmed_tokens: u64,
    confirmed_estimated: bool,
    last_status: u16,
    latency_ms: u128,
    last_error: Option<String>,
    summary: Option<String>,
    supported: Option<bool>,
    inconclusive: bool,
    result: Option<TestModelResult>,
}

#[derive(Serialize)]
struct ProviderCredential {
    value: String,
    source: String,
    editable: bool,
    deletable: bool,
}

#[tauri::command]
async fn read_provider_credential(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
) -> Result<ProviderCredential, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;

    match &provider.kind {
        ProviderKind::Custom => {
            let value = if let Some(key_ref) = provider.key_ref.as_deref() {
                store.key_store().get_secret(key_ref)?.unwrap_or_default()
            } else {
                String::new()
            };
            Ok(ProviderCredential {
                value,
                source: "Neko Route local storage".into(),
                editable: true,
                deletable: provider.key_ref.is_some(),
            })
        }
        ProviderKind::OfficialOpenAiAccount | ProviderKind::OfficialAnthropicAccount => {
            Ok(ProviderCredential {
                value: official_auth::provider_token_json(&provider.id)?.unwrap_or_default(),
                source: "Neko Route official token storage".into(),
                editable: true,
                deletable: true,
            })
        }
        ProviderKind::OfficialOpenAi => {
            let auth_path = codex_config::resolve_codex_home().join("auth.json");
            let value = if auth_path.exists() {
                pretty_json_or_raw(
                    &fs::read_to_string(&auth_path)
                        .map_err(|error| format!("Could not read Codex auth file: {error}"))?,
                )
            } else {
                String::new()
            };
            Ok(ProviderCredential {
                value,
                source: auth_path.display().to_string(),
                editable: false,
                deletable: false,
            })
        }
        ProviderKind::OfficialAnthropicCli => Ok(ProviderCredential {
            value: claude_auth::cli_credential_json()?,
            source: "Claude Code CLI credentials".into(),
            editable: false,
            deletable: false,
        }),
        ProviderKind::OfficialAnthropicDesktop => Ok(ProviderCredential {
            value: claude_auth::desktop_credential_json()?,
            source: "Claude Desktop token cache".into(),
            editable: false,
            deletable: false,
        }),
    }
}

#[tauri::command]
async fn test_model(
    store: tauri::State<'_, AppStore>,
    model: String,
    provider_id: Option<String>,
) -> Result<TestModelResult, String> {
    let config = store.config().await;
    let matched = provider_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|provider| match_route_for_provider(&config, &model, provider))
        .unwrap_or_else(|| match_route(&config, &model))?;

    test_matched_model(store.inner(), matched, ModelTestMode::Connectivity).await
}

#[tauri::command]
async fn start_model_test(
    store: tauri::State<'_, AppStore>,
    sessions: tauri::State<'_, ModelTestSessions>,
    model: String,
    provider_id: Option<String>,
    mode: String,
) -> Result<StartModelTestResult, String> {
    let mode = ModelTestMode::parse(&mode)?;
    let config = store.config().await;
    let matched = provider_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|provider| match_route_for_provider(&config, &model, provider))
        .unwrap_or_else(|| match_route(&config, &model))?;
    let test_id = Uuid::new_v4().to_string();
    let status = Arc::new(Mutex::new(ModelTestStatus {
        test_id: test_id.clone(),
        mode: mode.as_str().into(),
        state: "running".into(),
        model: model.clone(),
        provider_name: matched.provider.name.clone(),
        stage: "queued".into(),
        target_tokens: mode.target_tokens(),
        pass_threshold_tokens: mode.pass_threshold_tokens(),
        current_tokens: 0,
        current_estimated: true,
        confirmed_tokens: 0,
        confirmed_estimated: true,
        last_status: 0,
        latency_ms: 0,
        last_error: None,
        summary: None,
        supported: None,
        inconclusive: false,
        result: None,
    }));
    let cancelled = Arc::new(AtomicBool::new(false));
    let handle = tauri::async_runtime::spawn(run_model_test_task(
        store.inner().clone(),
        matched,
        mode,
        status.clone(),
        cancelled.clone(),
    ));
    sessions.sessions.lock().await.insert(
        test_id.clone(),
        ModelTestSession {
            status,
            cancelled,
            handle,
        },
    );

    Ok(StartModelTestResult { test_id })
}

#[tauri::command]
async fn get_model_test_status(
    sessions: tauri::State<'_, ModelTestSessions>,
    test_id: String,
) -> Result<ModelTestStatus, String> {
    let status = {
        let sessions = sessions.sessions.lock().await;
        sessions
            .get(&test_id)
            .map(|session| session.status.clone())
            .ok_or_else(|| "Model test not found".to_string())?
    };
    let snapshot = status.lock().await.clone();
    Ok(snapshot)
}

#[tauri::command]
async fn cancel_model_test(
    sessions: tauri::State<'_, ModelTestSessions>,
    test_id: String,
) -> Result<ModelTestStatus, String> {
    let session = sessions
        .sessions
        .lock()
        .await
        .remove(&test_id)
        .ok_or_else(|| "Model test not found".to_string())?;
    session.cancelled.store(true, Ordering::SeqCst);
    session.handle.abort();
    {
        let mut status = session.status.lock().await;
        status.state = "cancelled".into();
        status.stage = "cancelled".into();
        status.summary = Some("Test cancelled".into());
    }
    let snapshot = session.status.lock().await.clone();
    Ok(snapshot)
}

async fn run_model_test_task(
    store: AppStore,
    matched: RouteMatch,
    mode: ModelTestMode,
    status: Arc<Mutex<ModelTestStatus>>,
    cancelled: Arc<AtomicBool>,
) {
    if matches!(mode, ModelTestMode::Connectivity | ModelTestMode::Image) {
        run_connectivity_test_task(&store, matched, mode, &status).await;
        return;
    }
    run_context_pressure_test_task(&store, matched, mode, &status, &cancelled).await;
}

async fn update_model_test_status(
    status: &Arc<Mutex<ModelTestStatus>>,
    update: impl FnOnce(&mut ModelTestStatus),
) {
    let mut status = status.lock().await;
    update(&mut status);
}

async fn run_connectivity_test_task(
    store: &AppStore,
    matched: RouteMatch,
    mode: ModelTestMode,
    status: &Arc<Mutex<ModelTestStatus>>,
) {
    update_model_test_status(status, |status| {
        status.stage = "connectivity".into();
    })
    .await;

    match test_matched_model(store, matched, mode).await {
        Ok(result) => {
            update_model_test_status(status, |status| {
                status.state = "completed".into();
                status.stage = "done".into();
                status.last_status = result.status;
                status.latency_ms = result.latency_ms;
                status.last_error = result.error.clone();
                status.supported = Some(result.ok);
                status.inconclusive = false;
                status.summary = Some(if result.ok {
                    "Connectivity test passed".into()
                } else {
                    "Connectivity test failed".into()
                });
                status.result = Some(result);
            })
            .await;
        }
        Err(error) => {
            update_model_test_status(status, |status| {
                status.state = "completed".into();
                status.stage = "done".into();
                status.last_error = Some(error.clone());
                status.supported = None;
                status.inconclusive = true;
                status.summary = Some(format!("Connectivity test inconclusive: {error}"));
                status.result = Some(TestModelResult {
                    ok: false,
                    status: 0,
                    latency_ms: 0,
                    reply: String::new(),
                    error: Some(error),
                    usage: TokenUsage::default(),
                    provider_name: status.provider_name.clone(),
                    image_preview: None,
                });
            })
            .await;
        }
    }
}

async fn run_context_pressure_test_task(
    store: &AppStore,
    matched: RouteMatch,
    mode: ModelTestMode,
    status: &Arc<Mutex<ModelTestStatus>>,
    cancelled: &Arc<AtomicBool>,
) {
    update_model_test_status(status, |status| {
        status.stage = "connectivity".into();
    })
    .await;

    let connectivity =
        match test_matched_model(store, matched.clone(), ModelTestMode::Connectivity).await {
            Ok(result) => result,
            Err(error) => {
                update_model_test_status(status, |status| {
                    status.state = "completed".into();
                    status.stage = "done".into();
                    status.inconclusive = true;
                    status.supported = None;
                    status.last_error = Some(error.clone());
                    status.summary = Some(format!("Test inconclusive: {error}"));
                    status.result = Some(TestModelResult {
                        ok: false,
                        status: 0,
                        latency_ms: 0,
                        reply: String::new(),
                        error: Some(error),
                        usage: TokenUsage::default(),
                        provider_name: status.provider_name.clone(),
                        image_preview: None,
                    });
                })
                .await;
                return;
            }
        };

    if !connectivity.ok {
        update_model_test_status(status, |status| {
            status.state = "completed".into();
            status.stage = "done".into();
            status.last_status = connectivity.status;
            status.latency_ms = connectivity.latency_ms;
            status.last_error = connectivity.error.clone();
            status.inconclusive = true;
            status.supported = None;
            status.summary = Some("Test inconclusive: connectivity check failed".into());
            status.result = Some(connectivity);
        })
        .await;
        return;
    }

    update_model_test_status(status, |status| {
        status.result = Some(connectivity);
    })
    .await;

    let pass_threshold = mode.pass_threshold_tokens();
    let mut confirmed_tokens = 0_u64;
    let mut confirmed_estimated = true;

    for probe_tokens in mode.probe_steps() {
        if cancelled.load(Ordering::SeqCst) {
            mark_model_test_cancelled(status).await;
            return;
        }

        update_model_test_status(status, |status| {
            status.stage = "probe".into();
            status.current_tokens = *probe_tokens;
            status.current_estimated = true;
            status.last_error = None;
        })
        .await;

        let attempt = run_context_pressure_attempt(store, &matched, mode, *probe_tokens).await;
        let confirmation =
            context_confirmation(&attempt.usage, *probe_tokens, attempt.proof_verified);

        if attempt.ok {
            if !confirmation.verified {
                let supported = confirmed_tokens >= pass_threshold;
                update_model_test_status(status, |status| {
                    status.state = "completed".into();
                    status.stage = "done".into();
                    status.current_tokens = confirmation.tokens;
                    status.current_estimated = confirmation.estimated;
                    status.confirmed_tokens = confirmed_tokens;
                    status.confirmed_estimated = confirmed_estimated;
                    status.last_status = attempt.status;
                    status.latency_ms = attempt.latency_ms;
                    status.last_error =
                        Some("Response did not include the required context proof markers".into());
                    status.inconclusive = false;
                    status.supported = Some(supported);
                    status.summary = Some(pressure_test_summary(
                        mode,
                        supported,
                        false,
                        confirmed_tokens,
                        Some("context proof markers were missing"),
                    ));
                })
                .await;
                return;
            }

            if confirmation.tokens > confirmed_tokens {
                confirmed_tokens = confirmation.tokens;
                confirmed_estimated = confirmation.estimated;
            }
            update_model_test_status(status, |status| {
                status.current_tokens = confirmation.tokens;
                status.current_estimated = confirmation.estimated;
                status.confirmed_tokens = confirmed_tokens;
                status.confirmed_estimated = confirmed_estimated;
                status.last_status = attempt.status;
                status.latency_ms = attempt.latency_ms;
                status.last_error = None;
            })
            .await;
            continue;
        }

        let supported = confirmed_tokens >= pass_threshold;
        let inconclusive = !attempt.context_limit && !supported;
        update_model_test_status(status, |status| {
            status.state = "completed".into();
            status.stage = "done".into();
            status.current_tokens = confirmation.tokens;
            status.current_estimated = confirmation.estimated;
            status.confirmed_tokens = confirmed_tokens;
            status.confirmed_estimated = confirmed_estimated;
            status.last_status = attempt.status;
            status.latency_ms = attempt.latency_ms;
            status.last_error = attempt.error.clone();
            status.inconclusive = inconclusive;
            status.supported = if inconclusive { None } else { Some(supported) };
            status.summary = Some(pressure_test_summary(
                mode,
                supported,
                inconclusive,
                confirmed_tokens,
                attempt.error.as_deref(),
            ));
        })
        .await;
        return;
    }

    let supported = confirmed_tokens >= pass_threshold;
    update_model_test_status(status, |status| {
        status.state = "completed".into();
        status.stage = "done".into();
        status.confirmed_tokens = confirmed_tokens;
        status.confirmed_estimated = confirmed_estimated;
        status.inconclusive = false;
        status.supported = Some(supported);
        status.summary = Some(pressure_test_summary(
            mode,
            supported,
            false,
            confirmed_tokens,
            None,
        ));
    })
    .await;
}

async fn mark_model_test_cancelled(status: &Arc<Mutex<ModelTestStatus>>) {
    update_model_test_status(status, |status| {
        status.state = "cancelled".into();
        status.stage = "cancelled".into();
        status.summary = Some("Test cancelled".into());
    })
    .await;
}

struct PressureAttemptResult {
    ok: bool,
    status: u16,
    latency_ms: u128,
    usage: TokenUsage,
    proof_verified: bool,
    error: Option<String>,
    context_limit: bool,
}

struct ContextPressurePrompt {
    text: String,
    proof_markers: Vec<String>,
}

struct ContextConfirmation {
    tokens: u64,
    estimated: bool,
    verified: bool,
}

async fn run_context_pressure_attempt(
    store: &AppStore,
    matched: &RouteMatch,
    mode: ModelTestMode,
    probe_tokens: u64,
) -> PressureAttemptResult {
    let default_client = match default_http_client() {
        Ok(client) => client,
        Err(error) => {
            return PressureAttemptResult {
                ok: false,
                status: 0,
                latency_ms: 0,
                usage: TokenUsage::default(),
                proof_verified: false,
                error: Some(error),
                context_limit: false,
            };
        }
    };
    let client = match client_for_provider(store, &default_client, &matched.provider) {
        Ok(client) => client,
        Err(error) => {
            return PressureAttemptResult {
                ok: false,
                status: 0,
                latency_ms: 0,
                usage: TokenUsage::default(),
                proof_verified: false,
                error: Some(error),
                context_limit: false,
            };
        }
    };
    let prompt = context_pressure_prompt(probe_tokens);
    let (url, headers, payload) =
        match pressure_test_request_parts(store, &client, matched, mode, prompt.text.clone()).await
        {
            Ok(parts) => parts,
            Err(error) => {
                return PressureAttemptResult {
                    ok: false,
                    status: 0,
                    latency_ms: 0,
                    usage: TokenUsage::default(),
                    proof_verified: false,
                    error: Some(error),
                    context_limit: false,
                };
            }
        };

    let started = Instant::now();
    let mut request = client.post(&url).timeout(Duration::from_secs(180));
    for (name, value) in &headers {
        request = request.header(name, value);
    }
    let response = match request.json(&payload).send().await {
        Ok(response) => response,
        Err(error) => {
            return PressureAttemptResult {
                ok: false,
                status: 0,
                latency_ms: started.elapsed().as_millis(),
                usage: TokenUsage::default(),
                proof_verified: false,
                error: Some(format!("Could not reach provider: {error}")),
                context_limit: false,
            };
        }
    };
    let status = response.status().as_u16();
    let latency_ms = started.elapsed().as_millis();
    let text = match response.text().await {
        Ok(text) => text,
        Err(error) => {
            return PressureAttemptResult {
                ok: false,
                status,
                latency_ms,
                usage: TokenUsage::default(),
                proof_verified: false,
                error: Some(format!("Could not read provider response: {error}")),
                context_limit: false,
            };
        }
    };
    let value = test_response_value(&text);
    let usage = value
        .get("usage")
        .filter(|u| u.is_object())
        .map(|u| parse_usage(matched.provider.protocol.clone(), u))
        .unwrap_or_default();
    let reply = test_reply_from_value(&matched.provider.protocol, &value);
    let proof_verified = context_pressure_reply_verified(&reply, &prompt.proof_markers);

    if (200..300).contains(&status) {
        return PressureAttemptResult {
            ok: true,
            status,
            latency_ms,
            usage,
            proof_verified,
            error: None,
            context_limit: false,
        };
    }

    let error = provider_error_message(&value, &text);
    PressureAttemptResult {
        ok: false,
        status,
        latency_ms,
        usage,
        proof_verified,
        context_limit: is_explicit_context_limit_error(status, &error),
        error: Some(error),
    }
}

async fn pressure_test_request_parts(
    store: &AppStore,
    client: &reqwest::Client,
    matched: &RouteMatch,
    mode: ModelTestMode,
    prompt: String,
) -> Result<(String, Vec<(String, String)>, Value), String> {
    let (base_url, headers) = test_provider_upstream(store, client, matched).await?;
    Ok(pressure_test_request_shape(
        &matched.provider.protocol,
        &matched.provider.kind,
        &base_url,
        headers,
        &matched.upstream_model,
        matched.model.context_window,
        mode == ModelTestMode::Context1M,
        prompt,
    ))
}

fn pressure_test_request_shape(
    protocol: &ProviderProtocol,
    provider_kind: &ProviderKind,
    base_url: &str,
    headers: Vec<(String, String)>,
    upstream_model: &str,
    context_window: u64,
    force_one_million_context: bool,
    prompt: String,
) -> (String, Vec<(String, String)>, Value) {
    match protocol {
        ProviderProtocol::OpenAiResponses => {
            let official_openai = matches!(
                provider_kind,
                ProviderKind::OfficialOpenAi | ProviderKind::OfficialOpenAiAccount
            );
            let headers = if official_openai {
                openai_official_test_headers(headers)
            } else {
                headers
            };
            let payload = if official_openai {
                json!({
                    "model": upstream_model,
                    "input": [{
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": prompt
                        }]
                    }],
                    "instructions": "You are a context window test assistant. Read the full input and reply only with the FINAL_MARKER line if present.",
                    "store": false,
                    "stream": true
                })
            } else {
                json!({
                    "model": upstream_model,
                    "input": prompt,
                    "stream": false,
                    "max_output_tokens": 128
                })
            };
            (endpoint(base_url, "responses"), headers, payload)
        }
        ProviderProtocol::OpenAiChatCompletions => (
            endpoint(base_url, "chat/completions"),
            headers,
            json!({
                "model": upstream_model,
                "messages": [{ "role": "user", "content": prompt }],
                "stream": false,
                "max_tokens": 128
            }),
        ),
        ProviderProtocol::AnthropicMessages => {
            let context_window = if force_one_million_context {
                context_window.max(1_000_000)
            } else {
                context_window
            };
            let (upstream_model, one_million_context) =
                server::anthropic_model_for_request(upstream_model, context_window);
            let request = json!({
                "model": upstream_model,
                "input": prompt,
                "stream": false,
                "max_output_tokens": 128
            });
            let mut probe_body = server::build_anthropic_body(&request, &upstream_model, false);
            // 探测只测连通/input 容量：关掉思考，让小输出预算直接产出正文
            // （否则 thinking 会吃满预算，连通测试虽 200 但回复为空）。
            if let Some(object) = probe_body.as_object_mut() {
                object.remove("thinking");
                object.remove("output_config");
                if let Some(limit) = request.get("max_output_tokens").cloned() {
                    object.insert("max_tokens".into(), limit);
                }
            }
            (
                server::anthropic_messages_url(base_url, one_million_context),
                server::claude_code_mirror_headers(headers, &request, one_million_context),
                probe_body,
            )
        }
        ProviderProtocol::OpenAiImages => (
            endpoint(base_url, "images/generations"),
            headers,
            json!({ "model": upstream_model, "prompt": prompt, "n": 1, "size": "1024x1024" }),
        ),
        ProviderProtocol::GeminiImage => (
            format!(
                "{}/models/{}:generateContent",
                base_url.trim_end_matches('/'),
                upstream_model
            ),
            gemini_auth_headers(headers),
            json!({
                "contents": [{ "parts": [{ "text": prompt }] }],
                "generationConfig": { "responseModalities": ["IMAGE"] }
            }),
        ),
    }
}

fn context_pressure_prompt(estimated_tokens: u64) -> ContextPressurePrompt {
    const MARKER_COUNT: usize = 5;
    let seed = estimated_tokens
        .wrapping_mul(1_103_515_245)
        .wrapping_add(12_345);
    let proof_markers = (0..MARKER_COUNT)
        .map(|index| format!("NRCTX{:X}{index}", seed))
        .collect::<Vec<_>>();
    let repeat = estimated_tokens.saturating_sub(256) as usize;
    let segment_repeat = repeat / MARKER_COUNT;
    let mut remaining = repeat;
    let mut text = String::with_capacity(repeat.saturating_mul(2).saturating_add(1024));
    text.push_str("Neko Route context window probe.\n");
    text.push_str(
        "Read the whole input. Reply with NR_CONTEXT_OK followed by every CHECKPOINT value in order.\n",
    );
    text.push_str("Do not summarize. Do not invent missing checkpoints.\nBEGIN\n");
    for (index, marker) in proof_markers.iter().enumerate() {
        text.push_str(&format!("CHECKPOINT_{index}: {marker}\n"));
        let take = if index + 1 == MARKER_COUNT {
            remaining
        } else {
            segment_repeat.min(remaining)
        };
        for _ in 0..take {
            text.push_str(" a");
        }
        remaining = remaining.saturating_sub(take);
        text.push('\n');
    }
    text.push_str("END\n");
    text.push_str(
        "Reply now with NR_CONTEXT_OK and the CHECKPOINT values in order. The values are only in the input above.\n",
    );
    ContextPressurePrompt {
        text,
        proof_markers,
    }
}

fn context_confirmation(
    usage: &TokenUsage,
    fallback: u64,
    proof_verified: bool,
) -> ContextConfirmation {
    let prompt_tokens = usage
        .input_tokens
        .max(usage.total_tokens.saturating_sub(usage.output_tokens));
    if prompt_tokens > 0 {
        ContextConfirmation {
            tokens: prompt_tokens,
            estimated: false,
            verified: true,
        }
    } else {
        ContextConfirmation {
            tokens: fallback,
            estimated: true,
            verified: proof_verified,
        }
    }
}

fn context_pressure_reply_verified(reply: &str, proof_markers: &[String]) -> bool {
    if proof_markers.is_empty() {
        return false;
    }
    let normalized = reply.to_ascii_uppercase();
    normalized.contains("NR_CONTEXT_OK")
        && proof_markers
            .iter()
            .all(|marker| normalized.contains(&marker.to_ascii_uppercase()))
}

fn is_explicit_context_limit_error(status: u16, error: &str) -> bool {
    if status == 401 || status == 403 || status == 408 || status == 429 || status >= 500 {
        return false;
    }
    let lower = error.to_ascii_lowercase();
    lower.contains("context_length_exceeded")
        || lower.contains("context window is full")
        || lower.contains("maximum context length")
        || lower.contains("too many tokens")
        || (lower.contains("context") && (lower.contains("exceed") || lower.contains("full")))
        || (lower.contains("prompt") && lower.contains("too long"))
        || (lower.contains("input") && lower.contains("tokens") && lower.contains("exceed"))
}

fn pressure_test_summary(
    mode: ModelTestMode,
    supported: bool,
    inconclusive: bool,
    confirmed_tokens: u64,
    error: Option<&str>,
) -> String {
    if inconclusive {
        return format!(
            "Test inconclusive: {}",
            error.unwrap_or("upstream returned a non-context error")
        );
    }
    if supported {
        format!("Model supports {} context", mode.target_label())
    } else {
        format!(
            "Model did not reach {} context; last confirmed about {} tokens",
            mode.target_label(),
            confirmed_tokens
        )
    }
}

async fn test_matched_model(
    store: &AppStore,
    matched: RouteMatch,
    mode: ModelTestMode,
) -> Result<TestModelResult, String> {
    let default_client = default_http_client()?;
    let client = client_for_provider(store, &default_client, &matched.provider)?;
    let (url, headers, payload) = test_request_parts(store, &client, &matched, mode).await?;

    let started = Instant::now();
    let mut request = client.post(&url).timeout(Duration::from_secs(60));
    for (name, value) in &headers {
        request = request.header(name, value);
    }
    request = request.json(&payload);
    let response = request
        .send()
        .await
        .map_err(|error| format!("Could not reach provider: {error}"))?;
    let status = response.status().as_u16();
    let latency_ms = started.elapsed().as_millis();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Could not read provider response: {error}"))?;
    let value = test_response_value(&text);

    if !(200..300).contains(&status) {
        let error = provider_error_message(&value, &text);
        return Ok(TestModelResult {
            ok: false,
            status,
            latency_ms,
            reply: String::new(),
            error: Some(error),
            usage: TokenUsage::default(),
            provider_name: matched.provider.name.clone(),
            image_preview: None,
        });
    }

    // 图片测试用画图协议解析回复(检测 data → "(image generated)")，避免显示"无回复"。
    let reply = if mode == ModelTestMode::Image {
        test_reply_from_value(&ProviderProtocol::OpenAiImages, &value)
    } else {
        test_reply_from_value(&matched.provider.protocol, &value)
    };
    let usage = value
        .get("usage")
        .filter(|u| u.is_object())
        .map(|u| parse_usage(matched.provider.protocol.clone(), u))
        .unwrap_or_default();
    // 图片协议测试：把生成图 base64 带回前端预览(小猫)。
    let image_preview = (mode == ModelTestMode::Image
        || matched.provider.protocol == ProviderProtocol::OpenAiImages
        || matched.provider.protocol == ProviderProtocol::GeminiImage)
        .then(|| {
            // OpenAI: data[0].b64_json；Gemini: candidates[0].content.parts[].inlineData.data
            value
                .pointer("/data/0/b64_json")
                .and_then(|b64| b64.as_str())
                .or_else(|| {
                    value
                        .pointer("/candidates/0/content/parts")
                        .and_then(|parts| parts.as_array())
                        .and_then(|parts| {
                            parts.iter().find_map(|part| {
                                part.pointer("/inlineData/data")
                                    .or_else(|| part.pointer("/inline_data/data"))
                                    .and_then(|b64| b64.as_str())
                            })
                        })
                })
                .map(|s| s.to_string())
        })
        .flatten();

    Ok(TestModelResult {
        ok: true,
        status,
        latency_ms,
        reply,
        error: None,
        usage,
        provider_name: matched.provider.name.clone(),
        image_preview,
    })
}

async fn test_request_parts(
    store: &AppStore,
    client: &reqwest::Client,
    matched: &RouteMatch,
    mode: ModelTestMode,
) -> Result<(String, Vec<(String, String)>, Value), String> {
    let (base_url, headers) = test_provider_upstream(store, client, matched).await?;
    let upstream = &matched.upstream_model;
    // 图片测试：任意模型都强制走 images/generations 画一张猫，验证是否支持画图。
    if mode == ModelTestMode::Image {
        if matched.provider.protocol == ProviderProtocol::GeminiImage {
            return Ok((
                format!(
                    "{}/models/{}:generateContent",
                    base_url.trim_end_matches('/'),
                    upstream
                ),
                gemini_auth_headers(headers),
                json!({
                    "contents": [{ "parts": [{ "text": "a cute cat" }] }],
                    "generationConfig": { "responseModalities": ["IMAGE"] }
                }),
            ));
        }
        return Ok((
            endpoint(&base_url, "images/generations"),
            headers,
            json!({
                "model": upstream,
                "prompt": "a cute cat",
                "n": 1,
                "size": "1024x1024"
            }),
        ));
    }
    match &matched.provider.protocol {
        ProviderProtocol::OpenAiResponses => {
            let official_openai = matches!(
                matched.provider.kind,
                ProviderKind::OfficialOpenAi | ProviderKind::OfficialOpenAiAccount
            );
            Ok((
                endpoint(&base_url, "responses"),
                if official_openai {
                    openai_official_test_headers(headers)
                } else {
                    headers
                },
                if official_openai {
                    codex_responses_test_payload(upstream)
                } else {
                    json!({
                        "model": upstream,
                        "input": "hi",
                        "stream": false,
                        "max_output_tokens": 16
                    })
                },
            ))
        }
        ProviderProtocol::OpenAiChatCompletions => Ok((
            endpoint(&base_url, "chat/completions"),
            headers,
            json!({
                "model": upstream,
                "messages": [{ "role": "user", "content": "hi" }],
                "stream": false,
                "max_tokens": 16
            }),
        )),
        ProviderProtocol::AnthropicMessages => {
            let (upstream_model, one_million_context) =
                server::anthropic_model_for_request(upstream, matched.model.context_window);
            let request = json!({
                "model": upstream_model,
                "input": "hi",
                "stream": false,
                "max_output_tokens": 16
            });
            let mut probe_body = server::build_anthropic_body(&request, &upstream_model, false);
            // 探测只测连通/input 容量：关掉思考，让小输出预算直接产出正文
            // （否则 thinking 会吃满预算，连通测试虽 200 但回复为空）。
            if let Some(object) = probe_body.as_object_mut() {
                object.remove("thinking");
                object.remove("output_config");
                if let Some(limit) = request.get("max_output_tokens").cloned() {
                    object.insert("max_tokens".into(), limit);
                }
            }
            Ok((
                server::anthropic_messages_url(&base_url, one_million_context),
                server::claude_code_mirror_headers(headers, &request, one_million_context),
                probe_body,
            ))
        }
        ProviderProtocol::OpenAiImages => Ok((
            endpoint(&base_url, "images/generations"),
            headers,
            json!({
                "model": upstream,
                "prompt": "a cute cat",
                "n": 1,
                "size": "1024x1024"
            }),
        )),
        ProviderProtocol::GeminiImage => Ok((
            format!(
                "{}/models/{}:generateContent",
                base_url.trim_end_matches('/'),
                upstream
            ),
            gemini_auth_headers(headers),
            json!({
                "contents": [{ "parts": [{ "text": "a cute cat" }] }],
                "generationConfig": { "responseModalities": ["IMAGE"] }
            }),
        )),
    }
}

async fn test_provider_upstream(
    store: &AppStore,
    client: &reqwest::Client,
    matched: &RouteMatch,
) -> Result<(String, Vec<(String, String)>), String> {
    match &matched.provider.kind {
        ProviderKind::OfficialOpenAi => {
            let auth = official_auth::auth_for_codex_openai(client)
                .await
                .map_err(|_| "needs_codex_auth".to_string())?;
            Ok((auth.base_url, auth.headers))
        }
        ProviderKind::OfficialOpenAiAccount | ProviderKind::OfficialAnthropicAccount => {
            let auth = official_auth::auth_for_provider(client, &matched.provider).await?;
            Ok((auth.base_url, auth.headers))
        }
        ProviderKind::OfficialAnthropicCli => {
            let auth = claude_auth::cli_auth()?;
            Ok((auth.base_url, auth.headers))
        }
        ProviderKind::OfficialAnthropicDesktop => {
            let auth = claude_auth::desktop_auth()?;
            Ok((auth.base_url, auth.headers))
        }
        ProviderKind::Custom => {
            let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
            if let Some(key_ref) = matched.provider.key_ref.as_deref() {
                let secret = store.key_store().get_secret(key_ref)?.ok_or_else(|| {
                    format!(
                        "Provider '{}' needs an API key in local storage",
                        matched.provider.name
                    )
                })?;
                headers.push(("authorization".into(), format!("Bearer {secret}")));
            }
            Ok((matched.provider.base_url.clone(), headers))
        }
    }
}

fn codex_responses_test_payload(model: &str) -> Value {
    json!({
        "model": model,
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": "hi"
            }
        ],
        "instructions": CODEX_TEST_INSTRUCTIONS,
        "store": false,
        "stream": true
    })
}

const CODEX_TEST_INSTRUCTIONS: &str =
    "You are Codex, a concise coding assistant. Answer the user's test prompt naturally.";

fn openai_official_test_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut cleaned = Vec::new();
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "authorization"
                | "chatgpt-account-id"
                | "oai-language"
                | "originator"
                | "openai-beta"
                | "user-agent"
        ) {
            cleaned.push((name, value));
        }
    }
    set_header(&mut cleaned, "originator", "codex_cli_rs");
    set_header(
        &mut cleaned,
        "user-agent",
        "codex_cli_rs/0.125.0 (Ubuntu 22.4.0; x86_64) xterm-256color",
    );
    set_header(&mut cleaned, "content-type", "application/json");
    set_header(
        &mut cleaned,
        "accept",
        "text/event-stream, application/json",
    );
    set_header(&mut cleaned, "accept-encoding", "identity");
    cleaned
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    headers.push((name.to_string(), value.to_string()));
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!("{}/{}", base_url.trim_end_matches('/'), path)
}

fn test_response_value(text: &str) -> Value {
    serde_json::from_str::<Value>(text)
        .ok()
        .or_else(|| sse_test_response_value(text))
        .unwrap_or_else(|| json!({ "raw": text }))
}

fn sse_test_response_value(text: &str) -> Option<Value> {
    let mut last = None;
    let mut delta_text = String::new();
    let mut final_text = None;
    let mut chat_seen = false;
    let mut chat_content = String::new();
    let mut chat_reasoning = String::new();
    let mut chat_usage = None;
    for block in text.split("\n\n") {
        let event_name = block.lines().find_map(|line| {
            line.trim_start()
                .strip_prefix("event:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        });
        let data = block
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("data:"))
            .map(str::trim)
            .filter(|line| !line.is_empty() && *line != "[DONE]")
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(&data).ok()?;
        if value.get("choices").and_then(Value::as_array).is_some() {
            chat_seen = true;
            if let Some(content) = chat_delta_text(&value) {
                chat_content.push_str(&content);
            }
            if let Some(reasoning) = chat_delta_reasoning_text(&value) {
                chat_reasoning.push_str(&reasoning);
            }
            if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
                chat_usage = Some(usage.clone());
            }
            last = Some(value);
            continue;
        }
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .or(event_name.as_deref());
        if let Some(delta) = sse_delta_text(&value, event_type) {
            delta_text.push_str(&delta);
        }
        if let Some(text) = sse_final_text(&value, event_type) {
            final_text = Some(text);
        }
        if matches!(event_type, Some("response.completed")) {
            let mut response = value.get("response").cloned().unwrap_or(value);
            fill_output_text(
                &mut response,
                fallback_output_text(final_text.as_deref(), &delta_text),
            );
            return Some(response);
        }
        if matches!(event_type, Some("response.failed")) {
            return value.get("response").cloned().or(Some(value));
        }
        last = Some(value.get("response").cloned().unwrap_or(value));
    }
    if chat_seen {
        return Some(chat_test_response_value(
            fallback_output_text(Some(&chat_content), &chat_reasoning),
            chat_usage,
        ));
    }
    let mut value = last?;
    fill_output_text(
        &mut value,
        fallback_output_text(final_text.as_deref(), &delta_text),
    );
    Some(value)
}

fn chat_delta_text(value: &Value) -> Option<String> {
    value
        .pointer("/choices/0/delta/content")
        .or_else(|| value.pointer("/choices/0/message/content"))
        .map(text_from_model_test_content)
        .filter(|text| !text.is_empty())
}

fn chat_delta_reasoning_text(value: &Value) -> Option<String> {
    value
        .pointer("/choices/0/delta/reasoning_content")
        .or_else(|| value.pointer("/choices/0/message/reasoning_content"))
        .or_else(|| value.pointer("/choices/0/delta/reasoning"))
        .or_else(|| value.pointer("/choices/0/message/reasoning"))
        .map(text_from_model_test_content)
        .filter(|text| !text.is_empty())
}

fn chat_test_response_value(content: &str, usage: Option<Value>) -> Value {
    let mut value = json!({
        "choices": [{
            "message": {
                "content": content
            }
        }]
    });
    if let Some(usage) = usage {
        value["usage"] = usage;
    }
    value
}

fn sse_delta_text(value: &Value, event_type: Option<&str>) -> Option<String> {
    if !matches!(event_type, Some("response.output_text.delta")) {
        return None;
    }
    value
        .get("delta")
        .or_else(|| value.get("text"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn sse_final_text(value: &Value, event_type: Option<&str>) -> Option<String> {
    match event_type {
        Some("response.output_text.done") => value
            .get("text")
            .or_else(|| value.get("output_text"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        Some("response.content_part.done") => value
            .get("part")
            .and_then(|part| {
                part.get("text")
                    .or_else(|| part.get("output_text"))
                    .and_then(Value::as_str)
            })
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn fallback_output_text<'a>(final_text: Option<&'a str>, delta_text: &'a str) -> &'a str {
    final_text
        .filter(|text| !text.is_empty())
        .unwrap_or(delta_text)
}

fn fill_output_text(value: &mut Value, output_text: &str) {
    if output_text.is_empty() {
        return;
    }
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let existing = object
        .get("output_text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if existing.is_empty() {
        object.insert("output_text".into(), Value::String(output_text.to_string()));
    }
}

fn provider_error_message(value: &Value, raw: &str) -> String {
    if let Some(message) = value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .or_else(|| value.get("error").and_then(|error| error.get("message")))
        .or_else(|| value.get("error"))
        .or_else(|| value.get("detail"))
        .or_else(|| value.pointer("/response/error/message"))
        .or_else(|| value.get("raw"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
    {
        return truncate_text(message, 400);
    }

    let raw = raw.trim();
    if raw.is_empty() {
        "request_failed".into()
    } else {
        truncate_text(raw, 400)
    }
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn test_reply_from_value(protocol: &ProviderProtocol, value: &Value) -> String {
    match protocol {
        ProviderProtocol::OpenAiResponses => value
            .get("output_text")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| reply_from_output(value)),
        ProviderProtocol::OpenAiChatCompletions => chat_reply_from_value(value),
        ProviderProtocol::AnthropicMessages => value
            .get("content")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default(),
        ProviderProtocol::OpenAiImages => {
            if value
                .get("data")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
            {
                "(image generated)".to_string()
            } else {
                String::new()
            }
        }
        ProviderProtocol::GeminiImage => {
            let has_image = value
                .pointer("/candidates/0/content/parts")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    parts.iter().any(|part| {
                        part.pointer("/inlineData/data").is_some()
                            || part.pointer("/inline_data/data").is_some()
                    })
                });
            if has_image {
                "(image generated)".to_string()
            } else {
                String::new()
            }
        }
    }
}

/// 把含 `Authorization: Bearer KEY` 的 headers 转成 Gemini 的 `x-goog-api-key`。
fn gemini_auth_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .map(|(key, value)| {
            if key.eq_ignore_ascii_case("authorization") {
                (
                    "x-goog-api-key".to_string(),
                    value.strip_prefix("Bearer ").unwrap_or(&value).to_string(),
                )
            } else {
                (key, value)
            }
        })
        .collect()
}

fn chat_reply_from_value(value: &Value) -> String {
    let message = value.pointer("/choices/0/message");
    message
        .and_then(|message| message.get("content"))
        .map(text_from_model_test_content)
        .filter(|text| !text.is_empty())
        .or_else(|| {
            message
                .and_then(|message| {
                    message
                        .get("reasoning_content")
                        .or_else(|| message.get("reasoning"))
                })
                .map(text_from_model_test_content)
                .filter(|text| !text.is_empty())
        })
        .or_else(|| {
            value
                .pointer("/choices/0/text")
                .map(text_from_model_test_content)
                .filter(|text| !text.is_empty())
        })
        .unwrap_or_default()
}

fn text_from_model_test_content(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(text_from_model_test_content)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(""),
        Value::Object(object) => object
            .get("text")
            .or_else(|| object.get("content"))
            .or_else(|| object.get("reasoning_content"))
            .or_else(|| object.get("reasoning"))
            .map(text_from_model_test_content)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn pretty_json_or_raw(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| raw.to_string())
}

fn reply_from_output(value: &Value) -> String {
    value
        .get("output")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("content").and_then(Value::as_array))
                .flatten()
                .filter_map(|content| content.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[derive(Serialize)]
struct UpstreamModel {
    id: String,
    label: String,
}

#[derive(Serialize)]
struct UpstreamModelList {
    models: Vec<UpstreamModel>,
    error: Option<String>,
}

/// Fetch the catalog of models a provider exposes via its `/models` endpoint,
/// so the model dialog can offer a dropdown instead of free-text entry.
#[tauri::command]
async fn list_upstream_models(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
) -> Result<UpstreamModelList, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;

    let models = upstream_models_for(&store, provider).await?;
    Ok(UpstreamModelList {
        models,
        error: None,
    })
}

/// 拉取某个上游 provider 暴露的模型列表：OpenAI 官方走内置列表，其余 provider 真打 `/models`。
/// `list_upstream_models` 命令和直连模式 apply（取第一个模型）都复用它。
async fn upstream_models_for(
    store: &AppStore,
    provider: &Provider,
) -> Result<Vec<UpstreamModel>, String> {
    if let Some(models) = openai_official_upstream_models(provider) {
        return Ok(models);
    }
    let default_client = default_http_client()?;
    let client = client_for_provider(store, &default_client, provider)?;

    // Resolve base URL + auth headers per provider kind.
    let (base_url, headers, anthropic) = match &provider.kind {
        ProviderKind::OfficialOpenAi | ProviderKind::OfficialOpenAiAccount => unreachable!(),
        ProviderKind::OfficialAnthropicCli => {
            let auth = claude_auth::cli_auth()?;
            (auth.base_url, auth.headers, true)
        }
        ProviderKind::OfficialAnthropicDesktop => {
            let auth = claude_auth::desktop_auth()?;
            (auth.base_url, auth.headers, true)
        }
        ProviderKind::OfficialAnthropicAccount => {
            let auth = official_auth::auth_for_provider(&client, provider).await?;
            (auth.base_url, auth.headers, true)
        }
        ProviderKind::Custom => {
            let anthropic = provider.protocol == ProviderProtocol::AnthropicMessages;
            let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
            if let Some(key_ref) = provider.key_ref.as_deref() {
                let secret = store
                    .key_store()
                    .get_secret(key_ref)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "This provider needs an API key first".to_string())?;
                headers.push(("authorization".into(), format!("Bearer {secret}")));
            }
            (provider.base_url.clone(), headers, anthropic)
        }
    };

    fetch_upstream_models(&client, &base_url, &headers, anthropic).await
}

fn openai_official_upstream_models(
    provider: &crate::types::Provider,
) -> Option<Vec<UpstreamModel>> {
    if !matches!(
        provider.kind,
        ProviderKind::OfficialOpenAi | ProviderKind::OfficialOpenAiAccount
    ) {
        return None;
    }
    Some(
        official_auth::openai_builtin_models()
            .into_iter()
            .map(|(id, label)| UpstreamModel { id, label })
            .collect(),
    )
}

async fn fetch_upstream_models(
    client: &reqwest::Client,
    base_url: &str,
    headers: &[(String, String)],
    anthropic: bool,
) -> Result<Vec<UpstreamModel>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = client.get(&url).timeout(Duration::from_secs(20));
    for (name, value) in headers {
        let value = if name.eq_ignore_ascii_case("accept") {
            "application/json"
        } else {
            value
        };
        request = request.header(name, value);
    }
    if anthropic
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("anthropic-version"))
    {
        request = request.header("anthropic-version", "2023-06-01");
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("Could not reach provider: {e}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("Could not read provider response: {e}"))?;
    if !status.is_success() {
        return Err(provider_status_error(status, &text));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|e| format!("Invalid provider response: {e}"))?;

    let items = value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut models = items
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(Value::as_str)?.to_string();
            let label = item
                .get("display_name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| id.clone());
            Some(UpstreamModel { id, label })
        })
        .collect::<Vec<_>>();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

fn provider_status_error(status: reqwest::StatusCode, body: &str) -> String {
    let value = test_response_value(body);
    let message = provider_error_message(&value, body);
    format!("Provider returned {}: {}", status.as_u16(), message)
}

fn codex_restart_action(was_running: bool) -> &'static str {
    if was_running {
        "restarted"
    } else {
        "started"
    }
}

#[cfg(target_os = "macos")]
fn codex_macos_process_pattern() -> &'static str {
    "Codex.app/Contents/MacOS/Codex$"
}

#[cfg(target_os = "macos")]
fn codex_desktop_running() -> bool {
    Command::new("pgrep")
        .args(["-f", codex_macos_process_pattern()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "macos")]
fn stop_codex_desktop() -> Result<(), String> {
    let _ = Command::new("osascript")
        .args(["-e", "tell application \"Codex\" to quit"])
        .status();
    if wait_until_codex_stopped(Duration::from_secs(6)) {
        return Ok(());
    }
    let _ = Command::new("pkill")
        .args(["-TERM", "-f", codex_macos_process_pattern()])
        .status();
    if wait_until_codex_stopped(Duration::from_secs(3)) {
        return Ok(());
    }
    let _ = Command::new("pkill")
        .args(["-KILL", "-f", codex_macos_process_pattern()])
        .status();
    if wait_until_codex_stopped(Duration::from_secs(3)) {
        Ok(())
    } else {
        Err("Could not stop Codex desktop app".into())
    }
}

#[cfg(target_os = "macos")]
fn start_codex_desktop() -> Result<(), String> {
    if Command::new("open")
        .args(["-a", "Codex"])
        .status()
        .is_ok_and(|status| status.success())
    {
        return Ok(());
    }
    Command::new("open")
        .arg("/Applications/Codex.app")
        .status()
        .map_err(|error| format!("Could not start Codex desktop app: {error}"))
        .and_then(|status| {
            status
                .success()
                .then_some(())
                .ok_or_else(|| "Could not start Codex desktop app".to_string())
        })
}

#[cfg(target_os = "windows")]
const WINDOWS_CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(any(test, target_os = "windows"))]
fn dedupe_paths_case_insensitive(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key) {
            deduped.push(path);
        }
    }
    deduped
}

#[cfg(any(test, target_os = "windows"))]
fn merge_windows_codex_launch_paths(
    running_paths: Vec<PathBuf>,
    registry_paths: Vec<PathBuf>,
    path_paths: Vec<PathBuf>,
    common_paths: Vec<PathBuf>,
    shortcut_paths: Vec<PathBuf>,
    scanned_paths: Vec<PathBuf>,
) -> Vec<PathBuf> {
    dedupe_paths_case_insensitive(
        running_paths
            .into_iter()
            .chain(registry_paths)
            .chain(path_paths)
            .chain(common_paths)
            .chain(shortcut_paths)
            .chain(scanned_paths)
            .collect(),
    )
}

#[cfg(any(test, target_os = "windows"))]
fn windows_restart_preflight_error(was_running: bool, concrete_targets: usize) -> Option<String> {
    if was_running && concrete_targets == 0 {
        Some("Codex is running, but Neko Route could not find its executable path before restarting. Codex was left running.".into())
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn windows_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    command
}

#[cfg(target_os = "windows")]
fn windows_codex_process_names() -> [&'static str; 2] {
    ["Codex.exe", "OpenAI Codex.exe"]
}

#[cfg(target_os = "windows")]
fn windows_codex_executable_names() -> [&'static str; 2] {
    ["Codex.exe", "OpenAI Codex.exe"]
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone)]
struct WindowsCodexProcess {
    executable_path: Option<PathBuf>,
}

#[cfg(target_os = "windows")]
struct WindowsHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(target_os = "windows")]
impl Drop for WindowsHandle {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_wide_to_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|char| *char == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

#[cfg(target_os = "windows")]
fn windows_process_image_path(process_id: u32) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if handle.is_null() {
        return None;
    }
    let _handle = WindowsHandle(handle);
    let mut buffer = vec![0u16; 32768];
    let mut size = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
    if ok == 0 || size == 0 {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..size as usize],
    )))
}

#[cfg(target_os = "windows")]
fn windows_codex_running_processes() -> Vec<WindowsCodexProcess> {
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let _snapshot = WindowsHandle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut processes = Vec::new();
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while has_entry {
        let exe_name = windows_wide_to_string(&entry.szExeFile);
        if windows_codex_process_names()
            .iter()
            .any(|name| exe_name.eq_ignore_ascii_case(name))
        {
            processes.push(WindowsCodexProcess {
                executable_path: windows_process_image_path(entry.th32ProcessID),
            });
        }
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    processes
}

#[cfg(target_os = "windows")]
fn codex_desktop_running() -> bool {
    !windows_codex_running_processes().is_empty()
}

#[cfg(target_os = "windows")]
fn stop_codex_desktop() -> Result<(), String> {
    for process_name in windows_codex_process_names() {
        let _ = windows_command("taskkill")
            .args(["/IM", process_name, "/T", "/F"])
            .status();
    }
    if wait_until_codex_stopped(Duration::from_secs(8)) {
        Ok(())
    } else {
        Err("Could not stop Codex desktop app".into())
    }
}

#[cfg(target_os = "windows")]
fn parse_windows_reg_default_path(output: &str) -> Option<PathBuf> {
    output.lines().find_map(|line| {
        let (_, value) = line.split_once("REG_SZ")?;
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    })
}

#[cfg(target_os = "windows")]
fn windows_codex_app_path_from_registry() -> Vec<PathBuf> {
    [
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\Codex.exe",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\OpenAI Codex.exe",
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\App Paths\Codex.exe",
        r"HKLM\Software\Microsoft\Windows\CurrentVersion\App Paths\OpenAI Codex.exe",
    ]
    .iter()
    .filter_map(|key| {
        let output = windows_command("reg")
            .args(["query", key, "/ve"])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).to_string())
            .and_then(|text| parse_windows_reg_default_path(&text))
    })
    .collect()
}

#[cfg(target_os = "windows")]
fn windows_codex_paths_from_path() -> Vec<PathBuf> {
    windows_codex_executable_names()
        .iter()
        .filter_map(|executable| {
            windows_command("where.exe")
                .arg(executable)
                .output()
                .ok()
                .filter(|output| output.status.success())
        })
        .flat_map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn push_windows_codex_folder_paths(paths: &mut Vec<PathBuf>, folder: PathBuf) {
    for executable in windows_codex_executable_names() {
        paths.push(folder.join(executable));
    }
}

#[cfg(target_os = "windows")]
fn windows_codex_common_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        for folder in [
            local_app_data.join("Programs").join("Codex"),
            local_app_data.join("Programs").join("OpenAI Codex"),
            local_app_data.join("Codex"),
            local_app_data.join("OpenAI Codex"),
            local_app_data.join("OpenAI").join("Codex"),
            local_app_data.join("Microsoft").join("WindowsApps"),
        ] {
            push_windows_codex_folder_paths(&mut paths, folder);
        }
    }
    for var in ["PROGRAMFILES", "PROGRAMFILES(X86)"] {
        if let Some(base) = env::var_os(var).map(PathBuf::from) {
            for folder in [
                base.join("Codex"),
                base.join("OpenAI Codex"),
                base.join("OpenAI").join("Codex"),
            ] {
                push_windows_codex_folder_paths(&mut paths, folder);
            }
        }
    }
    paths
}

#[cfg(target_os = "windows")]
fn collect_windows_codex_executables(root: &Path, depth: usize, paths: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let lower = file_name.to_ascii_lowercase();
        if path.is_file() {
            if windows_codex_executable_names()
                .iter()
                .any(|name| file_name.eq_ignore_ascii_case(name))
                || (lower.ends_with(".exe") && lower.contains("codex"))
            {
                paths.push(path);
            }
            continue;
        }
        if path.is_dir() {
            collect_windows_codex_executables(&path, depth - 1, paths);
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_codex_shallow_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        collect_windows_codex_executables(&local_app_data.join("Programs"), 3, &mut paths);
    }
    for var in ["PROGRAMFILES", "PROGRAMFILES(X86)"] {
        if let Some(base) = env::var_os(var).map(PathBuf::from) {
            collect_windows_codex_executables(&base, 2, &mut paths);
        }
    }
    paths
}

#[cfg(target_os = "windows")]
fn collect_windows_shortcuts(root: &PathBuf, depth: usize, paths: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_windows_shortcuts(&path, depth - 1, paths);
            continue;
        }
        let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let lower = file_name.to_ascii_lowercase();
        if lower.ends_with(".lnk") && lower.contains("codex") {
            paths.push(path);
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_start_menu_programs_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(app_data) = env::var_os("APPDATA").map(PathBuf::from) {
        dirs.push(
            app_data
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    if let Some(program_data) = env::var_os("PROGRAMDATA").map(PathBuf::from) {
        dirs.push(
            program_data
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    dirs
}

#[cfg(target_os = "windows")]
fn windows_codex_shortcut_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for programs in windows_start_menu_programs_dirs() {
        for name in ["Codex.lnk", "OpenAI Codex.lnk", "Codex Desktop.lnk"] {
            paths.push(programs.join(name));
            paths.push(programs.join("OpenAI").join(name));
        }
        collect_windows_shortcuts(&programs, 4, &mut paths);
    }
    paths
}

#[cfg(target_os = "windows")]
fn shell_execute_windows(target: &OsStr) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{UI::Shell::ShellExecuteW, UI::WindowsAndMessaging::SW_SHOWNORMAL};

    let verb = OsStr::new("open")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let file = target
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecute failed with code {result}"))
    }
}

#[cfg(target_os = "windows")]
fn windows_codex_launch_candidates(running_paths: Vec<PathBuf>) -> Vec<PathBuf> {
    merge_windows_codex_launch_paths(
        running_paths,
        windows_codex_app_path_from_registry(),
        windows_codex_paths_from_path(),
        windows_codex_common_paths(),
        windows_codex_shortcut_paths(),
        windows_codex_shallow_search_paths(),
    )
}

#[cfg(target_os = "windows")]
fn existing_windows_codex_launch_candidates(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    candidates
        .into_iter()
        .filter(|path| path.exists())
        .collect()
}

#[cfg(target_os = "windows")]
fn start_codex_desktop_from_candidates(
    candidates: Vec<PathBuf>,
    allow_shell_fallback: bool,
) -> Result<(), String> {
    let had_concrete_candidates = !candidates.is_empty();
    let mut errors = Vec::new();
    for path in candidates {
        match shell_execute_windows(path.as_os_str()) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }

    if allow_shell_fallback {
        for target in ["Codex.exe", "Codex"] {
            match shell_execute_windows(OsStr::new(target)) {
                Ok(()) => return Ok(()),
                Err(error) => errors.push(format!("{target}: {error}")),
            }
        }
    }

    if errors.is_empty() {
        Err("Could not find Codex desktop executable. Checked the running process path, App Paths registry, PATH, common install folders, and Start Menu shortcuts.".into())
    } else if !had_concrete_candidates {
        Err(format!(
            "Could not find a concrete Codex desktop executable path. Fallback launches failed: {}",
            errors.join("; ")
        ))
    } else {
        Err(format!(
            "Could not start Codex desktop app. Tried: {}",
            errors.join("; ")
        ))
    }
}

#[cfg(target_os = "windows")]
fn restart_or_start_codex_desktop() -> Result<CodexAppRestartResult, String> {
    let running_processes = windows_codex_running_processes();
    let running = !running_processes.is_empty();
    let running_paths = running_processes
        .iter()
        .filter_map(|process| process.executable_path.clone())
        .collect::<Vec<_>>();
    let candidates =
        existing_windows_codex_launch_candidates(windows_codex_launch_candidates(running_paths));

    if let Some(error) = windows_restart_preflight_error(running, candidates.len()) {
        return Err(error);
    }
    if running {
        stop_codex_desktop()?;
    }
    start_codex_desktop_from_candidates(candidates, !running)?;
    Ok(CodexAppRestartResult {
        action: codex_restart_action(running),
    })
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn codex_desktop_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "Codex"])
        .status()
        .is_ok_and(|status| status.success())
        || Command::new("pgrep")
            .args(["-x", "codex"])
            .status()
            .is_ok_and(|status| status.success())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn stop_codex_desktop() -> Result<(), String> {
    let _ = Command::new("pkill")
        .args(["-TERM", "-x", "Codex"])
        .status();
    let _ = Command::new("pkill")
        .args(["-TERM", "-x", "codex"])
        .status();
    if wait_until_codex_stopped(Duration::from_secs(5)) {
        return Ok(());
    }
    let _ = Command::new("pkill")
        .args(["-KILL", "-x", "Codex"])
        .status();
    let _ = Command::new("pkill")
        .args(["-KILL", "-x", "codex"])
        .status();
    if wait_until_codex_stopped(Duration::from_secs(3)) {
        Ok(())
    } else {
        Err("Could not stop Codex desktop app".into())
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn start_codex_desktop() -> Result<(), String> {
    for (program, args) in [
        ("gtk-launch", &["codex.desktop"][..]),
        ("gtk-launch", &["Codex.desktop"][..]),
        ("codex", &[][..]),
    ] {
        if Command::new(program)
            .args(args)
            .status()
            .is_ok_and(|status| status.success())
        {
            return Ok(());
        }
    }
    Err("Could not start Codex desktop app".into())
}

fn wait_until_codex_stopped(timeout: Duration) -> bool {
    let started_at = Instant::now();
    while started_at.elapsed() < timeout {
        if !codex_desktop_running() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    !codex_desktop_running()
}

#[tauri::command]
async fn refresh_provider_usage(
    store: tauri::State<'_, AppStore>,
    provider_id: String,
) -> Result<AppSnapshot, String> {
    let config = store.config().await;
    let provider = config
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| "Provider not found".to_string())?;
    match refresh_official_usage_for_provider(&store, provider).await {
        Ok(()) => Ok(store.snapshot().await),
        Err(error) => {
            if matches!(
                &provider.kind,
                ProviderKind::OfficialOpenAiAccount
                    | ProviderKind::OfficialOpenAi
                    | ProviderKind::OfficialAnthropicAccount
                    | ProviderKind::OfficialAnthropicCli
                    | ProviderKind::OfficialAnthropicDesktop
            ) {
                store
                    .update_provider_usage_snapshot(
                        provider.id.clone(),
                        "unavailable".into(),
                        None,
                        Some(error.clone()),
                    )
                    .await;
            }
            Err(error)
        }
    }
}

async fn refresh_official_usage_for_provider(
    store: &AppStore,
    provider: &Provider,
) -> Result<(), String> {
    let default_client = default_http_client()?;
    let client = client_for_provider(store, &default_client, provider)?;
    let quota = match &provider.kind {
        ProviderKind::OfficialOpenAiAccount => {
            official_usage::fetch_openai_quota(&client, provider).await
        }
        ProviderKind::OfficialOpenAi => official_usage::fetch_codex_openai_quota(&client).await,
        ProviderKind::OfficialAnthropicCli => official_usage::fetch_claude_cli_quota(&client).await,
        ProviderKind::OfficialAnthropicDesktop => {
            official_usage::fetch_claude_desktop_quota(&client).await
        }
        ProviderKind::OfficialAnthropicAccount => {
            official_usage::fetch_claude_quota(&client, provider).await
        }
        _ => return Err("Only official account providers expose account quota".into()),
    }?;
    store
        .update_provider_usage_snapshot(provider.id.clone(), "live".into(), Some(quota), None)
        .await;
    Ok(())
}

#[tauri::command]
async fn export_catalog(store: tauri::State<'_, AppStore>) -> Result<String, String> {
    let codex_home = codex_config::resolve_codex_home();
    let path = store.export_catalog_to(&codex_home).await?;
    Ok(path.display().to_string())
}

#[tauri::command]
async fn install_codex_config(
    store: tauri::State<'_, AppStore>,
    default_model: Option<String>,
) -> Result<codex_config::InjectionResult, String> {
    let config = store.config().await;
    match store
        .inject_codex_config_for(&config, default_model.as_deref())
        .await
    {
        Ok(result) => {
            store.clear_codex_apply_error().await;
            Ok(result)
        }
        Err(error) => {
            store.set_codex_apply_error(error.clone()).await;
            Err(error)
        }
    }
}

#[tauri::command]
async fn restore_codex_config(delete_catalog: bool) -> Result<codex_config::RestoreResult, String> {
    codex_config::restore(delete_catalog)
}

#[tauri::command]
async fn read_codex_config() -> Result<codex_config::CodexConfigContent, String> {
    codex_config::read_codex_config_file()
}

#[tauri::command]
async fn save_codex_config(content: String) -> Result<codex_config::ManualSaveResult, String> {
    codex_config::save_codex_config_file(&content)
}

#[tauri::command]
async fn import_sessions() -> Result<session_import::ImportResult, String> {
    tokio::task::spawn_blocking(session_import::import_sessions)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn clear_request_logs(store: tauri::State<'_, AppStore>) -> Result<AppSnapshot, String> {
    store.clear_requests().await;
    Ok(store.snapshot().await)
}

/// 读 image_cache 里的画图原图，返回 base64(供日志点击预览)。
#[tauri::command]
async fn read_image_preview(
    store: tauri::State<'_, AppStore>,
    name: String,
) -> Result<String, String> {
    use base64::Engine;
    store
        .image_preview_bytes(&name)
        .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
        .ok_or_else(|| "Image not found".to_string())
}

#[tauri::command]
async fn get_request_logs(
    store: tauri::State<'_, AppStore>,
    page: usize,
    page_size: usize,
) -> Result<RequestLogPage, String> {
    Ok(store.request_log_page(page, page_size).await)
}

#[cfg(test)]
mod tests {
    use super::{
        codex_responses_test_payload, codex_restart_action, context_confirmation,
        context_pressure_prompt, context_pressure_reply_verified, dedupe_paths_case_insensitive,
        is_explicit_context_limit_error, merge_windows_codex_launch_paths,
        openai_official_test_headers, openai_official_upstream_models, pressure_test_request_shape,
        provider_error_message, provider_status_error, server_bind_settings_changed,
        test_reply_from_value, test_response_value, windows_restart_preflight_error, ModelTestMode,
    };
    use crate::types::{seeded_config, Provider, ProviderKind, ProviderProtocol, TokenUsage};
    use std::path::PathBuf;

    #[test]
    fn official_openai_test_payload_uses_minimal_codex_responses_shape() {
        let payload = codex_responses_test_payload("gpt-5.5");

        assert_eq!(payload["model"], "gpt-5.5");
        assert_eq!(payload["input"][0]["type"], "message");
        assert_eq!(payload["input"][0]["content"], "hi");
        let instructions = payload["instructions"].as_str().unwrap();
        assert!(instructions.contains("Answer the user's test prompt naturally"));
        assert!(!instructions.contains("Reply with OK"));
        assert_eq!(payload["store"], false);
        assert_eq!(payload["stream"], true);
        for field in [
            "include",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "reasoning",
            "max_output_tokens",
        ] {
            assert!(payload.get(field).is_none(), "{field} should not be sent");
        }
    }

    #[test]
    fn pressure_payload_limits_output_for_each_protocol() {
        let (_, _, responses) = pressure_test_request_shape(
            &ProviderProtocol::OpenAiResponses,
            &ProviderKind::Custom,
            "https://relay.example/v1",
            vec![],
            "gpt-test",
            400_000,
            false,
            "large prompt".into(),
        );
        assert_eq!(responses["max_output_tokens"], 128);
        assert_eq!(responses["input"], "large prompt");

        let (_, _, official_responses) = pressure_test_request_shape(
            &ProviderProtocol::OpenAiResponses,
            &ProviderKind::OfficialOpenAiAccount,
            "https://chatgpt.com/backend-api/codex",
            vec![],
            "gpt-5.4-pro",
            400_000,
            false,
            "large prompt".into(),
        );
        assert_eq!(official_responses["model"], "gpt-5.4-pro");
        assert_eq!(
            official_responses["input"][0]["content"][0]["type"],
            "input_text"
        );
        assert_eq!(
            official_responses["input"][0]["content"][0]["text"],
            "large prompt"
        );
        assert_eq!(official_responses["store"], false);
        assert_eq!(official_responses["stream"], true);
        assert!(official_responses.get("max_output_tokens").is_none());

        let (_, _, chat) = pressure_test_request_shape(
            &ProviderProtocol::OpenAiChatCompletions,
            &ProviderKind::Custom,
            "https://relay.example/v1",
            vec![],
            "chat-test",
            400_000,
            false,
            "large prompt".into(),
        );
        assert_eq!(chat["max_tokens"], 128);
        assert_eq!(chat["messages"][0]["content"], "large prompt");

        let (_, _, anthropic) = pressure_test_request_shape(
            &ProviderProtocol::AnthropicMessages,
            &ProviderKind::Custom,
            "https://relay.example/v1",
            vec![],
            "claude-test",
            400_000,
            false,
            "large prompt".into(),
        );
        assert_eq!(anthropic["max_tokens"], 128);
        assert_eq!(anthropic["messages"][0]["content"][0]["type"], "text");
        assert_eq!(
            anthropic["messages"][0]["content"][0]["text"],
            "large prompt"
        );
        assert_eq!(
            anthropic["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn pressure_confirmation_requires_usage_or_context_proof() {
        let no_usage = TokenUsage::default();
        let unverified = context_confirmation(&no_usage, 380_000, false);
        assert_eq!(unverified.tokens, 380_000);
        assert!(unverified.estimated);
        assert!(!unverified.verified);

        let verified_by_proof = context_confirmation(&no_usage, 380_000, true);
        assert_eq!(verified_by_proof.tokens, 380_000);
        assert!(verified_by_proof.estimated);
        assert!(verified_by_proof.verified);

        let verified_by_usage = context_confirmation(
            &TokenUsage {
                input_tokens: 391_000,
                output_tokens: 12,
                total_tokens: 391_012,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            380_000,
            false,
        );
        assert_eq!(verified_by_usage.tokens, 391_000);
        assert!(!verified_by_usage.estimated);
        assert!(verified_by_usage.verified);
    }

    #[test]
    fn pressure_prompt_requires_all_distributed_markers() {
        let prompt = context_pressure_prompt(380_000);
        assert_eq!(prompt.proof_markers.len(), 5);
        for marker in &prompt.proof_markers {
            assert!(prompt.text.contains(marker));
        }
        let complete_reply = format!("NR_CONTEXT_OK {}", prompt.proof_markers.join(" "));
        assert!(context_pressure_reply_verified(
            &complete_reply,
            &prompt.proof_markers
        ));
        let partial_reply = format!("NR_CONTEXT_OK {}", prompt.proof_markers[0]);
        assert!(!context_pressure_reply_verified(
            &partial_reply,
            &prompt.proof_markers
        ));
    }

    #[test]
    fn pressure_anthropic_one_million_uses_beta_without_suffix() {
        let (url, headers, body) = pressure_test_request_shape(
            &ProviderProtocol::AnthropicMessages,
            &ProviderKind::Custom,
            "https://relay.example/v1",
            vec![("anthropic-beta".into(), "oauth-2025-04-20".into())],
            "claude-opus-4-8[1m]",
            258_000,
            true,
            "large prompt".into(),
        );

        assert_eq!(body["model"], "claude-opus-4-8");
        assert!(url.ends_with("/messages?beta=true"));
        let beta = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
            .map(|(_, value)| value.as_str())
            .unwrap();
        assert!(!beta.contains("oauth-2025-04-20"));
        assert!(beta.contains("context-1m-2025-08-07"));
        assert!(beta.contains("mid-conversation-system-2026-04-07"));
        assert!(beta.contains("effort-2025-11-24"));
        assert!(!beta.contains("fallback-credit-2026-06-01"));
        // 探测显式关闭思考与 effort，让小输出预算直接产出正文（否则回复为空）。
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("context_management").is_none());
    }

    #[test]
    fn context_error_classifier_avoids_infrastructure_errors() {
        assert!(is_explicit_context_limit_error(
            400,
            "context_length_exceeded: maximum context length"
        ));
        assert!(is_explicit_context_limit_error(
            400,
            "Context window is full. Reduce conversation history."
        ));
        assert!(!is_explicit_context_limit_error(
            502,
            "upstream request failed"
        ));
        assert!(!is_explicit_context_limit_error(429, "rate limit exceeded"));
        assert!(!is_explicit_context_limit_error(
            400,
            "Input content length exceeds threshold"
        ));
    }

    #[test]
    fn pressure_test_threshold_uses_ninety_five_percent() {
        assert_eq!(ModelTestMode::Context400K.pass_threshold_tokens(), 380_000);
        assert_eq!(ModelTestMode::Context1M.pass_threshold_tokens(), 950_000);
    }

    #[test]
    fn codex_restart_action_reflects_previous_running_state() {
        assert_eq!(codex_restart_action(false), "started");
        assert_eq!(codex_restart_action(true), "restarted");
    }

    #[test]
    fn server_restart_is_required_only_for_bind_address_changes() {
        let mut previous = seeded_config().settings;
        let mut next = previous.clone();

        next.allow_lan = !previous.allow_lan;
        assert!(!server_bind_settings_changed(&previous, &next));

        next.bind_host = "0.0.0.0".into();
        assert!(server_bind_settings_changed(&previous, &next));

        previous.bind_host = next.bind_host.clone();
        next.port += 1;
        assert!(server_bind_settings_changed(&previous, &next));
    }

    #[test]
    fn windows_codex_launch_paths_are_deduped_case_insensitively() {
        let paths = dedupe_paths_case_insensitive(vec![
            PathBuf::from(r"C:\Users\zoe\AppData\Local\Programs\Codex\Codex.exe"),
            PathBuf::from(r"c:\users\zoe\appdata\local\programs\codex\codex.exe"),
            PathBuf::from(r"C:\Users\zoe\AppData\Local\Programs\Codex\OpenAI Codex.exe"),
        ]);

        assert_eq!(paths.len(), 2);
        assert_eq!(
            paths[0],
            PathBuf::from(r"C:\Users\zoe\AppData\Local\Programs\Codex\Codex.exe")
        );
    }

    #[test]
    fn windows_codex_launch_paths_prefer_running_process_path() {
        let paths = merge_windows_codex_launch_paths(
            vec![PathBuf::from(r"D:\Apps\Codex\Codex.exe")],
            vec![PathBuf::from(r"C:\Registry\Codex.exe")],
            vec![PathBuf::from(r"C:\Path\Codex.exe")],
            vec![PathBuf::from(r"C:\Common\Codex.exe")],
            vec![PathBuf::from(r"C:\Start Menu\Codex.lnk")],
            vec![PathBuf::from(r"C:\Scanned\Codex.exe")],
        );

        assert_eq!(paths[0], PathBuf::from(r"D:\Apps\Codex\Codex.exe"));
        assert_eq!(paths[1], PathBuf::from(r"C:\Registry\Codex.exe"));
    }

    #[test]
    fn windows_codex_restart_refuses_to_stop_without_start_target() {
        let error = windows_restart_preflight_error(true, 0).unwrap();

        assert!(error.contains("left running"));
        assert!(windows_restart_preflight_error(true, 1).is_none());
        assert!(windows_restart_preflight_error(false, 0).is_none());
    }

    #[test]
    fn official_openai_test_headers_match_codex_oauth() {
        let headers = openai_official_test_headers(vec![
            ("authorization".into(), "Bearer token".into()),
            ("chatgpt-account-id".into(), "acct".into()),
            (
                "accept".into(),
                "text/event-stream, application/json".into(),
            ),
            ("sec-fetch-mode".into(), "no-cors".into()),
            ("openai-beta".into(), "responses=experimental".into()),
        ]);

        assert!(headers.contains(&("authorization".into(), "Bearer token".into())));
        assert!(headers.contains(&("chatgpt-account-id".into(), "acct".into())));
        assert!(headers.contains(&("originator".into(), "codex_cli_rs".into())));
        assert!(headers
            .iter()
            .any(|(name, value)| { name == "user-agent" && value.starts_with("codex_cli_rs/") }));
        assert!(headers.contains(&("content-type".into(), "application/json".into())));
        assert!(headers.contains(&(
            "accept".into(),
            "text/event-stream, application/json".into()
        )));
        assert!(headers.contains(&("accept-encoding".into(), "identity".into())));
        assert!(!headers.iter().any(|(name, _)| name == "sec-fetch-mode"));
    }

    #[test]
    fn official_openai_providers_use_builtin_models() {
        let fixed = Provider {
            id: "openai-official".into(),
            name: "OpenAI Official Account".into(),
            kind: ProviderKind::OfficialOpenAi,
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            key_ref: None,
            http_proxy: Default::default(),
        };
        let added = Provider {
            id: "openai-account".into(),
            name: "OpenAI Account".into(),
            kind: ProviderKind::OfficialOpenAiAccount,
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            key_ref: Some("official-token:openai-account".into()),
            http_proxy: Default::default(),
        };

        let fixed_models = openai_official_upstream_models(&fixed).unwrap();
        let added_models = openai_official_upstream_models(&added).unwrap();

        assert_eq!(fixed_models[0].id, "gpt-5.5");
        assert_eq!(added_models[0].id, "gpt-5.5");
        assert_eq!(fixed_models.len(), added_models.len());
    }

    #[test]
    fn non_json_test_error_returns_raw_body() {
        let raw = "upstream rejected this request";
        let value = test_response_value(raw);

        assert_eq!(provider_error_message(&value, raw), raw);
    }

    #[test]
    fn detail_error_returns_clean_message() {
        let raw = r#"{"detail":"Unsupported content type"}"#;
        let value = test_response_value(raw);

        assert_eq!(
            provider_error_message(&value, raw),
            "Unsupported content type"
        );
    }

    #[test]
    fn provider_status_error_includes_response_body_summary() {
        let error = provider_status_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"bad model list"}}"#,
        );

        assert_eq!(error, "Provider returned 400: bad model list");
    }

    #[test]
    fn sse_test_response_reads_completed_response() {
        let raw = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"OK\"}}\n\n"
        );
        let value = test_response_value(raw);

        assert_eq!(value["output_text"], "OK");
    }

    #[test]
    fn sse_test_response_keeps_delta_text_when_completed_has_only_usage() {
        let raw = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"O\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"delta\":\"K\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":15,\"output_tokens\":26,\"total_tokens\":41}}}\n\n"
        );
        let value = test_response_value(raw);

        assert_eq!(value["output_text"], "OK");
        assert_eq!(value["usage"]["total_tokens"], 41);
        assert_eq!(
            test_reply_from_value(&ProviderProtocol::OpenAiResponses, &value),
            "OK"
        );
    }

    #[test]
    fn sse_test_response_prefers_completed_output_text() {
        let raw = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"final\"}}\n\n"
        );
        let value = test_response_value(raw);

        assert_eq!(value["output_text"], "final");
    }

    #[test]
    fn sse_test_response_does_not_duplicate_output_text_done() {
        let raw = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n",
            "event: response.output_text.done\n",
            "data: {\"type\":\"response.output_text.done\",\"text\":\"OK\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":15,\"output_tokens\":25,\"total_tokens\":40}}}\n\n"
        );
        let value = test_response_value(raw);

        assert_eq!(value["output_text"], "OK");
    }

    #[test]
    fn sse_test_response_reads_content_part_done_text() {
        let raw = concat!(
            "event: response.content_part.done\n",
            "data: {\"type\":\"response.content_part.done\",\"part\":{\"type\":\"output_text\",\"text\":\"OK\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n"
        );
        let value = test_response_value(raw);

        assert_eq!(value["output_text"], "OK");
    }

    #[test]
    fn sse_test_response_does_not_duplicate_content_part_done() {
        let raw = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"OK\"}\n\n",
            "event: response.content_part.done\n",
            "data: {\"type\":\"response.content_part.done\",\"part\":{\"type\":\"output_text\",\"text\":\"OK\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":15,\"output_tokens\":25,\"total_tokens\":40}}}\n\n"
        );
        let value = test_response_value(raw);

        assert_eq!(value["output_text"], "OK");
    }

    #[test]
    fn sse_test_response_without_text_stays_empty() {
        let raw = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0,\"total_tokens\":1}}}\n\n"
        );
        let value = test_response_value(raw);

        assert_eq!(
            test_reply_from_value(&ProviderProtocol::OpenAiResponses, &value),
            ""
        );
    }

    #[test]
    fn json_test_response_reads_output_text_and_output_items() {
        let output_text = test_response_value(r#"{"output_text":"OK"}"#);
        assert_eq!(
            test_reply_from_value(&ProviderProtocol::OpenAiResponses, &output_text),
            "OK"
        );

        let output_item = test_response_value(
            r#"{"output":[{"content":[{"type":"output_text","text":"from output"}]}]}"#,
        );
        assert_eq!(
            test_reply_from_value(&ProviderProtocol::OpenAiResponses, &output_item),
            "from output"
        );
    }

    #[test]
    fn json_test_response_reads_chat_completions_content_shapes() {
        let string_content = test_response_value(
            r#"{"choices":[{"message":{"content":"OK"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
        );
        assert_eq!(
            test_reply_from_value(&ProviderProtocol::OpenAiChatCompletions, &string_content),
            "OK"
        );

        let array_content = test_response_value(
            r#"{"choices":[{"message":{"content":[{"type":"text","text":"O"},{"type":"text","text":"K"}]}}]}"#,
        );
        assert_eq!(
            test_reply_from_value(&ProviderProtocol::OpenAiChatCompletions, &array_content),
            "OK"
        );
    }

    #[test]
    fn json_test_response_uses_chat_reasoning_when_content_is_empty() {
        let value = test_response_value(
            r#"{"choices":[{"message":{"content":null,"reasoning_content":"NR_CONTEXT_OK marker"}}]}"#,
        );

        assert_eq!(
            test_reply_from_value(&ProviderProtocol::OpenAiChatCompletions, &value),
            "NR_CONTEXT_OK marker"
        );
    }

    #[test]
    fn sse_test_response_reads_chat_completions_delta_content() {
        let raw = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"O\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"K\"}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        );
        let value = test_response_value(raw);

        assert_eq!(
            test_reply_from_value(&ProviderProtocol::OpenAiChatCompletions, &value),
            "OK"
        );
        assert_eq!(value["usage"]["total_tokens"], 3);
    }

    #[test]
    fn sse_test_response_reads_chat_reasoning_when_content_is_empty() {
        let raw = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"NR_CONTEXT_\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"OK\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let value = test_response_value(raw);

        assert_eq!(
            test_reply_from_value(&ProviderProtocol::OpenAiChatCompletions, &value),
            "NR_CONTEXT_OK"
        );
    }
}

fn show_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(true);

    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn hide_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }

    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(false);
}

fn configure_native_window_frame(window: &tauri::WebviewWindow) {
    configure_platform_window_frame(window);
}

#[cfg(target_os = "macos")]
fn configure_platform_window_frame(window: &tauri::WebviewWindow) {
    let _ = window.set_decorations(true);
    let _ = window.set_title_bar_style(tauri::TitleBarStyle::Overlay);
    let _ = window.set_shadow(true);
}

#[cfg(target_os = "windows")]
fn configure_platform_window_frame(window: &tauri::WebviewWindow) {
    use std::mem::size_of;
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE,
    };

    let _ = window.set_decorations(false);
    let _ = window.set_shadow(true);
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let preference: u32 = 2;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd.0 as _,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &preference as *const _ as *const std::ffi::c_void,
            size_of::<u32>() as u32,
        );
    }
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn configure_platform_window_frame(window: &tauri::WebviewWindow) {
    let _ = window.set_decorations(true);
    let _ = window.set_shadow(true);
}

fn setup_tray(app: &mut tauri::App, exit_requested: Arc<AtomicBool>) -> tauri::Result<()> {
    let menu = MenuBuilder::new(app)
        .text("show_main", "显示主窗口")
        .text("quit", "退出")
        .build()?;

    let menu_exit_requested = exit_requested.clone();
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Neko Route")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "show_main" => show_main_window(app),
            "quit" => {
                menu_exit_requested.store(true, Ordering::SeqCst);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    Ok(())
}

pub fn run() {
    let exit_requested = Arc::new(AtomicBool::new(false));
    let setup_exit_requested = exit_requested.clone();
    let close_exit_requested = exit_requested.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            setup_tray(app, setup_exit_requested.clone())?;
            if let Some(window) = app.get_webview_window("main") {
                configure_native_window_frame(&window);
            }
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| format!("Failed to resolve app data dir: {error}"))?;
            let store = AppStore::load(app_data_dir)?;
            let catalog_store = store.clone();
            let server_runtime = ServerRuntime::new(store.clone());
            app.manage(store);
            app.manage(server_runtime.clone());
            app.manage(OpenAiOAuthSessions::default());
            app.manage(ClaudeOAuthSessions::default());
            app.manage(ModelTestSessions::default());
            tauri::async_runtime::spawn(async move {
                let codex_home = codex_config::resolve_codex_home();
                let _ = catalog_store.export_catalog_to(&codex_home).await;
            });
            tauri::async_runtime::spawn(async move {
                server_runtime.start().await;
            });
            Ok(())
        })
        .on_window_event(move |window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if !close_exit_requested.load(Ordering::SeqCst) {
                    api.prevent_close();
                    hide_main_window(window.app_handle());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            save_config,
            regenerate_lan_api_key,
            list_lan_models,
            set_provider_key,
            delete_provider_key,
            read_provider_proxy_password,
            set_provider_proxy_password,
            delete_provider_proxy_password,
            set_official_provider_token,
            start_openai_oauth,
            finish_openai_oauth,
            start_claude_oauth,
            finish_claude_oauth,
            finish_claude_cookie_oauth,
            delete_official_provider_token,
            refresh_official_provider_token,
            read_provider_credential,
            codex_app_status,
            restart_codex_app,
            test_route,
            test_model,
            start_model_test,
            get_model_test_status,
            cancel_model_test,
            list_upstream_models,
            model_health,
            refresh_provider_usage,
            export_catalog,
            install_codex_config,
            restore_codex_config,
            read_codex_config,
            save_codex_config,
            import_sessions,
            clear_request_logs,
            read_image_preview,
            get_request_logs
        ])
        .run(tauri::generate_context!())
        .expect("error while running Neko Route");
}
