use crate::{
    catalog, claude_auth, lan_share, official_auth, official_usage, provider_proxy,
    redact::redact,
    router::{match_route, RouteMatch},
    store::{validate_bind_settings, AppStore},
    types::{
        ClaudeContextPressureSample, CodexInjectionMode, ContextArtifact, ContextBridgeDiagnostics,
        ModelEntry, Provider, ProviderKind, ProviderProtocol, RequestRecord, Settings, TokenUsage,
    },
    usage::{parse_usage, usage_from_responses_text},
};
use axum::{
    body::Body,
    extract::{connect_info::ConnectInfo, DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
use bytes::Bytes;
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    convert::Infallible,
    future::Future,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, time::sleep};
use uuid::Uuid;

const RESPONSES_BODY_LIMIT_BYTES: usize = 64 * 1024 * 1024;
const ANTHROPIC_ONE_MILLION_CONTEXT_WINDOW: u64 = 1_000_000;
const ANTHROPIC_ONE_MILLION_SUFFIX: &str = "[1m]";
const ANTHROPIC_VERSION: &str = "2023-06-01";
pub(crate) const CLAUDE_DESKTOP_MESSAGES_BETA: &str = "claude-code-20250219,context-1m-2025-08-07,interleaved-thinking-2025-05-14,mid-conversation-system-2026-04-07,effort-2025-11-24";
pub(crate) const CLAUDE_DESKTOP_COUNT_TOKENS_BETA: &str =
    "claude-code-20250219,interleaved-thinking-2025-05-14,token-counting-2024-11-01";
const CLAUDE_DESKTOP_USER_AGENT: &str =
    "claude-cli/2.1.170 (external, claude-desktop-3p, agent-sdk/0.3.170)";
const CLAUDE_CODE_STAINLESS_PACKAGE_VERSION: &str = "0.94.0";
const CLAUDE_CODE_STAINLESS_RUNTIME_VERSION: &str = "v24.3.0";
const CLAUDE_DESKTOP_STAINLESS_TIMEOUT: &str = "900";
const CLAUDE_DESKTOP_BILLING_HEADER: &str =
    "x-anthropic-billing-header: cc_version=2.1.170.e4c; cc_entrypoint=claude-desktop-3p; cch=52d85;";
const CLAUDE_DESKTOP_IDENTITY: &str =
    "You are Claude Code, Anthropic's official CLI for Claude, running within the Claude Agent SDK.";
const ANTHROPIC_THINKING_PREFIX: &str = "neko-route-anthropic-thinking:v1:";
const CONTEXT_BRIDGE_KEEP_TOOL_RESULTS: usize = 12;
const CONTEXT_BRIDGE_FALLBACK_KEEP_TOOL_RESULTS: usize = 6;
const CONTEXT_BRIDGE_TOKEN_TARGET_PERCENT: u64 = 88;
const CONTEXT_BRIDGE_RECALL_LIMIT: usize = 4;
const CONTEXT_BRIDGE_PREVIEW_CHARS: usize = 80;
const CONTEXT_BRIDGE_PRECOMPRESSION_THRESHOLD_PERCENT: u64 = 80;

#[derive(Clone)]
struct ServerState {
    store: AppStore,
    client: Client,
}

#[derive(Debug)]
struct RouteError {
    status: StatusCode,
    code: &'static str,
    message: String,
    record_model: Option<String>,
    requested_model: Option<String>,
    route_reason: Option<String>,
    provider_id: Option<String>,
    provider_name: Option<String>,
    provider_protocol: Option<ProviderProtocol>,
    context_bridge: Option<ContextBridgeDiagnostics>,
}

impl RouteError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: redact(&message.into()),
            record_model: None,
            requested_model: None,
            route_reason: None,
            provider_id: None,
            provider_name: None,
            provider_protocol: None,
            context_bridge: None,
        }
    }

    fn with_match(mut self, matched: &RouteMatch) -> Self {
        self.record_model = Some(matched.model.id.clone());
        self.requested_model =
            (matched.model.id != matched.requested_model).then(|| matched.requested_model.clone());
        self.route_reason = Some(matched.route_reason.clone());
        self.provider_id = Some(matched.provider.id.clone());
        self.provider_name = Some(matched.provider.name.clone());
        self.provider_protocol = Some(matched.provider.protocol.clone());
        self
    }

    fn with_context_bridge(mut self, context_bridge: ContextBridgeDiagnostics) -> Self {
        self.context_bridge = Some(context_bridge);
        self
    }

    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": {
                    "message": self.message,
                    "type": self.code,
                    "code": self.code
                }
            })),
        )
            .into_response()
    }
}

pub async fn run_with_shutdown<F>(store: AppStore, shutdown: F) -> Result<(), String>
where
    F: Future<Output = ()> + Send + 'static,
{
    let config = store.config().await;
    validate_bind_settings(&config)?;
    let addr: SocketAddr = format!("{}:{}", config.settings.bind_host, config.settings.port)
        .parse()
        .map_err(|error| format!("Invalid bind address: {error}"))?;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|error| error.to_string())?;
    let local_addr = listener.local_addr().map_err(|error| error.to_string())?;
    let bind_url = format!("http://{}/v1", local_addr);
    store.set_server_running(bind_url).await;

    let state = ServerState {
        store,
        client: Client::builder()
            .user_agent("NekoRoute/0.1")
            .build()
            .map_err(|error| error.to_string())?,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route(
            "/v1/responses",
            post(responses).layer(DefaultBodyLimit::max(RESPONSES_BODY_LIMIT_BYTES)),
        )
        .with_state(state);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(|error| error.to_string())
}

async fn health(State(state): State<ServerState>) -> Json<Value> {
    let config = state.store.config().await;
    let keys = state.store.key_statuses(&config);
    Json(json!({
        "ok": true,
        "service": "neko-route",
        "version": env!("CARGO_PKG_VERSION"),
        "config_version": config.version,
        "codex_slot_count": config.settings.codex_slots.len(),
        "providers": config.providers.len(),
        "models": config.models.iter().filter(|model| model.enabled).count(),
        "keys": keys,
    }))
}

async fn ensure_lan_request_authorized(
    state: &ServerState,
    remote_addr: SocketAddr,
    headers: &HeaderMap,
) -> Result<(), RouteError> {
    let config = state.store.config().await;
    validate_lan_authorization(&config.settings, remote_addr, headers)
}

fn validate_lan_authorization(
    settings: &Settings,
    remote_addr: SocketAddr,
    headers: &HeaderMap,
) -> Result<(), RouteError> {
    if !settings.allow_lan || remote_addr.ip().is_loopback() {
        return Ok(());
    }
    let expected = lan_share::bearer_value(&settings.lan_api_key).map_err(|message| {
        RouteError::new(
            StatusCode::FAILED_DEPENDENCY,
            "lan_api_key_missing",
            message,
        )
    })?;
    let actual = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if actual == expected {
        Ok(())
    } else {
        Err(RouteError::new(
            StatusCode::UNAUTHORIZED,
            "lan_auth_required",
            "LAN access requires a valid Neko Route API key",
        ))
    }
}

async fn proxy_lan_models(state: &ServerState, settings: &Settings) -> Response {
    if let Err(error) = lan_share::remote_base_url(settings) {
        return RouteError::new(StatusCode::BAD_REQUEST, "lan_remote_invalid", error)
            .into_response();
    }
    if let Err(error) = lan_share::bearer_value(&settings.lan_remote_api_key) {
        return RouteError::new(StatusCode::BAD_REQUEST, "lan_remote_invalid", error)
            .into_response();
    }
    match state.store.lan_codex_catalog_models().await {
        Ok(models) => models_response(models),
        Err(error) => RouteError::new(
            StatusCode::BAD_GATEWAY,
            "lan_remote_unavailable",
            format!("LAN remote models could not be loaded: {error}"),
        )
        .into_response(),
    }
}

fn models_response(models: Vec<catalog::CatalogModel>) -> Response {
    Json(json!({
        "object": "list",
        "data": models
            .into_iter()
            .map(|model| {
                json!({
                    "id": model.slug,
                    "object": "model",
                    "created": 0,
                    "owned_by": "neko-route",
                    "display_name": model.display_name,
                    "description": model.description,
                    "context_window": model.context_window,
                    "max_context_window": model.context_window,
                    "supports_reasoning_summaries": model.reasoning_enabled,
                    "default_reasoning_level": model.default_reasoning_level,
                    "supported_reasoning_levels": model.supported_reasoning_levels,
                    "provider_protocol": model.provider_protocol,
                })
            })
            .collect::<Vec<_>>()
    }))
    .into_response()
}

async fn models(
    State(state): State<ServerState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = ensure_lan_request_authorized(&state, remote_addr, &headers).await {
        return error.into_response();
    }
    let config = state.store.config().await;
    if config.settings.codex_injection_mode == CodexInjectionMode::LanShare {
        return proxy_lan_models(&state, &config.settings).await;
    }
    let allowed_model_ids = match state.store.codex_allowed_model_ids(&config) {
        Ok(value) => value,
        Err(error) => {
            return RouteError::new(StatusCode::INTERNAL_SERVER_ERROR, "catalog_failed", error)
                .into_response();
        }
    };
    let models = match catalog::catalog_models_for_config(&config, allowed_model_ids.as_ref()) {
        Ok(models) => models,
        Err(error) => {
            return RouteError::new(StatusCode::INTERNAL_SERVER_ERROR, "catalog_failed", error)
                .into_response();
        }
    };
    models_response(models)
}

async fn responses(
    State(state): State<ServerState>,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body_bytes: Bytes,
) -> Response {
    let started = std::time::Instant::now();
    let request_id = Uuid::new_v4().to_string();
    if let Err(error) = ensure_lan_request_authorized(&state, remote_addr, &headers).await {
        return error.into_response();
    }

    let parsed_body = match serde_json::from_slice::<Value>(&body_bytes) {
        Ok(value) => value,
        Err(error) => {
            let route_error = RouteError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("Invalid JSON request body: {error}"),
            );
            let status = route_error.status.as_u16();
            let message = route_error.message.clone();
            state
                .store
                .push_request(RequestRecord {
                    id: request_id,
                    started_at: Utc::now(),
                    model: String::new(),
                    requested_model: None,
                    route_reason: None,
                    provider_id: None,
                    provider_name: None,
                    provider_protocol: None,
                    status,
                    latency_ms: started.elapsed().as_millis(),
                    streaming: false,
                    error: Some(message),
                    reasoning_effort: None,
                    stream_state: None,
                    stream_error: None,
                    last_event: None,
                    stream_bytes: 0,
                    context_bridge: None,
                    usage: TokenUsage::default(),
                })
                .await;
            return route_error.into_response();
        }
    };
    let model = parsed_body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let streaming = parsed_body
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let reasoning_effort = map_reasoning_effort(&parsed_body).map(str::to_string);

    let result = route_response(
        state.clone(),
        headers,
        parsed_body,
        body_bytes,
        request_id.clone(),
    )
    .await;
    let (
        status,
        record_model,
        requested_model,
        route_reason,
        provider_id,
        provider_name,
        provider_protocol,
        context_bridge,
        error,
        usage,
    ) = match &result {
        Ok((response, matched, usage, context_bridge)) => (
            response.status().as_u16(),
            matched.model.id.clone(),
            (matched.model.id != matched.requested_model).then(|| matched.requested_model.clone()),
            Some(matched.route_reason.clone()),
            Some(matched.provider.id.clone()),
            Some(matched.provider.name.clone()),
            Some(matched.provider.protocol.clone()),
            context_bridge.clone(),
            None,
            usage.unwrap_or_default(),
        ),
        Err(error) => (
            error.status.as_u16(),
            error.record_model.clone().unwrap_or_else(|| model.clone()),
            error.requested_model.clone(),
            error.route_reason.clone(),
            error.provider_id.clone(),
            error.provider_name.clone(),
            error.provider_protocol.clone(),
            error.context_bridge.clone(),
            Some(error.message.clone()),
            TokenUsage::default(),
        ),
    };
    let stream_state = initial_stream_state(status, provider_protocol.as_ref(), streaming);

    state
        .store
        .push_request(RequestRecord {
            id: request_id,
            started_at: Utc::now(),
            model: record_model,
            requested_model,
            route_reason,
            provider_id,
            provider_name,
            provider_protocol,
            status,
            latency_ms: started.elapsed().as_millis(),
            streaming,
            error,
            reasoning_effort,
            stream_state,
            stream_error: None,
            last_event: None,
            stream_bytes: 0,
            context_bridge,
            usage,
        })
        .await;

    match result {
        Ok((response, _, _, _)) => response,
        Err(error) => error.into_response(),
    }
}

async fn route_response(
    state: ServerState,
    headers: HeaderMap,
    mut body: Value,
    body_bytes: Bytes,
    request_id: String,
) -> Result<
    (
        Response,
        RouteMatch,
        Option<TokenUsage>,
        Option<ContextBridgeDiagnostics>,
    ),
    RouteError,
> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RouteError::new(StatusCode::BAD_REQUEST, "invalid_request", "Missing model")
        })?
        .to_string();
    let config = state.store.config().await;
    if config.settings.codex_injection_mode == CodexInjectionMode::LanShare {
        let lan_model = state
            .store
            .resolve_lan_codex_model(&model)
            .await
            .map_err(|error| {
                RouteError::new(StatusCode::NOT_FOUND, "lan_model_not_mapped", error)
            })?;
        let matched = lan_route_match(&config.settings, &model, &lan_model);
        let response = forward_lan_responses(
            &state,
            &headers,
            body,
            &request_id,
            lan_model.real_target_model_id(),
        )
        .await
        .map_err(|error| error.with_match(&matched))?;
        return Ok((response, matched, None, None));
    }
    let matched = match_route(&config, &model).map_err(|message| {
        RouteError::new(StatusCode::NOT_FOUND, "model_not_configured", message)
    })?;
    let response_result: Result<
        (
            Response,
            Option<TokenUsage>,
            Option<ContextBridgeDiagnostics>,
        ),
        RouteError,
    > = match &matched.provider.kind {
        ProviderKind::OfficialOpenAi => forward_responses_proxy(
            &state,
            &headers,
            &matched,
            body,
            body_bytes,
            &model,
            ResponsesAuthMode::CodexOfficial,
            request_id,
        )
        .await
        .map(|(response, usage)| (response, usage, None)),
        ProviderKind::OfficialOpenAiAccount => forward_responses_proxy(
            &state,
            &headers,
            &matched,
            body,
            body_bytes,
            &model,
            ResponsesAuthMode::StoredOfficialAccount,
            request_id,
        )
        .await
        .map(|(response, usage)| (response, usage, None)),
        ProviderKind::OfficialAnthropicCli
        | ProviderKind::OfficialAnthropicDesktop
        | ProviderKind::OfficialAnthropicAccount => {
            body["model"] = Value::String(matched.upstream_model.clone());
            forward_anthropic(&state, &matched, body, request_id, &model).await
        }
        ProviderKind::Custom => match &matched.provider.protocol {
            ProviderProtocol::OpenAiResponses => forward_responses_proxy(
                &state,
                &headers,
                &matched,
                body,
                body_bytes,
                &model,
                ResponsesAuthMode::ProviderKey,
                request_id,
            )
            .await
            .map(|(response, usage)| (response, usage, None)),
            ProviderProtocol::OpenAiChatCompletions => {
                body["model"] = Value::String(matched.upstream_model.clone());
                forward_chat_completions(&state, &matched, body, request_id, &model)
                    .await
                    .map(|(response, usage)| (response, usage, None))
            }
            ProviderProtocol::AnthropicMessages => {
                body["model"] = Value::String(matched.upstream_model.clone());
                forward_anthropic(&state, &matched, body, request_id, &model).await
            }
        },
    };
    let (response, usage, context_bridge) =
        response_result.map_err(|error| error.with_match(&matched))?;

    Ok((response, matched, usage, context_bridge))
}

fn lan_route_match(
    settings: &Settings,
    requested_model: &str,
    lan_model: &catalog::CatalogModel,
) -> RouteMatch {
    let target_model = lan_model.real_target_model_id().to_string();
    RouteMatch {
        model: ModelEntry {
            id: target_model.clone(),
            display_name: lan_model.display_name.clone(),
            description: lan_model.description.clone(),
            context_window: lan_model.context_window,
            enabled: true,
            provider_id: "lan-share".into(),
            upstream_model: None,
            timeout_ms: 0,
            retry_count: 0,
            reasoning_enabled: lan_model.reasoning_enabled,
            default_reasoning_level: lan_model.default_reasoning_level.clone(),
            supported_reasoning_levels: lan_model.supported_reasoning_levels.clone(),
            codex_alias: None,
        },
        provider: Provider {
            id: "lan-share".into(),
            name: format!(
                "LAN Share {}:{}",
                settings.lan_remote_host.trim(),
                settings.lan_remote_port
            ),
            kind: ProviderKind::Custom,
            protocol: lan_model
                .provider_protocol
                .clone()
                .unwrap_or(ProviderProtocol::OpenAiResponses),
            base_url: lan_share::remote_base_url(settings).unwrap_or_default(),
            key_ref: None,
            http_proxy: Default::default(),
        },
        upstream_model: target_model,
        timeout_ms: 0,
        retry_count: 0,
        requested_model: requested_model.to_string(),
        route_reason: "lan_share".into(),
        locked_from_model: None,
    }
}

async fn forward_lan_responses(
    state: &ServerState,
    inbound_headers: &HeaderMap,
    mut body: Value,
    request_id: &str,
    target_model: &str,
) -> Result<Response, RouteError> {
    let config = state.store.config().await;
    let base_url = lan_share::remote_base_url(&config.settings)
        .map_err(|error| RouteError::new(StatusCode::BAD_REQUEST, "lan_remote_invalid", error))?;
    let bearer = lan_share::bearer_value(&config.settings.lan_remote_api_key)
        .map_err(|error| RouteError::new(StatusCode::BAD_REQUEST, "lan_remote_invalid", error))?;
    let mut headers = lan_proxy_request_headers(inbound_headers);
    set_proxy_header(&mut headers, "authorization", &bearer);
    set_proxy_header(&mut headers, "content-type", "application/json");
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("accept"))
    {
        headers.push((
            "accept".to_string(),
            "text/event-stream, application/json".to_string(),
        ));
    }
    let url = format!("{base_url}/responses");
    body["model"] = Value::String(target_model.to_string());
    let body_bytes = serde_json::to_vec(&body)
        .map(Bytes::from)
        .map_err(|error| {
            RouteError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "lan_proxy_body_failed",
                format!("Could not prepare LAN request body: {error}"),
            )
        })?;
    let upstream = post_bytes_with_retries(&state.client, &url, headers, body_bytes, 0, 0).await?;
    let streaming = is_event_stream(upstream.headers());
    let context = RawProxyContext::from_response(&upstream, request_id.to_string(), streaming);
    Ok(proxy_raw(upstream, state.store.clone(), context))
}

fn lan_proxy_request_headers(inbound_headers: &HeaderMap) -> Vec<(String, String)> {
    inbound_headers
        .iter()
        .filter_map(|(name, value)| {
            if should_skip_proxy_request_header(name.as_str()) {
                return None;
            }
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

fn initial_stream_state(
    status: u16,
    protocol: Option<&ProviderProtocol>,
    streaming: bool,
) -> Option<String> {
    if (200..300).contains(&status)
        && streaming
        && matches!(
            protocol,
            Some(
                ProviderProtocol::OpenAiResponses
                    | ProviderProtocol::OpenAiChatCompletions
                    | ProviderProtocol::AnthropicMessages
            )
        )
    {
        Some("pending".into())
    } else {
        None
    }
}

fn is_event_stream(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

fn client_for_provider(state: &ServerState, provider: &Provider) -> Result<Client, RouteError> {
    provider_proxy::client_for_provider(&state.client, state.store.key_store(), provider).map_err(
        |message| {
            RouteError::new(
                StatusCode::FAILED_DEPENDENCY,
                "provider_proxy_unavailable",
                message,
            )
        },
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResponsesAuthMode {
    CodexOfficial,
    StoredOfficialAccount,
    ProviderKey,
}

async fn forward_responses_proxy(
    state: &ServerState,
    inbound_headers: &HeaderMap,
    matched: &RouteMatch,
    body: Value,
    body_bytes: Bytes,
    requested_model: &str,
    auth_mode: ResponsesAuthMode,
    request_id: String,
) -> Result<(Response, Option<TokenUsage>), RouteError> {
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let upstream_body = responses_proxy_body(body, body_bytes, requested_model, matched)?;
    let url = responses_proxy_url(auth_mode, inbound_headers, &matched.provider)?;
    let client = client_for_provider(state, &matched.provider)?;
    let headers = proxy_request_headers(
        state,
        &client,
        inbound_headers,
        &matched.provider,
        auth_mode,
    )
    .await?;
    let upstream = post_bytes_with_retries(
        &client,
        &url,
        headers,
        upstream_body,
        matched.timeout_ms,
        matched.retry_count,
    )
    .await?;
    if matches!(
        matched.provider.kind,
        ProviderKind::OfficialOpenAi | ProviderKind::OfficialOpenAiAccount
    ) {
        if let Some(quota) = official_usage::quota_from_codex_headers(upstream.headers()) {
            state
                .store
                .update_provider_usage_snapshot(
                    matched.provider.id.clone(),
                    "passive".into(),
                    Some(quota),
                    None,
                )
                .await;
        }
    }
    let context = RawProxyContext::from_response(&upstream, request_id.clone(), streaming);
    // Tap the passthrough stream to record token usage once it finishes,
    // without buffering the whole body in memory.
    Ok((proxy_raw(upstream, state.store.clone(), context), None))
}

fn responses_proxy_body(
    mut body: Value,
    original_bytes: Bytes,
    requested_model: &str,
    matched: &RouteMatch,
) -> Result<Bytes, RouteError> {
    if matched.upstream_model == requested_model {
        return Ok(original_bytes);
    }

    body["model"] = Value::String(matched.upstream_model.clone());
    serde_json::to_vec(&body).map(Bytes::from).map_err(|error| {
        RouteError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "proxy_body_failed",
            error.to_string(),
        )
    })
}

fn official_responses_endpoint(
    inbound_headers: &HeaderMap,
    provider: &Provider,
) -> Result<String, RouteError> {
    let auth = inbound_headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            RouteError::new(
                StatusCode::UNAUTHORIZED,
                "missing_openai_auth",
                "This model uses OpenAI Official Account, but Codex did not send Authorization",
            )
        })?;

    if is_public_openai_api_key(auth) {
        Ok(endpoint(&provider.base_url, "responses"))
    } else {
        Ok("https://chatgpt.com/backend-api/codex/responses".to_string())
    }
}

fn responses_proxy_url(
    auth_mode: ResponsesAuthMode,
    inbound_headers: &HeaderMap,
    provider: &Provider,
) -> Result<String, RouteError> {
    match auth_mode {
        ResponsesAuthMode::CodexOfficial => official_responses_endpoint(inbound_headers, provider),
        ResponsesAuthMode::StoredOfficialAccount => Ok(endpoint(
            official_auth::openai_codex_base_url(),
            "responses",
        )),
        ResponsesAuthMode::ProviderKey => Ok(endpoint(&provider.base_url, "responses")),
    }
}

async fn proxy_request_headers(
    state: &ServerState,
    client: &Client,
    inbound_headers: &HeaderMap,
    provider: &Provider,
    auth_mode: ResponsesAuthMode,
) -> Result<Vec<(String, String)>, RouteError> {
    let mut headers = Vec::new();
    let force_identity = matches!(
        auth_mode,
        ResponsesAuthMode::CodexOfficial | ResponsesAuthMode::StoredOfficialAccount
    );

    for (name, value) in inbound_headers {
        if force_identity && name.as_str().eq_ignore_ascii_case("accept-encoding") {
            continue;
        }
        if should_skip_proxy_request_header(name.as_str()) {
            continue;
        }
        if let Ok(value) = value.to_str() {
            headers.push((name.as_str().to_string(), value.to_string()));
        }
    }

    headers.push(("content-type".to_string(), "application/json".to_string()));
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("accept"))
    {
        headers.push((
            "accept".to_string(),
            "text/event-stream, application/json".to_string(),
        ));
    }

    if force_identity {
        set_proxy_header(&mut headers, "accept-encoding", "identity");
    }

    match auth_mode {
        ResponsesAuthMode::CodexOfficial => {
            let auth = inbound_headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    RouteError::new(
                        StatusCode::UNAUTHORIZED,
                        "missing_openai_auth",
                        "This model uses OpenAI Official Account, but Codex did not send Authorization",
                    )
                })?;
            headers.push(("authorization".to_string(), auth.to_string()));
            if !is_public_openai_api_key(auth)
                && !headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("openai-beta"))
            {
                headers.push((
                    "openai-beta".to_string(),
                    "responses_websockets=2026-02-06".to_string(),
                ));
            }
        }
        ResponsesAuthMode::StoredOfficialAccount => {
            let auth = official_auth::auth_for_provider(client, provider)
                .await
                .map_err(|message| {
                    RouteError::new(StatusCode::UNAUTHORIZED, "missing_official_auth", message)
                })?;
            for (name, value) in auth.headers {
                if name.eq_ignore_ascii_case("content-type") {
                    continue;
                }
                set_proxy_header(&mut headers, &name, &value);
            }
        }
        ResponsesAuthMode::ProviderKey => {
            if let Some(key_ref) = provider.key_ref.as_deref() {
                let secret = state
                    .store
                    .key_store()
                    .get_secret(key_ref)
                    .map_err(|message| {
                        RouteError::new(
                            StatusCode::FAILED_DEPENDENCY,
                            "key_store_unavailable",
                            message,
                        )
                    })?
                    .ok_or_else(|| {
                        RouteError::new(
                            StatusCode::UNAUTHORIZED,
                            "missing_provider_key",
                            format!(
                                "Provider '{}' needs an API key in local storage",
                                provider.name
                            ),
                        )
                    })?;
                headers.push(("authorization".to_string(), format!("Bearer {secret}")));
            }
        }
    }

    Ok(headers)
}

fn set_proxy_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
    headers.push((name.to_string(), value.to_string()));
}

pub(crate) fn anthropic_model_for_request(
    upstream_model: &str,
    context_window: u64,
) -> (String, bool) {
    let trimmed = upstream_model.trim();
    let lower = trimmed.to_ascii_lowercase();
    let has_suffix = lower.ends_with(ANTHROPIC_ONE_MILLION_SUFFIX);
    let model = if has_suffix {
        trimmed[..trimmed.len() - ANTHROPIC_ONE_MILLION_SUFFIX.len()]
            .trim_end()
            .to_string()
    } else {
        trimmed.to_string()
    };
    let model = if model.is_empty() {
        trimmed.to_string()
    } else {
        model
    };
    (
        model,
        has_suffix || context_window >= ANTHROPIC_ONE_MILLION_CONTEXT_WINDOW,
    )
}

pub(crate) fn anthropic_messages_url(base_url: &str, one_million_context: bool) -> String {
    let _ = one_million_context;
    append_beta_query(endpoint(base_url, "messages"))
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn claude_code_session_id(request: &Value) -> String {
    if let Some(session_id) = request
        .pointer("/metadata/session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        return session_id.to_string();
    }
    if let Some(user_id) = request
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        if let Ok(value) = serde_json::from_str::<Value>(user_id) {
            if let Some(session_id) = value
                .get("session_id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                return session_id.to_string();
            }
        }
    }
    static SESSION_ID: OnceLock<String> = OnceLock::new();
    SESSION_ID
        .get_or_init(|| Uuid::new_v4().to_string())
        .clone()
}

fn claude_code_stainless_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "arm" => "arm",
        value => value,
    }
}

fn claude_code_stainless_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "MacOS",
        "windows" => "Windows",
        "linux" => "Linux",
        value => value,
    }
}

#[derive(Clone, Copy)]
enum ClaudeDesktopRequestProfile {
    Messages,
    CountTokens,
}

pub(crate) fn claude_code_mirror_headers(
    base_headers: Vec<(String, String)>,
    request: &Value,
) -> Vec<(String, String)> {
    claude_desktop_mirror_headers(base_headers, request, ClaudeDesktopRequestProfile::Messages)
}

fn claude_code_count_tokens_headers(
    base_headers: Vec<(String, String)>,
    request: &Value,
) -> Vec<(String, String)> {
    claude_desktop_mirror_headers(
        base_headers,
        request,
        ClaudeDesktopRequestProfile::CountTokens,
    )
}

fn claude_desktop_mirror_headers(
    base_headers: Vec<(String, String)>,
    request: &Value,
    profile: ClaudeDesktopRequestProfile,
) -> Vec<(String, String)> {
    let auth = header_value(&base_headers, "authorization")
        .or_else(|| header_value(&base_headers, "x-api-key").map(|key| format!("Bearer {key}")));
    let mut headers = Vec::new();
    headers.push(("accept".into(), "application/json".into()));
    if let Some(auth) = auth {
        headers.push(("authorization".into(), auth));
    }
    headers.push(("content-type".into(), "application/json".into()));
    headers.push(("user-agent".into(), CLAUDE_DESKTOP_USER_AGENT.into()));
    headers.push((
        "x-claude-code-session-id".into(),
        claude_code_session_id(request),
    ));
    headers.push((
        "x-stainless-arch".into(),
        claude_code_stainless_arch().into(),
    ));
    headers.push(("x-stainless-lang".into(), "js".into()));
    headers.push(("x-stainless-os".into(), claude_code_stainless_os().into()));
    headers.push((
        "x-stainless-package-version".into(),
        CLAUDE_CODE_STAINLESS_PACKAGE_VERSION.into(),
    ));
    headers.push(("x-stainless-retry-count".into(), "0".into()));
    headers.push(("x-stainless-runtime".into(), "node".into()));
    headers.push((
        "x-stainless-runtime-version".into(),
        CLAUDE_CODE_STAINLESS_RUNTIME_VERSION.into(),
    ));
    match profile {
        ClaudeDesktopRequestProfile::Messages => {
            headers.push((
                "x-stainless-timeout".into(),
                CLAUDE_DESKTOP_STAINLESS_TIMEOUT.into(),
            ));
            headers.push(("anthropic-beta".into(), CLAUDE_DESKTOP_MESSAGES_BETA.into()));
        }
        ClaudeDesktopRequestProfile::CountTokens => {
            headers.push((
                "anthropic-beta".into(),
                CLAUDE_DESKTOP_COUNT_TOKENS_BETA.into(),
            ));
        }
    }
    headers.push((
        "anthropic-dangerous-direct-browser-access".into(),
        "true".into(),
    ));
    headers.push(("anthropic-version".into(), ANTHROPIC_VERSION.into()));
    headers.push(("x-app".into(), "cli".into()));
    headers
}

fn append_beta_query(url: String) -> String {
    if url.contains('?') {
        format!("{url}&beta=true")
    } else {
        format!("{url}?beta=true")
    }
}

fn anthropic_count_tokens_url(base_url: &str) -> String {
    append_beta_query(endpoint(base_url, "messages/count_tokens"))
}

#[derive(Clone, Default)]
struct ToolUseInfo {
    name: Option<String>,
    args: Option<String>,
}

async fn bridge_anthropic_context(
    state: &ServerState,
    body: &mut Value,
    original_request: &Value,
    request_id: &str,
    model: &str,
    preserve_recent_tool_results: usize,
    target_body_bytes: Option<u64>,
    compression_reason: impl Into<String>,
) -> ContextBridgeDiagnostics {
    let query = current_anthropic_query_text(body);
    let recalled = state
        .store
        .search_context_artifacts(query, CONTEXT_BRIDGE_RECALL_LIMIT)
        .await;
    let (diagnostics, artifacts) = bridge_anthropic_context_body(
        body,
        original_request,
        request_id,
        model,
        preserve_recent_tool_results,
        target_body_bytes,
        &recalled,
        Some(compression_reason.into()),
    );
    state.store.archive_context_artifacts(artifacts).await;
    diagnostics
}

fn bridge_anthropic_context_body(
    body: &mut Value,
    original_request: &Value,
    request_id: &str,
    model: &str,
    preserve_recent_tool_results: usize,
    target_body_bytes: Option<u64>,
    recalled: &[ContextArtifact],
    compression_reason: Option<String>,
) -> (ContextBridgeDiagnostics, Vec<ContextArtifact>) {
    let original_body_bytes = json_size(body) as u64;
    let tool_uses = collect_anthropic_tool_uses(body);
    let tool_result_positions = collect_anthropic_tool_result_positions(body);
    let original_tool_result_bytes = tool_result_positions
        .iter()
        .map(|position| position.content_bytes)
        .sum::<usize>() as u64;
    let tool_result_count = tool_result_positions.len() as u64;
    let keep_from = tool_result_positions
        .len()
        .saturating_sub(preserve_recent_tool_results);
    let query = current_anthropic_query_text(body);
    let query_tokens = bridge_search_tokens(&query);
    let mut artifacts = Vec::new();
    let mut archived_tool_results = 0_u64;
    let mut archived_bytes = 0_u64;
    let mut candidates = tool_result_positions
        .into_iter()
        .enumerate()
        .filter_map(|(ordinal, position)| {
            if ordinal >= keep_from || position.content.trim().is_empty() {
                return None;
            }
            let relevant = content_matches_tokens(&position.content, &query_tokens);
            Some((ordinal, relevant, position))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_ordinal, left_relevant, left), (right_ordinal, right_relevant, right)| {
            left_relevant
                .cmp(right_relevant)
                .then(left_ordinal.cmp(right_ordinal))
                .then(right.content_bytes.cmp(&left.content_bytes))
        },
    );

    for (_ordinal, relevant, position) in candidates {
        if target_body_bytes
            .map(|target| json_size(body) as u64 <= target)
            .unwrap_or(false)
        {
            break;
        }
        let hash = sha256_hex(&position.content);
        let tool = position
            .tool_use_id
            .as_deref()
            .and_then(|id| tool_uses.get(id))
            .cloned()
            .unwrap_or_default();
        let summary = summarize_tool_result(
            &position.content,
            tool.name.as_deref(),
            tool.args.as_deref(),
            relevant,
        );
        archived_tool_results += 1;
        archived_bytes += position.content_bytes as u64;
        artifacts.push(ContextArtifact {
            hash: hash.clone(),
            created_at: Utc::now(),
            request_id: request_id.to_string(),
            model: model.to_string(),
            tool_name: tool.name.clone(),
            tool_args: tool.args.clone(),
            content_bytes: position.content_bytes as u64,
            content_text: position.content.clone(),
            summary: summary.clone(),
        });
        replace_tool_result_content(
            body,
            position.message_index,
            position.part_index,
            archived_tool_result_card(
                &hash,
                &summary,
                position.content_bytes,
                tool.name.as_deref(),
                tool.args.as_deref(),
            ),
        );
    }

    let (recalled_artifacts, recalled_bytes) = append_recalled_context(body, recalled);
    let final_body_bytes = json_size(body) as u64;
    let strategy = if archived_tool_results > 0 || recalled_artifacts > 0 {
        format!("artifact_bridge_preserve{preserve_recent_tool_results}")
    } else {
        "pass_through".to_string()
    };
    let mut diagnostics = context_bridge_diagnostics(
        body,
        original_request,
        original_body_bytes,
        original_tool_result_bytes,
    );
    diagnostics.strategy = strategy;
    diagnostics.final_body_bytes = final_body_bytes;
    diagnostics.tool_result_count = tool_result_count;
    diagnostics.kept_tool_results = tool_result_count.saturating_sub(archived_tool_results);
    diagnostics.archived_tool_results = archived_tool_results;
    diagnostics.archived_bytes = archived_bytes;
    diagnostics.recalled_artifacts = recalled_artifacts;
    diagnostics.recalled_bytes = recalled_bytes;
    diagnostics.compression_reason = compression_reason;
    (diagnostics, artifacts)
}

fn context_bridge_diagnostics(
    body: &Value,
    original_request: &Value,
    original_body_bytes: u64,
    original_tool_result_bytes: u64,
) -> ContextBridgeDiagnostics {
    let tool_result_positions = collect_anthropic_tool_result_positions(body);
    let (last_role, last_type, last_text, last_is_tool_result) =
        last_anthropic_message_summary(body);
    let latest_from_function_call_output =
        latest_request_item_is_function_call_output(original_request) && last_is_tool_result;
    let single_dot_user_message = last_role.as_deref() == Some("user")
        && !latest_from_function_call_output
        && !last_is_tool_result
        && last_text.trim() == ".";
    let (preview_head, preview_tail) = preview_head_tail(&last_text, CONTEXT_BRIDGE_PREVIEW_CHARS);
    let (latest_tool_result_count, latest_tool_result_text_length, latest_tool_result_single_dot) =
        latest_tool_result_summary(body);

    ContextBridgeDiagnostics {
        strategy: "pass_through".to_string(),
        original_body_bytes,
        final_body_bytes: json_size(body) as u64,
        original_tool_result_bytes,
        tool_result_count: tool_result_positions.len() as u64,
        kept_tool_results: tool_result_positions.len() as u64,
        archived_tool_results: 0,
        archived_bytes: 0,
        recalled_artifacts: 0,
        recalled_bytes: 0,
        count_tokens_input_tokens: None,
        count_tokens_error: None,
        context_management: false,
        raw_precheck_input_tokens: None,
        final_input_tokens: None,
        estimated_input_tokens: None,
        estimate_source: None,
        estimate_confidence: None,
        protection_triggered: false,
        target_input_tokens: None,
        previous_success_input_tokens: None,
        previous_success_body_bytes: None,
        compression_stage: None,
        protection_failure_reason: None,
        compression_reason: None,
        last_message_role: last_role,
        last_message_content_type: last_type,
        last_message_text_length: last_text.chars().count() as u64,
        last_message_preview_head: preview_head,
        last_message_preview_tail: preview_tail,
        last_message_from_function_call_output: latest_from_function_call_output,
        single_dot_user_message,
        latest_tool_result_count,
        latest_tool_result_text_length,
        latest_tool_result_single_dot,
    }
}

fn last_anthropic_message_summary(body: &Value) -> (Option<String>, Option<String>, String, bool) {
    let Some(message) = body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.last())
    else {
        return (None, None, String::new(), false);
    };
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let (content_type, text, is_tool_result) =
        anthropic_content_summary(message.get("content").unwrap_or(&Value::Null));
    (role, content_type, text, is_tool_result)
}

fn anthropic_content_summary(content: &Value) -> (Option<String>, String, bool) {
    match content {
        Value::String(text) => (Some("text".to_string()), text.clone(), false),
        Value::Array(parts) => {
            for part in parts.iter().rev() {
                let part_type = part
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("object")
                    .to_string();
                if part_type == "tool_result" {
                    return (
                        Some(part_type),
                        value_to_text(part.get("content").unwrap_or(&Value::Null)),
                        true,
                    );
                }
                if part_type == "text" {
                    return (
                        Some(part_type),
                        part.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        false,
                    );
                }
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    return (Some(part_type), text.to_string(), false);
                }
            }
            (Some("array".to_string()), value_to_text(content), false)
        }
        Value::Object(_) => {
            let content_type = content
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("object")
                .to_string();
            if content_type == "tool_result" {
                return (
                    Some(content_type),
                    value_to_text(content.get("content").unwrap_or(&Value::Null)),
                    true,
                );
            }
            (
                Some(content_type),
                content
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                false,
            )
        }
        _ => (None, String::new(), false),
    }
}

fn latest_request_item_is_function_call_output(request: &Value) -> bool {
    request
        .get("input")
        .or_else(|| request.get("messages"))
        .and_then(Value::as_array)
        .and_then(|items| items.last())
        .and_then(|item| item.get("type").and_then(Value::as_str))
        == Some("function_call_output")
}

fn latest_tool_result_summary(body: &Value) -> (u64, u64, bool) {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return (0, 0, false);
    };
    let mut count = 0_u64;
    let mut latest_len = 0_u64;
    let mut latest_single_dot = false;
    for message in messages.iter().rev() {
        let tool_results = message_tool_result_texts(message);
        if tool_results.is_empty() {
            if count > 0 {
                break;
            }
            continue;
        }
        for text in tool_results.iter().rev() {
            if count == 0 {
                latest_len = text.chars().count() as u64;
                latest_single_dot = text.trim() == ".";
            }
            count += 1;
        }
    }
    (count, latest_len, latest_single_dot)
}

fn message_tool_result_texts(message: &Value) -> Vec<String> {
    match message.get("content") {
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|part| value_to_text(part.get("content").unwrap_or(&Value::Null)))
            .collect(),
        Some(content) if message.get("type").and_then(Value::as_str) == Some("tool_result") => {
            vec![value_to_text(content)]
        }
        _ => Vec::new(),
    }
}

fn preview_head_tail(content: &str, max_chars: usize) -> (Option<String>, Option<String>) {
    if content.is_empty() {
        return (None, None);
    }
    let head = content.chars().take(max_chars).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(max_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    (Some(head), Some(tail))
}

#[derive(Clone)]
struct ToolResultPosition {
    message_index: usize,
    part_index: Option<usize>,
    tool_use_id: Option<String>,
    content: String,
    content_bytes: usize,
}

fn collect_anthropic_tool_result_positions(body: &Value) -> Vec<ToolResultPosition> {
    let mut positions = Vec::new();
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return positions;
    };
    for (message_index, message) in messages.iter().enumerate() {
        match message.get("content") {
            Some(Value::Array(parts)) => {
                for (part_index, part) in parts.iter().enumerate() {
                    if part.get("type").and_then(Value::as_str) == Some("tool_result") {
                        let content = value_to_text(part.get("content").unwrap_or(&Value::Null));
                        positions.push(ToolResultPosition {
                            message_index,
                            part_index: Some(part_index),
                            tool_use_id: part
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            content_bytes: content.len(),
                            content,
                        });
                    }
                }
            }
            Some(content) => {
                if message.get("type").and_then(Value::as_str) == Some("tool_result") {
                    let content = value_to_text(content);
                    positions.push(ToolResultPosition {
                        message_index,
                        part_index: None,
                        tool_use_id: None,
                        content_bytes: content.len(),
                        content,
                    });
                }
            }
            None => {}
        }
    }
    positions
}

fn collect_anthropic_tool_uses(body: &Value) -> HashMap<String, ToolUseInfo> {
    let mut tool_uses = HashMap::new();
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return tool_uses;
    };
    for message in messages {
        let Some(parts) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(id) = part.get("id").and_then(Value::as_str) else {
                continue;
            };
            tool_uses.insert(
                id.to_string(),
                ToolUseInfo {
                    name: part
                        .get("name")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    args: part.get("input").map(compact_json_string),
                },
            );
        }
    }
    tool_uses
}

fn replace_tool_result_content(
    body: &mut Value,
    message_index: usize,
    part_index: Option<usize>,
    replacement: String,
) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(message) = messages.get_mut(message_index) else {
        return;
    };
    match part_index {
        Some(part_index) => {
            if let Some(part) = message
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .and_then(|parts| parts.get_mut(part_index))
            {
                part["content"] = Value::String(replacement);
            }
        }
        None => {
            message["content"] = Value::String(replacement);
        }
    }
}

fn append_recalled_context(body: &mut Value, recalled: &[ContextArtifact]) -> (u64, u64) {
    if recalled.is_empty() {
        return (0, 0);
    }
    let mut recalled_bytes = 0_u64;
    let mut text = String::from(
        "Neko Route recalled relevant archived tool outputs. Exact originals remain stored locally; use these excerpts as working memory.\n",
    );
    for artifact in recalled {
        let excerpt = excerpt_text(&artifact.content_text, 900, 900);
        recalled_bytes += excerpt.len() as u64;
        text.push_str(&format!(
            "\n- sha256:{} · {} bytes · {}\n{}\n",
            artifact.hash,
            artifact.content_bytes,
            artifact.tool_name.as_deref().unwrap_or("unknown tool"),
            excerpt
        ));
    }
    append_system_text(body, text);
    (recalled.len() as u64, recalled_bytes)
}

fn append_system_text(body: &mut Value, text: String) {
    let block = json!({
        "type": "text",
        "text": text,
        "cache_control": { "type": "ephemeral" }
    });
    match body.get_mut("system") {
        Some(Value::Array(parts)) => parts.push(block),
        Some(existing) => {
            let previous = existing.take();
            *existing = Value::Array(vec![previous, block]);
        }
        None => {
            body["system"] = Value::Array(vec![block]);
        }
    }
}

fn archived_tool_result_card(
    hash: &str,
    summary: &str,
    original_bytes: usize,
    tool_name: Option<&str>,
    tool_args: Option<&str>,
) -> String {
    let args = tool_args
        .map(|value| truncate_middle(value, 420))
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "[Neko Route archived an older tool result]\nTool: {}\nArguments: {}\nOriginal size: {} bytes\nArchive hash: sha256:{}\n{}\nThe exact original output is stored locally and can be recalled by Neko Route when relevant.",
        tool_name.unwrap_or("unknown"),
        args,
        original_bytes,
        hash,
        summary
    )
}

fn summarize_tool_result(
    content: &str,
    tool_name: Option<&str>,
    tool_args: Option<&str>,
    include_relevant_excerpt: bool,
) -> String {
    let mut summary = String::new();
    summary.push_str(&format!(
        "Summary for {} output.\n",
        tool_name.unwrap_or("tool")
    ));
    if let Some(args) = tool_args {
        summary.push_str(&format!(
            "Arguments summary: {}\n",
            truncate_middle(args, 360)
        ));
    }
    let interesting = interesting_lines(content, 8);
    if !interesting.is_empty() {
        summary.push_str("Important lines:\n");
        for line in interesting {
            summary.push_str("- ");
            summary.push_str(&truncate_middle(&line, 260));
            summary.push('\n');
        }
    }
    summary.push_str("Output excerpt:\n");
    summary.push_str(&excerpt_text(content, 900, 900));
    if include_relevant_excerpt {
        summary.push_str("\nRelevant match excerpt:\n");
        summary.push_str(&excerpt_text(content, 1400, 1400));
    }
    summary
}

fn interesting_lines(content: &str, limit: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        let looks_interesting = lower.contains("error")
            || lower.contains("failed")
            || lower.contains("panic")
            || lower.contains("exception")
            || lower.contains("context window")
            || lower.contains("warning")
            || lower.contains(".rs:")
            || lower.contains(".ts:")
            || lower.contains(".tsx:")
            || lower.contains(".py:")
            || lower.contains(".js:");
        if looks_interesting {
            lines.push(trimmed.to_string());
            if lines.len() >= limit {
                break;
            }
        }
    }
    lines
}

fn current_anthropic_query_text(body: &Value) -> String {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return String::new();
    };
    for message in messages.iter().rev() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let content = message.get("content").unwrap_or(&Value::Null);
        let text = value_to_text(content);
        if !text.trim().is_empty() && !text.contains("[Neko Route archived an older tool result]") {
            return text;
        }
    }
    String::new()
}

fn bridge_search_tokens(query: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-' && ch != '.')
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.len() >= 4)
    {
        if !tokens.iter().any(|existing| existing == &token) {
            tokens.push(token);
        }
        if tokens.len() >= 24 {
            break;
        }
    }
    tokens
}

fn content_matches_tokens(content: &str, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let lower = content.to_ascii_lowercase();
    tokens.iter().any(|token| lower.contains(token))
}

fn excerpt_text(content: &str, head_chars: usize, tail_chars: usize) -> String {
    let total_chars = content.chars().count();
    if total_chars <= head_chars + tail_chars + 64 {
        return content.to_string();
    }
    let head = content.chars().take(head_chars).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!(
        "{head}\n\n... [archived middle omitted: {} chars] ...\n\n{tail}",
        total_chars.saturating_sub(head_chars + tail_chars)
    )
}

fn truncate_middle(content: &str, max_chars: usize) -> String {
    let total_chars = content.chars().count();
    if total_chars <= max_chars {
        return content.to_string();
    }
    let half = max_chars.saturating_sub(24) / 2;
    let head = content.chars().take(half).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(half)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{head} ... {tail}")
}

fn compact_json_string(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn json_size(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn sha256_hex(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn count_anthropic_input_tokens(
    client: &Client,
    base_url: &str,
    headers: &[(String, String)],
    body: &Value,
) -> Result<u64, String> {
    let count_body = build_anthropic_count_tokens_body(body);
    let mut request = client
        .post(anthropic_count_tokens_url(base_url))
        .json(&count_body)
        .timeout(Duration::from_secs(5));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().await.map_err(|error| error.to_string())?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "count_tokens {}: {}",
            status.as_u16(),
            redact(&text)
        ));
    }
    let value = serde_json::from_str::<Value>(&text).map_err(|error| error.to_string())?;
    let input_tokens = value
        .get("input_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| "count_tokens response missing input_tokens".to_string())?;
    Ok(input_tokens.saturating_add(anthropic_count_tokens_local_overhead(body)))
}

pub(crate) fn build_anthropic_count_tokens_body(body: &Value) -> Value {
    let mut messages = body
        .get("messages")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|message| message.get("role").and_then(Value::as_str) != Some("system"))
                .cloned()
                .map(|mut message| {
                    strip_cache_control_recursive(&mut message);
                    message
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": "" }));
    }
    json!({
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "messages": messages,
        "tools": [],
        "thinking": { "type": "enabled", "budget_tokens": 1024 }
    })
}

fn anthropic_count_tokens_local_overhead(body: &Value) -> u64 {
    ["system", "tools", "metadata"]
        .iter()
        .filter_map(|key| body.get(*key))
        .map(|value| (json_size(value) as u64).div_ceil(4))
        .sum()
}

fn estimate_anthropic_input_tokens_from_body(body: &Value) -> u64 {
    (json_size(body) as u64).div_ceil(4)
}

fn strip_cache_control_recursive(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("cache_control");
            for value in object.values_mut() {
                strip_cache_control_recursive(value);
            }
        }
        Value::Array(items) => {
            for value in items {
                strip_cache_control_recursive(value);
            }
        }
        _ => {}
    }
}

fn context_token_target(context_window: u64) -> u64 {
    context_window.saturating_mul(CONTEXT_BRIDGE_TOKEN_TARGET_PERCENT) / 100
}

fn context_body_target_bytes(context_window: u64) -> u64 {
    context_token_target(context_window).saturating_mul(4)
}

fn target_body_bytes_from_token_ratio(
    original_body_bytes: u64,
    raw_input_tokens: u64,
    target_input_tokens: u64,
) -> Option<u64> {
    if raw_input_tokens == 0 {
        return None;
    }
    let target = (original_body_bytes as u128).saturating_mul(target_input_tokens as u128)
        / raw_input_tokens as u128;
    Some(target.max(4096).min(u64::MAX as u128) as u64)
}

fn should_precompress_from_pressure_sample(
    sample: Option<&ClaudeContextPressureSample>,
    current_body_bytes: u64,
) -> bool {
    let Some(sample) = sample else {
        return false;
    };
    if !sample.requires_precompression || sample.context_full_body_bytes == 0 {
        return false;
    }
    (current_body_bytes as u128) * 100
        >= (sample.context_full_body_bytes as u128)
            * (CONTEXT_BRIDGE_PRECOMPRESSION_THRESHOLD_PERCENT as u128)
}

fn claude_context_key(request: &Value) -> String {
    let raw = request
        .get("prompt_cache_key")
        .or_else(|| request.pointer("/metadata/prompt_cache_key"))
        .or_else(|| request.pointer("/metadata/conversation_id"))
        .or_else(|| request.pointer("/metadata/thread_id"))
        .or_else(|| request.get("safety_identifier"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(raw) = raw {
        let hash = sha256_hex(raw);
        return format!("key:{}", &hash[..16]);
    }
    "provider_model".to_string()
}

fn context_full_error_message() -> &'static str {
    "Claude context window is full. Neko Route could not safely reduce more context; compact the Codex conversation and retry."
}

fn is_context_window_full_error(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("context window is full")
        || lower.contains("context_length_exceeded")
        || lower.contains("context length")
        || lower.contains("input exceeds the context")
}

fn upstream_error_from_text(status: reqwest::StatusCode, text: String) -> RouteError {
    let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    RouteError::new(status, "upstream_error", text)
}

fn should_skip_proxy_request_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "content-type"
            | "host"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

fn should_skip_proxy_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "content-length"
            | "transfer-encoding"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

async fn forward_anthropic(
    state: &ServerState,
    matched: &RouteMatch,
    body: Value,
    request_id: String,
    requested_model: &str,
) -> Result<
    (
        Response,
        Option<TokenUsage>,
        Option<ContextBridgeDiagnostics>,
    ),
    RouteError,
> {
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let (upstream_model, one_million_context) =
        anthropic_model_for_request(&matched.upstream_model, matched.model.context_window);
    let original_anthropic_body = build_anthropic_body(&body, &upstream_model, stream);
    let client = client_for_provider(state, &matched.provider)?;
    let (base_url, headers) = anthropic_upstream(state, &client, &matched.provider).await?;
    let url = anthropic_messages_url(&base_url, one_million_context);
    let message_headers = claude_code_mirror_headers(headers.clone(), &body);
    let count_headers = claude_code_count_tokens_headers(headers, &body);

    let original_body_bytes = json_size(&original_anthropic_body) as u64;
    let original_tool_result_bytes =
        collect_anthropic_tool_result_positions(&original_anthropic_body)
            .iter()
            .map(|position| position.content_bytes)
            .sum::<usize>() as u64;
    let context_key = claude_context_key(&body);
    let previous_pressure = state
        .store
        .claude_context_pressure(
            matched.provider.id.clone(),
            matched.model.id.clone(),
            context_key.clone(),
        )
        .await;
    let target_input_tokens = context_token_target(matched.model.context_window);
    let body_size_target = context_body_target_bytes(matched.model.context_window);
    let mut anthropic_body = original_anthropic_body.clone();
    let mut context_bridge = context_bridge_diagnostics(
        &anthropic_body,
        &body,
        original_body_bytes,
        original_tool_result_bytes,
    );
    context_bridge.context_management = anthropic_body.get("context_management").is_some();
    context_bridge.target_input_tokens = Some(target_input_tokens);
    if let Some(sample) = &previous_pressure {
        context_bridge.previous_success_input_tokens = Some(sample.input_tokens);
        context_bridge.previous_success_body_bytes = Some(sample.body_bytes);
    }
    let mut should_compress = false;
    let mut target_body_bytes = None;
    let mut compression_reason = None::<String>;
    let mut raw_precheck_input_tokens = None::<u64>;
    let mut count_tokens_error = None::<String>;

    match count_anthropic_input_tokens(&client, &base_url, &count_headers, &original_anthropic_body)
        .await
    {
        Ok(input_tokens) => {
            raw_precheck_input_tokens = Some(input_tokens);
            context_bridge.raw_precheck_input_tokens = Some(input_tokens);
            context_bridge.final_input_tokens = Some(input_tokens);
            context_bridge.count_tokens_input_tokens = Some(input_tokens);
            context_bridge.estimate_source = Some("count_tokens".into());
            context_bridge.estimate_confidence = Some("high".into());
            if input_tokens > target_input_tokens {
                should_compress = true;
                target_body_bytes = target_body_bytes_from_token_ratio(
                    original_body_bytes,
                    input_tokens,
                    target_input_tokens,
                );
                compression_reason = Some(format!(
                    "token_precheck_over_budget:{input_tokens}>{target_input_tokens}"
                ));
            }
        }
        Err(error) => {
            count_tokens_error = Some(error);
            let estimated = estimate_anthropic_input_tokens_from_body(&original_anthropic_body);
            context_bridge.estimated_input_tokens = Some(estimated);
            context_bridge.estimate_source = Some("body_size_chars".into());
            context_bridge.estimate_confidence = Some("low".into());
            if estimated > target_input_tokens || original_body_bytes > body_size_target {
                should_compress = true;
                target_body_bytes = Some(body_size_target);
                compression_reason = Some(format!(
                    "body_size_fallback:estimated={estimated} target={target_input_tokens} bytes={original_body_bytes}>{body_size_target}"
                ));
            }
        }
    }

    let should_precompress =
        should_precompress_from_pressure_sample(previous_pressure.as_ref(), original_body_bytes);
    if should_precompress {
        let previous_compression_stage = previous_pressure
            .as_ref()
            .and_then(|sample| sample.compression_stage.as_deref())
            .unwrap_or("context_full_retry_preserve_6");
        let precompression_reason =
            format!("previous_context_full_precompression:{previous_compression_stage}");
        let mut precompressed_body = original_anthropic_body.clone();
        let mut precompressed_bridge = bridge_anthropic_context(
            state,
            &mut precompressed_body,
            &body,
            &request_id,
            requested_model,
            CONTEXT_BRIDGE_FALLBACK_KEEP_TOOL_RESULTS,
            Some(0),
            precompression_reason,
        )
        .await;
        if precompressed_bridge.archived_tool_results > 0 {
            anthropic_body = precompressed_body;
            precompressed_bridge.protection_triggered = true;
            precompressed_bridge.target_input_tokens = Some(target_input_tokens);
            precompressed_bridge.previous_success_input_tokens =
                previous_pressure.as_ref().map(|sample| sample.input_tokens);
            precompressed_bridge.previous_success_body_bytes =
                previous_pressure.as_ref().map(|sample| sample.body_bytes);
            precompressed_bridge.raw_precheck_input_tokens = raw_precheck_input_tokens;
            precompressed_bridge.count_tokens_input_tokens = raw_precheck_input_tokens;
            precompressed_bridge.count_tokens_error = count_tokens_error.clone();
            precompressed_bridge.final_input_tokens = None;
            precompressed_bridge.estimated_input_tokens =
                Some(estimate_anthropic_input_tokens_from_body(&anthropic_body));
            precompressed_bridge.estimate_source =
                Some("previous_context_full_precompression".into());
            precompressed_bridge.estimate_confidence = Some("medium".into());
            precompressed_bridge.context_management = false;
            precompressed_bridge.final_body_bytes = json_size(&anthropic_body) as u64;
            precompressed_bridge.compression_stage = Some("precompress_preserve_6".into());
            context_bridge = precompressed_bridge;
            should_compress = false;
        }
    }

    if should_compress {
        let reason = compression_reason
            .clone()
            .unwrap_or_else(|| "over_budget".to_string());
        let pre_bridge_estimated_input_tokens = context_bridge.estimated_input_tokens;
        let pre_bridge_estimate_source = context_bridge.estimate_source.clone();
        let pre_bridge_estimate_confidence = context_bridge.estimate_confidence.clone();
        anthropic_body = original_anthropic_body.clone();
        context_bridge = bridge_anthropic_context(
            state,
            &mut anthropic_body,
            &body,
            &request_id,
            requested_model,
            CONTEXT_BRIDGE_KEEP_TOOL_RESULTS,
            target_body_bytes,
            reason,
        )
        .await;
        context_bridge.protection_triggered = true;
        context_bridge.target_input_tokens = Some(target_input_tokens);
        if let Some(sample) = &previous_pressure {
            context_bridge.previous_success_input_tokens = Some(sample.input_tokens);
            context_bridge.previous_success_body_bytes = Some(sample.body_bytes);
        }
        if context_bridge.estimated_input_tokens.is_none() {
            context_bridge.estimated_input_tokens =
                pre_bridge_estimated_input_tokens.or(raw_precheck_input_tokens);
        }
        context_bridge.estimate_source = pre_bridge_estimate_source;
        context_bridge.estimate_confidence = pre_bridge_estimate_confidence;
        context_bridge.compression_stage = Some("preserve_12".into());
        context_bridge.raw_precheck_input_tokens = raw_precheck_input_tokens;
        context_bridge.context_management = anthropic_body.get("context_management").is_some();
        context_bridge.final_body_bytes = json_size(&anthropic_body) as u64;
        match count_anthropic_input_tokens(&client, &base_url, &count_headers, &anthropic_body)
            .await
        {
            Ok(input_tokens) => {
                context_bridge.final_input_tokens = Some(input_tokens);
                context_bridge.count_tokens_input_tokens = Some(input_tokens);
                if input_tokens > target_input_tokens && context_bridge.archived_tool_results > 0 {
                    let reason =
                        format!("token_fallback_over_budget:{input_tokens}>{target_input_tokens}");
                    let pre_bridge_estimated_input_tokens = context_bridge.estimated_input_tokens;
                    let pre_bridge_estimate_source = context_bridge.estimate_source.clone();
                    let pre_bridge_estimate_confidence = context_bridge.estimate_confidence.clone();
                    anthropic_body = original_anthropic_body.clone();
                    context_bridge = bridge_anthropic_context(
                        state,
                        &mut anthropic_body,
                        &body,
                        &request_id,
                        requested_model,
                        CONTEXT_BRIDGE_FALLBACK_KEEP_TOOL_RESULTS,
                        target_body_bytes,
                        reason,
                    )
                    .await;
                    context_bridge.protection_triggered = true;
                    context_bridge.target_input_tokens = Some(target_input_tokens);
                    context_bridge.compression_stage = Some("preserve_6".into());
                    if let Some(sample) = &previous_pressure {
                        context_bridge.previous_success_input_tokens = Some(sample.input_tokens);
                        context_bridge.previous_success_body_bytes = Some(sample.body_bytes);
                    }
                    context_bridge.estimated_input_tokens = pre_bridge_estimated_input_tokens;
                    context_bridge.estimate_source = pre_bridge_estimate_source;
                    context_bridge.estimate_confidence = pre_bridge_estimate_confidence;
                    context_bridge.raw_precheck_input_tokens = raw_precheck_input_tokens;
                    context_bridge.context_management =
                        anthropic_body.get("context_management").is_some();
                    context_bridge.final_body_bytes = json_size(&anthropic_body) as u64;
                    match count_anthropic_input_tokens(
                        &client,
                        &base_url,
                        &count_headers,
                        &anthropic_body,
                    )
                    .await
                    {
                        Ok(input_tokens) => {
                            context_bridge.final_input_tokens = Some(input_tokens);
                            context_bridge.count_tokens_input_tokens = Some(input_tokens);
                        }
                        Err(error) => {
                            context_bridge.count_tokens_error = Some(error);
                        }
                    }
                }
            }
            Err(error) => {
                context_bridge.count_tokens_error = Some(error);
            }
        }
    } else if let Some(error) = count_tokens_error.as_ref() {
        context_bridge.count_tokens_error = Some(error.clone());
    }

    if should_compress
        && context_bridge.archived_tool_results == 0
        && (context_bridge
            .final_input_tokens
            .or(context_bridge.raw_precheck_input_tokens)
            .or(context_bridge.estimated_input_tokens)
            .map(|tokens| tokens > target_input_tokens)
            .unwrap_or(false)
            || (context_bridge.final_input_tokens.is_none()
                && context_bridge.raw_precheck_input_tokens.is_none()
                && context_bridge
                    .estimated_input_tokens
                    .map(|tokens| tokens > target_input_tokens)
                    .unwrap_or(false)))
    {
        context_bridge.protection_failure_reason = Some("no_compressible_old_tool_results".into());
        return Err(RouteError::new(
            StatusCode::BAD_REQUEST,
            "context_length_exceeded",
            context_full_error_message(),
        )
        .with_match(matched)
        .with_context_bridge(context_bridge));
    }

    let mut upstream = post_json_with_retries(
        &client,
        &url,
        message_headers.clone(),
        anthropic_body.clone(),
        matched.timeout_ms,
        matched.retry_count,
    )
    .await?;
    let mut context_full_retry_used = false;
    loop {
        if upstream.status().is_success() {
            break;
        }
        let status = upstream.status();
        let text = upstream.text().await.unwrap_or_default();
        if is_context_window_full_error(&text)
            && !context_full_retry_used
            && !context_bridge.protection_triggered
        {
            context_full_retry_used = true;
            let mut retry_body = original_anthropic_body.clone();
            let mut retry_bridge = bridge_anthropic_context(
                state,
                &mut retry_body,
                &body,
                &request_id,
                requested_model,
                CONTEXT_BRIDGE_FALLBACK_KEEP_TOOL_RESULTS,
                Some(0),
                "upstream_context_full_retry",
            )
            .await;
            retry_bridge.protection_triggered = true;
            retry_bridge.target_input_tokens = Some(target_input_tokens);
            retry_bridge.compression_stage = Some("context_full_retry_preserve_6".into());
            retry_bridge.raw_precheck_input_tokens = raw_precheck_input_tokens;
            retry_bridge.count_tokens_error = count_tokens_error.clone();
            if let Some(sample) = &previous_pressure {
                retry_bridge.previous_success_input_tokens = Some(sample.input_tokens);
                retry_bridge.previous_success_body_bytes = Some(sample.body_bytes);
            }
            retry_bridge.context_management = false;
            retry_bridge.final_body_bytes = json_size(&retry_body) as u64;
            context_bridge = retry_bridge;
            if context_bridge.archived_tool_results == 0 {
                context_bridge.protection_failure_reason =
                    Some("no_compressible_old_tool_results".into());
                return Err(RouteError::new(
                    StatusCode::BAD_REQUEST,
                    "context_length_exceeded",
                    context_full_error_message(),
                )
                .with_match(matched)
                .with_context_bridge(context_bridge));
            }
            anthropic_body = retry_body;
            upstream = post_json_with_retries(
                &client,
                &url,
                message_headers.clone(),
                anthropic_body.clone(),
                matched.timeout_ms,
                matched.retry_count,
            )
            .await?;
            continue;
        }
        if is_context_window_full_error(&text) {
            return Err(RouteError::new(
                StatusCode::BAD_REQUEST,
                "context_length_exceeded",
                context_full_error_message(),
            )
            .with_match(matched)
            .with_context_bridge(context_bridge));
        }
        return Err(upstream_error_from_text(status, text)
            .with_match(matched)
            .with_context_bridge(context_bridge));
    }
    if context_full_retry_used
        && context_bridge.protection_triggered
        && context_bridge.archived_tool_results > 0
    {
        state
            .store
            .mark_claude_context_precompression(
                matched.provider.id.clone(),
                matched.model.id.clone(),
                context_key.clone(),
                original_body_bytes,
                context_bridge.compression_stage.clone(),
            )
            .await;
    }
    if stream {
        let pressure_context = ClaudePressureContext {
            provider_id: matched.provider.id.clone(),
            model: matched.model.id.clone(),
            context_key,
            body_bytes: context_bridge.final_body_bytes,
        };
        Ok((
            converted_anthropic_sse(
                upstream,
                &request_id,
                requested_model,
                state.store.clone(),
                Some(pressure_context),
            ),
            None,
            Some(context_bridge),
        ))
    } else {
        let value = upstream_json(upstream).await?;
        let usage = value
            .get("usage")
            .map(|u| parse_usage(ProviderProtocol::AnthropicMessages, u));
        let pressure_context = ClaudePressureContext {
            provider_id: matched.provider.id.clone(),
            model: matched.model.id.clone(),
            context_key,
            body_bytes: context_bridge.final_body_bytes,
        };
        record_claude_pressure_sample(&state.store, Some(&pressure_context), usage.as_ref()).await;
        Ok((
            json_response(anthropic_response_json(
                &request_id,
                requested_model,
                &value,
                value.get("usage").cloned(),
            )),
            usage,
            Some(context_bridge),
        ))
    }
}

async fn forward_chat_completions(
    state: &ServerState,
    matched: &RouteMatch,
    body: Value,
    request_id: String,
    requested_model: &str,
) -> Result<(Response, Option<TokenUsage>), RouteError> {
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let profile =
        chat_completions_compatibility_profile(&matched.provider, &matched.upstream_model);
    let chat_body =
        build_chat_completions_body_with_profile(&body, &matched.upstream_model, stream, profile);
    let url = endpoint(&matched.provider.base_url, "chat/completions");
    let headers = provider_headers(state, &matched.provider).await?;
    let client = client_for_provider(state, &matched.provider)?;
    let mut upstream = post_json_with_retries(
        &client,
        &url,
        headers.clone(),
        chat_body.clone(),
        matched.timeout_ms,
        matched.retry_count,
    )
    .await?;
    if !upstream.status().is_success() {
        let status = upstream.status();
        let text = upstream.text().await.unwrap_or_default();
        if should_retry_chat_without_response_format(status, &text, &chat_body) {
            upstream = post_json_with_retries(
                &client,
                &url,
                headers,
                chat_body_without_response_format(chat_body),
                matched.timeout_ms,
                matched.retry_count,
            )
            .await?;
            if !upstream.status().is_success() {
                return Err(upstream_error(upstream).await);
            }
        } else {
            return Err(upstream_error_from_text(status, text));
        }
    }
    if stream {
        Ok((
            converted_chat_sse(upstream, &request_id, requested_model, state.store.clone()),
            None,
        ))
    } else {
        let value = upstream_json(upstream).await?;
        let usage = value
            .get("usage")
            .map(|u| parse_usage(ProviderProtocol::OpenAiChatCompletions, u));
        Ok((
            json_response(chat_completion_response_json(
                &request_id,
                requested_model,
                &value,
                value.get("usage").cloned(),
            )),
            usage,
        ))
    }
}

async fn provider_headers(
    state: &ServerState,
    provider: &Provider,
) -> Result<Vec<(String, String)>, RouteError> {
    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
    let Some(key_ref) = provider.key_ref.as_deref() else {
        return Ok(headers);
    };
    let secret = state
        .store
        .key_store()
        .get_secret(key_ref)
        .map_err(|message| {
            RouteError::new(
                StatusCode::FAILED_DEPENDENCY,
                "key_store_unavailable",
                message,
            )
        })?
        .ok_or_else(|| {
            RouteError::new(
                StatusCode::UNAUTHORIZED,
                "missing_provider_key",
                format!(
                    "Provider '{}' needs an API key in local storage",
                    provider.name
                ),
            )
        })?;
    headers.push(("authorization".into(), format!("Bearer {secret}")));
    Ok(headers)
}

async fn anthropic_upstream(
    state: &ServerState,
    client: &Client,
    provider: &Provider,
) -> Result<(String, Vec<(String, String)>), RouteError> {
    match &provider.kind {
        ProviderKind::OfficialAnthropicCli => {
            let auth = claude_auth::cli_auth().map_err(|message| {
                RouteError::new(StatusCode::UNAUTHORIZED, "missing_claude_auth", message)
            })?;
            Ok((auth.base_url, auth.headers))
        }
        ProviderKind::OfficialAnthropicDesktop => {
            let auth = claude_auth::desktop_auth().map_err(|message| {
                let status = if message.contains("not compatible") {
                    StatusCode::FAILED_DEPENDENCY
                } else {
                    StatusCode::UNAUTHORIZED
                };
                RouteError::new(status, "missing_claude_auth", message)
            })?;
            Ok((auth.base_url, auth.headers))
        }
        ProviderKind::OfficialAnthropicAccount => {
            let auth = official_auth::auth_for_provider(client, provider)
                .await
                .map_err(|message| {
                    RouteError::new(StatusCode::UNAUTHORIZED, "missing_official_auth", message)
                })?;
            Ok((auth.base_url, auth.headers))
        }
        ProviderKind::Custom if provider.protocol == ProviderProtocol::AnthropicMessages => Ok((
            provider.base_url.clone(),
            provider_headers(state, provider).await?,
        )),
        _ => Err(RouteError::new(
            StatusCode::BAD_REQUEST,
            "invalid_provider",
            "Provider is not an Anthropic Claude provider",
        )),
    }
}

async fn post_json_with_retries(
    client: &Client,
    url: &str,
    headers: Vec<(String, String)>,
    body: Value,
    timeout_ms: u64,
    retry_count: u8,
) -> Result<reqwest::Response, RouteError> {
    let attempts = retry_count.saturating_add(1);
    let mut last_error = None;

    for attempt in 0..attempts {
        let mut request = client.post(url).json(&body);
        if timeout_ms > 0 {
            request = request.timeout(Duration::from_millis(timeout_ms));
        }
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        match request.send().await {
            Ok(response) => {
                if !should_retry_status(response.status()) || attempt + 1 == attempts {
                    return Ok(response);
                }
                let _ = response.bytes().await;
            }
            Err(error) => {
                let code = if error.is_timeout() {
                    "upstream_timeout"
                } else {
                    "upstream_request_failed"
                };
                last_error = Some(RouteError::new(
                    if error.is_timeout() {
                        StatusCode::GATEWAY_TIMEOUT
                    } else {
                        StatusCode::BAD_GATEWAY
                    },
                    code,
                    error.to_string(),
                ));
                if attempt + 1 == attempts {
                    break;
                }
            }
        }
        sleep(Duration::from_millis(250 * u64::from(attempt + 1))).await;
    }

    Err(last_error.unwrap_or_else(|| {
        RouteError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_request_failed",
            "Upstream request failed",
        )
    }))
}

async fn post_bytes_with_retries(
    client: &Client,
    url: &str,
    headers: Vec<(String, String)>,
    body: Bytes,
    timeout_ms: u64,
    retry_count: u8,
) -> Result<reqwest::Response, RouteError> {
    let attempts = retry_count.saturating_add(1);
    let mut last_error = None;

    for attempt in 0..attempts {
        let mut request = client.post(url).body(body.clone());
        if timeout_ms > 0 {
            request = request.timeout(Duration::from_millis(timeout_ms));
        }
        for (name, value) in &headers {
            request = request.header(name, value);
        }
        match request.send().await {
            Ok(response) => {
                if !should_retry_status(response.status()) || attempt + 1 == attempts {
                    return Ok(response);
                }
                let _ = response.bytes().await;
            }
            Err(error) => {
                let code = if error.is_timeout() {
                    "upstream_timeout"
                } else {
                    "upstream_request_failed"
                };
                last_error = Some(RouteError::new(
                    if error.is_timeout() {
                        StatusCode::GATEWAY_TIMEOUT
                    } else {
                        StatusCode::BAD_GATEWAY
                    },
                    code,
                    error.to_string(),
                ));
                if attempt + 1 == attempts {
                    break;
                }
            }
        }
        sleep(Duration::from_millis(250 * u64::from(attempt + 1))).await;
    }

    Err(last_error.unwrap_or_else(|| {
        RouteError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_request_failed",
            "Upstream request failed",
        )
    }))
}

fn should_retry_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

async fn upstream_error(response: reqwest::Response) -> RouteError {
    let status =
        StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let text = response.text().await.unwrap_or_default();
    RouteError::new(status, "upstream_error", text)
}

async fn upstream_json(response: reqwest::Response) -> Result<Value, RouteError> {
    response.json::<Value>().await.map_err(|error| {
        RouteError::new(
            StatusCode::BAD_GATEWAY,
            "invalid_upstream_json",
            error.to_string(),
        )
    })
}

#[derive(Debug, Clone)]
struct RawProxyContext {
    request_id: String,
    status: u16,
    content_type: Option<String>,
    streaming: bool,
}

impl RawProxyContext {
    fn from_response(response: &reqwest::Response, request_id: String, streaming: bool) -> Self {
        Self {
            request_id,
            status: response.status().as_u16(),
            content_type: response_header(response, header::CONTENT_TYPE.as_str()),
            streaming,
        }
    }

    fn should_record_stream(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawStreamOutcome {
    state: &'static str,
    stream_error: Option<String>,
    last_event: Option<String>,
}

impl RawStreamOutcome {
    fn new(state: &'static str, stream_error: Option<String>, last_event: Option<String>) -> Self {
        Self {
            state,
            stream_error,
            last_event,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RawSseObserver {
    pending: String,
    last_event: Option<String>,
    terminal_event: Option<String>,
    terminal_error: Option<String>,
    saw_event: bool,
}

impl RawSseObserver {
    fn observe(&mut self, bytes: &Bytes) {
        self.pending.push_str(&String::from_utf8_lossy(bytes));
        const CAP: usize = 128 * 1024;
        if self.pending.len() > CAP {
            let mut start = self.pending.len() - CAP;
            while !self.pending.is_char_boundary(start) {
                start += 1;
            }
            self.pending = self.pending[start..].to_string();
        }

        while let Some((index, boundary_len)) = find_sse_boundary(&self.pending) {
            let event = self.pending[..index].to_string();
            self.pending = self.pending[index + boundary_len..].to_string();
            let Some(event_name) = raw_sse_event_name(&event) else {
                continue;
            };
            self.saw_event = true;
            self.last_event = Some(event_name.clone());
            if terminal_stream_state(&event_name).is_some() {
                self.terminal_event = Some(event_name);
                self.terminal_error = raw_sse_error_message(&event);
            }
        }
    }
}

struct StreamProgressTracker {
    store: AppStore,
    request_id: String,
    stream_bytes: u64,
    flushed_bytes: u64,
    last_flush: Instant,
}

impl StreamProgressTracker {
    fn new(store: AppStore, request_id: String) -> Self {
        Self {
            store,
            request_id,
            stream_bytes: 0,
            flushed_bytes: 0,
            last_flush: Instant::now(),
        }
    }

    async fn observe_bytes(&mut self, bytes: &Bytes) {
        self.stream_bytes = self.stream_bytes.saturating_add(bytes.len() as u64);
        if self.should_flush() {
            self.flush(None).await;
        }
    }

    async fn observe_usage(&mut self, usage: TokenUsage) {
        if usage.is_empty() {
            return;
        }
        self.flush(Some(usage)).await;
    }

    async fn finish(&mut self, usage: Option<TokenUsage>) {
        let usage = usage.filter(|usage| !usage.is_empty());
        if self.stream_bytes != self.flushed_bytes || usage.is_some() {
            self.flush(usage).await;
        }
    }

    fn should_flush(&self) -> bool {
        const MIN_BYTES: u64 = 16 * 1024;
        self.stream_bytes.saturating_sub(self.flushed_bytes) >= MIN_BYTES
            || self.last_flush.elapsed() >= Duration::from_millis(500)
    }

    async fn flush(&mut self, usage: Option<TokenUsage>) {
        self.store
            .update_request_stream_progress(self.request_id.clone(), self.stream_bytes, usage)
            .await;
        self.flushed_bytes = self.stream_bytes;
        self.last_flush = Instant::now();
    }
}

#[derive(Clone)]
struct RawStreamGuard {
    finished: Arc<AtomicBool>,
    store: AppStore,
    context: RawProxyContext,
}

impl RawStreamGuard {
    fn new(store: AppStore, context: RawProxyContext) -> Self {
        Self {
            finished: Arc::new(AtomicBool::new(false)),
            store,
            context,
        }
    }

    async fn record(&self, outcome: RawStreamOutcome) {
        if !self.context.should_record_stream() || self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        record_raw_stream_outcome(self.store.clone(), self.context.clone(), outcome).await;
    }
}

impl Drop for RawStreamGuard {
    fn drop(&mut self) {
        if !self.context.should_record_stream() || self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let store = self.store.clone();
        let context = self.context.clone();
        tokio::spawn(async move {
            record_raw_stream_outcome(
                store,
                context,
                RawStreamOutcome::new(
                    "client_disconnected",
                    Some("client disconnected before stream completed".into()),
                    None,
                ),
            )
            .await;
        });
    }
}

async fn record_raw_stream_outcome(
    store: AppStore,
    context: RawProxyContext,
    outcome: RawStreamOutcome,
) {
    store
        .update_request_stream(
            context.request_id.clone(),
            outcome.state.to_string(),
            outcome.stream_error.clone(),
            outcome.last_event.clone(),
        )
        .await;
}

fn response_header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn is_sse_content_type(content_type: Option<&str>) -> bool {
    content_type
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

fn raw_sse_event_name(event: &str) -> Option<String> {
    for line in event.lines() {
        let Some(value) = line.trim_start().strip_prefix("event:") else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }

    for data in event_data_lines(event) {
        if data == "[DONE]" {
            return Some("done".into());
        }
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if let Some(event_type) = value.get("type").and_then(Value::as_str) {
            return Some(event_type.to_string());
        }
    }
    None
}

fn raw_sse_error_message(event: &str) -> Option<String> {
    for data in event_data_lines(event) {
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let error = value
            .get("error")
            .or_else(|| value.pointer("/response/error"))?;
        let message = error.get("message").and_then(Value::as_str);
        let code = error.get("code").and_then(Value::as_str);
        let combined = match (code, message) {
            (Some(code), Some(message)) if !message.is_empty() => format!("{code}: {message}"),
            (Some(code), _) => code.to_string(),
            (_, Some(message)) if !message.is_empty() => message.to_string(),
            _ => continue,
        };
        return Some(redact(&combined));
    }
    None
}

fn terminal_stream_state(event_name: &str) -> Option<&'static str> {
    match event_name {
        "response.completed" | "done" => Some("completed"),
        "response.failed" => Some("failed"),
        "response.cancelled" => Some("cancelled"),
        _ => None,
    }
}

fn classify_raw_stream_finish(
    context: &RawProxyContext,
    observer: &RawSseObserver,
    stream_error: Option<String>,
) -> Option<RawStreamOutcome> {
    if !context.should_record_stream() {
        return None;
    }
    if let Some(error) = stream_error {
        return Some(RawStreamOutcome::new(
            "interrupted",
            Some(error),
            observer.last_event.clone(),
        ));
    }

    let requires_terminal = context.streaming
        && (is_sse_content_type(context.content_type.as_deref()) || observer.saw_event);
    if !requires_terminal {
        return Some(RawStreamOutcome::new(
            "completed",
            None,
            observer.last_event.clone(),
        ));
    }

    if let Some(event) = observer.terminal_event.as_deref() {
        let state = terminal_stream_state(event).unwrap_or("completed");
        let stream_error = if state == "failed" {
            observer.terminal_error.clone()
        } else {
            None
        };
        return Some(RawStreamOutcome::new(
            state,
            stream_error,
            observer.last_event.clone(),
        ));
    }

    Some(RawStreamOutcome::new(
        "incomplete",
        Some("stream ended before terminal event".into()),
        observer.last_event.clone(),
    ))
}

fn proxy_raw(response: reqwest::Response, store: AppStore, context: RawProxyContext) -> Response {
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::OK);
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            if should_skip_proxy_response_header(name.as_str()) {
                return None;
            }
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect::<Vec<_>>();
    let record_usage = status.is_success();
    let guard = RawStreamGuard::new(store.clone(), context.clone());
    let stream = async_stream::stream! {
        let guard = guard;
        // Accumulate text (bounded) so we can parse the final usage object.
        let mut captured = String::new();
        const CAP: usize = 2 * 1024 * 1024;
        let mut observer = RawSseObserver::default();
        let mut progress = StreamProgressTracker::new(store.clone(), context.request_id.clone());
        let mut upstream = response.bytes_stream();
        while let Some(item) = upstream.next().await {
            match item {
                Ok(bytes) => {
                    progress.observe_bytes(&bytes).await;
                    if record_usage {
                        observer.observe(&bytes);
                    }
                    if record_usage && captured.len() < CAP {
                        captured.push_str(&String::from_utf8_lossy(&bytes));
                        if captured.len() > CAP {
                            // keep only the tail where usage lives
                            let start = captured.len() - CAP;
                            captured = captured[start..].to_string();
                        }
                        if let Some(usage) = usage_from_responses_text(&captured) {
                            progress.observe_usage(usage).await;
                        }
                    }
                    yield Ok::<Bytes, std::io::Error>(bytes);
                }
                Err(error) => {
                    let error = redact(&error.to_string());
                    if let Some(outcome) =
                        classify_raw_stream_finish(&context, &observer, Some(error.clone()))
                    {
                        guard.record(outcome).await;
                    }
                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, error));
                    return;
                }
            }
        }
        if record_usage {
            let usage = usage_from_responses_text(&captured);
            progress.finish(usage).await;
            if let Some(outcome) = classify_raw_stream_finish(&context, &observer, None) {
                guard.record(outcome).await;
            }
        }
    };
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder.body(Body::from_stream(stream)).unwrap()
}

#[derive(Debug, Clone, Default)]
struct StreamingToolCall {
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    started: bool,
    output_index: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct StreamingAnthropicThinking {
    thinking: String,
    signature: String,
}

#[derive(Debug, Clone)]
struct ChatToolCallDelta {
    index: usize,
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}

const CHAT_REASONING_PREFIX: &str = "neko-route-chat-reasoning:v1:";

fn converted_chat_sse(
    response: reqwest::Response,
    request_id: &str,
    model: &str,
    store: AppStore,
) -> Response {
    let request_id = request_id.to_string();
    let model = model.to_string();
    let stream = async_stream::stream! {
        let item_id = format!("msg_{request_id}");
        let reasoning_item_id = format!("rsn_{request_id}");
        let mut sequence_number = 0_u64;
        let mut full_text = String::new();
        let mut reasoning_content = String::new();
        let mut response_started = false;
        let mut reasoning_started = false;
        let mut reasoning_done = false;
        let mut reasoning_output_index: Option<usize> = None;
        let mut text_output_index: Option<usize> = None;
        let mut tool_calls: Vec<StreamingToolCall> = Vec::new();
        let mut pending = String::new();
        let mut captured_usage: Option<TokenUsage> = None;
        let mut finish_reason: Option<String> = None;
        let mut last_stream_event: Option<String> = None;
        let mut progress = StreamProgressTracker::new(store.clone(), request_id.clone());
        let mut upstream = response.bytes_stream();

        while let Some(chunk) = upstream.next().await {
            let bytes = match chunk {
                Ok(bytes) => bytes,
                Err(error) => {
                    let error = redact(&error.to_string());
                    if !response_started {
                        for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                            yield Ok::<Bytes, Infallible>(event);
                        }
                    }
                    yield Ok::<Bytes, Infallible>(response_failed_event(
                        &mut sequence_number,
                        &request_id,
                        &model,
                        "upstream_stream_interrupted",
                        &error,
                    ));
                    progress.finish(captured_usage).await;
                    store.update_request_stream(
                        request_id.clone(),
                        "interrupted".into(),
                        Some(error),
                        last_stream_event,
                    ).await;
                    return;
                }
            };
            progress.observe_bytes(&bytes).await;
            pending.push_str(&String::from_utf8_lossy(&bytes));
            while let Some((index, boundary_len)) = find_sse_boundary(&pending) {
                let event = pending[..index].to_string();
                pending = pending[index + boundary_len..].to_string();
                for data in event_data_lines(&event) {
                    if data == "[DONE]" {
                        last_stream_event = Some("done".into());
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&data) else {
                        continue;
                    };
                    last_stream_event = Some("chat.completion.chunk".into());
                    if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
                        let usage = parse_usage(ProviderProtocol::OpenAiChatCompletions, usage);
                        captured_usage = Some(usage);
                        progress.observe_usage(usage).await;
                    }
                    if let Some(reason) = value
                        .pointer("/choices/0/finish_reason")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        finish_reason = Some(reason.to_string());
                    }
                    if let Some(delta) = value
                        .pointer("/choices/0/delta/reasoning_content")
                        .and_then(Value::as_str)
                    {
                        if !delta.is_empty() {
                            if !response_started {
                                for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                                    yield Ok::<Bytes, Infallible>(event);
                                }
                                response_started = true;
                            }
                            let output_index = *reasoning_output_index.get_or_insert_with(|| {
                                next_chat_output_index(None, text_output_index, &tool_calls)
                            });
                            if !reasoning_started {
                                for event in reasoning_output_start_events(&reasoning_item_id, output_index, &mut sequence_number) {
                                    yield Ok::<Bytes, Infallible>(event);
                                }
                                reasoning_started = true;
                            }
                            reasoning_content.push_str(delta);
                        }
                    }
                    if let Some(delta) = value
                        .pointer("/choices/0/delta/content")
                        .and_then(Value::as_str)
                    {
                        if delta.is_empty() {
                            continue;
                        }
                        if !response_started {
                            for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                                yield Ok::<Bytes, Infallible>(event);
                            }
                            response_started = true;
                        }
                        if reasoning_started && !reasoning_done {
                            let output_index = reasoning_output_index.unwrap_or(0);
                            for event in reasoning_output_done_events(&reasoning_item_id, output_index, &reasoning_content, &mut sequence_number) {
                                yield Ok::<Bytes, Infallible>(event);
                            }
                            reasoning_done = true;
                        }
                        let output_index = *text_output_index.get_or_insert_with(|| next_chat_output_index(reasoning_output_index, None, &tool_calls));
                        if full_text.is_empty() {
                            for event in text_output_start_events(&item_id, output_index, &mut sequence_number) {
                                yield Ok::<Bytes, Infallible>(event);
                            }
                        }
                        full_text.push_str(delta);
                        yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.output_text.delta", json!({
                            "type": "response.output_text.delta",
                            "delta": delta,
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": 0
                        })));
                    }

                    for tool_delta in chat_tool_call_deltas(&value) {
                        if reasoning_started && !reasoning_done {
                            let output_index = reasoning_output_index.unwrap_or(0);
                            for event in reasoning_output_done_events(&reasoning_item_id, output_index, &reasoning_content, &mut sequence_number) {
                                yield Ok::<Bytes, Infallible>(event);
                            }
                            reasoning_done = true;
                        }
                        ensure_streaming_tool_call(&mut tool_calls, tool_delta.index);
                        let fallback_call_id = format!("call_{}_{}", request_id, tool_delta.index);
                        let mut added_event = None;
                        let mut arguments_event = None;
                        {
                            let output_index = if tool_calls[tool_delta.index].output_index.is_none() {
                                next_chat_output_index(reasoning_output_index, text_output_index, &tool_calls)
                            } else {
                                tool_calls[tool_delta.index].output_index.unwrap()
                            };
                            let call = &mut tool_calls[tool_delta.index];
                            call.output_index.get_or_insert(output_index);
                        if let Some(call_id) = tool_delta.id {
                            if call.call_id.is_empty() {
                                call.call_id = call_id;
                            }
                        }
                        if let Some(name) = tool_delta.name {
                            if call.name.is_empty() {
                                call.name = name;
                            }
                        }
                            if call.call_id.is_empty() && !call.name.is_empty() {
                                call.call_id = fallback_call_id;
                            }
                        if !call.started && !call.call_id.is_empty() && !call.name.is_empty() {
                            call.item_id = format!("fc_{}", call.call_id);
                            call.started = true;
                                let output_index = call.output_index.unwrap_or(0);
                                let item = function_call_item(&call.item_id, &call.call_id, &call.name, "", "in_progress");
                                added_event = Some((output_index, item));
                        }
                        if let Some(arguments_delta) = tool_delta.arguments {
                            call.arguments.push_str(&arguments_delta);
                            if call.started {
                                    let output_index = call.output_index.unwrap_or(0);
                                    arguments_event = Some((call.item_id.clone(), output_index, arguments_delta));
                            }
                        }
                        }
                        if added_event.is_some() || arguments_event.is_some() {
                            if !response_started {
                                for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                                    yield Ok::<Bytes, Infallible>(event);
                                }
                                response_started = true;
                            }
                        }
                        if let Some((output_index, item)) = added_event {
                            yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.output_item.added", json!({
                                "type": "response.output_item.added",
                                "output_index": output_index,
                                "item": item
                            })));
                        }
                        if let Some((item_id, output_index, arguments_delta)) = arguments_event {
                            yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.function_call_arguments.delta", json!({
                                "type": "response.function_call_arguments.delta",
                                "item_id": item_id,
                                "output_index": output_index,
                                "delta": arguments_delta
                            })));
                        }
                    }
                }
            }
        }

        if !response_started {
            for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                yield Ok::<Bytes, Infallible>(event);
            }
        }
        if reasoning_started && !reasoning_done {
            let output_index = reasoning_output_index.unwrap_or(0);
            for event in reasoning_output_done_events(&reasoning_item_id, output_index, &reasoning_content, &mut sequence_number) {
                yield Ok::<Bytes, Infallible>(event);
            }
        }
        if full_text.is_empty()
            && !reasoning_content.trim().is_empty()
            && tool_calls.iter().all(|call| !call.started)
        {
            let fallback_text = reasoning_content.clone();
            let output_index = *text_output_index.get_or_insert_with(|| next_chat_output_index(reasoning_output_index, None, &tool_calls));
            for event in text_output_start_events(&item_id, output_index, &mut sequence_number) {
                yield Ok::<Bytes, Infallible>(event);
            }
            full_text.push_str(&fallback_text);
            yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.output_text.delta", json!({
                "type": "response.output_text.delta",
                "delta": fallback_text,
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0
            })));
        }

        let mut output = Vec::new();
        if !reasoning_content.is_empty() {
            let output_index = reasoning_output_index.unwrap_or_else(|| next_chat_output_index(None, text_output_index, &tool_calls));
            output.push((output_index, chat_reasoning_output_item(&reasoning_item_id, &reasoning_content)));
        }
        if let Some(output_index) = text_output_index {
            for event in text_output_done_events(&item_id, output_index, &full_text, &mut sequence_number) {
                yield Ok::<Bytes, Infallible>(event);
            }
            output.push((output_index, message_output_item(&item_id, "completed", Some(&full_text))));
        }
        for call in tool_calls.iter().filter(|call| call.started) {
            let output_index = call.output_index.unwrap_or_else(|| next_chat_output_index(reasoning_output_index, text_output_index, &tool_calls));
                yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.function_call_arguments.done", json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": call.item_id,
                    "output_index": output_index,
                    "arguments": call.arguments
                })));
                let item = function_call_item(&call.item_id, &call.call_id, &call.name, &call.arguments, "completed");
                yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.output_item.done", json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": item
                })));
            output.push((output_index, item));
        }
        output.sort_by_key(|(index, _)| *index);
        let output = output.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
        let usage_json = captured_usage.map(usage_to_responses_json);
        let status = if finish_reason.as_deref() == Some("length") {
            "incomplete"
        } else {
            "completed"
        };
        yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.completed", json!({
            "type": "response.completed",
            "response": response_object_with_incomplete_details(
                &request_id,
                &model,
                status,
                output,
                usage_json,
                chat_completion_incomplete_details(status)
            )
        })));
        progress.finish(captured_usage).await;
        store.update_request_stream(
            request_id.clone(),
            status.into(),
            None,
            Some("response.completed".into()),
        ).await;
    };

    sse_response(Body::from_stream(stream))
}

#[derive(Clone)]
struct ClaudePressureContext {
    provider_id: String,
    model: String,
    context_key: String,
    body_bytes: u64,
}

async fn record_claude_pressure_sample(
    store: &AppStore,
    context: Option<&ClaudePressureContext>,
    usage: Option<&TokenUsage>,
) {
    let Some(context) = context else {
        return;
    };
    let Some(usage) = usage else {
        return;
    };
    if usage.input_tokens == 0 || context.body_bytes == 0 {
        return;
    }
    store
        .upsert_claude_context_pressure(
            context.provider_id.clone(),
            context.model.clone(),
            context.context_key.clone(),
            usage.input_tokens,
            context.body_bytes,
        )
        .await;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AnthropicStreamTerminalState {
    Completed,
    Incomplete { reason: &'static str },
    Failed { code: &'static str, message: String },
}

fn classify_anthropic_stream_terminal(
    saw_message_stop: bool,
    stop_reason: Option<&str>,
    last_upstream_event: Option<&str>,
) -> AnthropicStreamTerminalState {
    if !saw_message_stop {
        let last = last_upstream_event.unwrap_or("none");
        return AnthropicStreamTerminalState::Incomplete {
            reason: if last == "none" {
                "stream_ended_without_events"
            } else {
                "stream_ended_without_message_stop"
            },
        };
    }

    match stop_reason {
        Some("end_turn" | "tool_use" | "stop_sequence") => AnthropicStreamTerminalState::Completed,
        Some("max_tokens") => AnthropicStreamTerminalState::Incomplete {
            reason: "max_output_tokens",
        },
        Some(reason) => AnthropicStreamTerminalState::Failed {
            code: "unsupported_anthropic_stop_reason",
            message: format!("Anthropic stream stopped with unsupported stop_reason '{reason}'."),
        },
        None => AnthropicStreamTerminalState::Failed {
            code: "missing_anthropic_stop_reason",
            message: "Anthropic stream ended without stop_reason.".into(),
        },
    }
}

fn incomplete_details(reason: &'static str) -> Option<Value> {
    Some(json!({ "reason": reason }))
}

fn response_failed_event(
    sequence_number: &mut u64,
    request_id: &str,
    model: &str,
    code: &str,
    message: &str,
) -> Bytes {
    sequenced_sse_event(
        sequence_number,
        "response.failed",
        json!({
            "type": "response.failed",
            "response": response_failed_object(request_id, model, code, message)
        }),
    )
}

fn response_failed_object(request_id: &str, model: &str, code: &str, message: &str) -> Value {
    json!({
        "id": format!("resp_{request_id}"),
        "object": "response",
        "created_at": Utc::now().timestamp(),
        "status": "failed",
        "model": model,
        "output": [],
        "output_text": "",
        "usage": null,
        "error": {
            "code": code,
            "message": redact(message)
        }
    })
}

fn converted_anthropic_sse(
    response: reqwest::Response,
    request_id: &str,
    model: &str,
    store: AppStore,
    pressure_context: Option<ClaudePressureContext>,
) -> Response {
    let request_id = request_id.to_string();
    let model = model.to_string();
    let stream = async_stream::stream! {
        let item_id = format!("msg_{request_id}");
        let mut sequence_number = 0_u64;
        let mut full_text = String::new();
        let mut response_started = false;
        let mut text_output_index: Option<usize> = None;
        let mut tool_calls: Vec<StreamingToolCall> = Vec::new();
        let mut thinking_blocks: HashMap<usize, StreamingAnthropicThinking> = HashMap::new();
        let mut pending = String::new();
        let mut anthropic_usage = serde_json::Map::new();
        let mut saw_message_stop = false;
        let mut stop_reason: Option<String> = None;
        let mut last_upstream_event: Option<String> = None;
        let mut stream_error: Option<String> = None;
        let mut progress = StreamProgressTracker::new(store.clone(), request_id.clone());
        let mut upstream = response.bytes_stream();

        while let Some(chunk) = upstream.next().await {
            let bytes = match chunk {
                Ok(bytes) => bytes,
                Err(error) => {
                    stream_error = Some(redact(&error.to_string()));
                    break;
                }
            };
            progress.observe_bytes(&bytes).await;
            pending.push_str(&String::from_utf8_lossy(&bytes));
            while let Some((index, boundary_len)) = find_sse_boundary(&pending) {
                let event = pending[..index].to_string();
                pending = pending[index + boundary_len..].to_string();
                if let Some(event_name) = raw_sse_event_name(&event) {
                    last_upstream_event = Some(event_name);
                }
                for data in event_data_lines(&event) {
                    let Ok(value) = serde_json::from_str::<Value>(&data) else {
                        continue;
                    };
                    if let Some(event_type) = value.get("type").and_then(Value::as_str) {
                        last_upstream_event = Some(event_type.to_string());
                        if event_type == "message_stop" {
                            saw_message_stop = true;
                        }
                        if event_type == "message_delta" {
                            if let Some(reason) = value
                                .pointer("/delta/stop_reason")
                                .and_then(Value::as_str)
                                .filter(|value| !value.is_empty())
                            {
                                stop_reason = Some(reason.to_string());
                            }
                        }
                        if event_type == "error" {
                            let message = raw_sse_error_message(&event)
                                .unwrap_or_else(|| "Anthropic stream returned an error event.".into());
                            if !response_started {
                                for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                                    yield Ok::<Bytes, Infallible>(event);
                                }
                            }
                            yield Ok::<Bytes, Infallible>(response_failed_event(
                                &mut sequence_number,
                                &request_id,
                                &model,
                                "upstream_stream_error",
                                &message,
                            ));
                            let captured_usage = if anthropic_usage.is_empty() {
                                None
                            } else {
                                Some(parse_usage(
                                    ProviderProtocol::AnthropicMessages,
                                    &Value::Object(anthropic_usage.clone()),
                                ))
                            };
                            progress.finish(captured_usage).await;
                            store.update_request_stream(
                                request_id.clone(),
                                "failed".into(),
                                Some(message),
                                last_upstream_event,
                            ).await;
                            return;
                        }
                    }
                    // Capture usage: message_start carries input/cache tokens,
                    // message_delta carries the running output token count.
                    if let Some(u) = value.pointer("/message/usage").and_then(Value::as_object) {
                        for (k, v) in u {
                            anthropic_usage.insert(k.clone(), v.clone());
                        }
                        let usage = parse_usage(ProviderProtocol::AnthropicMessages, &Value::Object(anthropic_usage.clone()));
                        progress.observe_usage(usage).await;
                    }
                    if let Some(u) = value.get("usage").and_then(Value::as_object) {
                        for (k, v) in u {
                            anthropic_usage.insert(k.clone(), v.clone());
                        }
                        let usage = parse_usage(ProviderProtocol::AnthropicMessages, &Value::Object(anthropic_usage.clone()));
                        progress.observe_usage(usage).await;
                    }
                    match value.get("type").and_then(Value::as_str) {
                        Some("content_block_start")
                            if value.pointer("/content_block/type").and_then(Value::as_str)
                                == Some("thinking") =>
                        {
                            let block_index =
                                value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            let block = thinking_blocks.entry(block_index).or_default();
                            if let Some(thinking) =
                                value.pointer("/content_block/thinking").and_then(Value::as_str)
                            {
                                block.thinking.push_str(thinking);
                            }
                            if let Some(signature) =
                                value.pointer("/content_block/signature").and_then(Value::as_str)
                            {
                                block.signature.push_str(signature);
                            }
                        }
                        Some("content_block_delta")
                            if value.pointer("/delta/type").and_then(Value::as_str)
                                == Some("thinking_delta") =>
                        {
                            let block_index =
                                value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            if let Some(thinking) =
                                value.pointer("/delta/thinking").and_then(Value::as_str)
                            {
                                thinking_blocks
                                    .entry(block_index)
                                    .or_default()
                                    .thinking
                                    .push_str(thinking);
                            }
                        }
                        Some("content_block_delta")
                            if value.pointer("/delta/type").and_then(Value::as_str)
                                == Some("signature_delta") =>
                        {
                            let block_index =
                                value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            if let Some(signature) =
                                value.pointer("/delta/signature").and_then(Value::as_str)
                            {
                                thinking_blocks
                                    .entry(block_index)
                                    .or_default()
                                    .signature
                                    .push_str(signature);
                            }
                        }
                        Some("content_block_start")
                            if value.pointer("/content_block/type").and_then(Value::as_str)
                                == Some("tool_use") =>
                        {
                            let block_index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            ensure_streaming_tool_call(&mut tool_calls, block_index);
                            let (output_index, item) = {
                                let output_index = next_output_index(text_output_index, &tool_calls);
                                let call = &mut tool_calls[block_index];
                                call.output_index.get_or_insert(output_index);
                                call.call_id = value
                                    .pointer("/content_block/id")
                                    .and_then(Value::as_str)
                                    .map(ToOwned::to_owned)
                                    .unwrap_or_else(|| format!("call_{}_{}", request_id, block_index));
                                call.name = value
                                    .pointer("/content_block/name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("tool")
                                    .to_string();
                                call.item_id = format!("fc_{}", call.call_id);
                                call.started = true;
                                if let Some(input) = value.pointer("/content_block/input") {
                                    let initial_arguments = value_to_argument_string(Some(input));
                                    if initial_arguments != "{}" {
                                        call.arguments.push_str(&initial_arguments);
                                    }
                                }
                                let output_index = call.output_index.unwrap_or(0);
                                let item = function_call_item(&call.item_id, &call.call_id, &call.name, "", "in_progress");
                                (output_index, item)
                            };
                            if !response_started {
                                for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                                    yield Ok::<Bytes, Infallible>(event);
                                }
                                response_started = true;
                            }
                            yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.output_item.added", json!({
                                "type": "response.output_item.added",
                                "output_index": output_index,
                                "item": item
                            })));
                        }
                        Some("content_block_delta")
                            if value.pointer("/delta/type").and_then(Value::as_str) == Some("text_delta") =>
                        {
                            let delta = value.pointer("/delta/text").and_then(Value::as_str).unwrap_or_default();
                            if delta.is_empty() {
                                continue;
                            }
                            if !response_started {
                                for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                                    yield Ok::<Bytes, Infallible>(event);
                                }
                                response_started = true;
                            }
                            let output_index = *text_output_index.get_or_insert_with(|| next_output_index(None, &tool_calls));
                            if full_text.is_empty() {
                                for event in text_output_start_events(&item_id, output_index, &mut sequence_number) {
                                    yield Ok::<Bytes, Infallible>(event);
                                }
                            }
                            full_text.push_str(delta);
                            yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.output_text.delta", json!({
                                "type": "response.output_text.delta",
                                "delta": delta,
                                "item_id": item_id,
                                "output_index": output_index,
                                "content_index": 0
                            })));
                        }
                        Some("content_block_delta")
                            if value.pointer("/delta/type").and_then(Value::as_str) == Some("input_json_delta") =>
                        {
                            let block_index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            ensure_streaming_tool_call(&mut tool_calls, block_index);
                            let partial_json = value.pointer("/delta/partial_json").and_then(Value::as_str).unwrap_or_default().to_string();
                            if partial_json.is_empty() {
                                continue;
                            }
                            let Some((item_id, output_index)) = ({
                                let call = &mut tool_calls[block_index];
                                if !call.started {
                                    None
                                } else {
                                    call.arguments.push_str(&partial_json);
                                    Some((call.item_id.clone(), call.output_index.unwrap_or(0)))
                                }
                            }) else {
                                continue;
                            };
                            yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.function_call_arguments.delta", json!({
                                "type": "response.function_call_arguments.delta",
                                "item_id": item_id,
                                "output_index": output_index,
                                "delta": partial_json
                            })));
                        }
                        _ => {}
                    }
                }
            }
        }

        let captured_usage = if anthropic_usage.is_empty() {
            None
        } else {
            Some(parse_usage(ProviderProtocol::AnthropicMessages, &Value::Object(anthropic_usage)))
        };
        if let Some(error) = stream_error {
            if !response_started {
                for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                    yield Ok::<Bytes, Infallible>(event);
                }
            }
            yield Ok::<Bytes, Infallible>(response_failed_event(
                &mut sequence_number,
                &request_id,
                &model,
                "upstream_stream_interrupted",
                &error,
            ));
            progress.finish(captured_usage).await;
            store.update_request_stream(
                request_id.clone(),
                "interrupted".into(),
                Some(error),
                last_upstream_event,
            ).await;
            return;
        }

        let terminal_state = classify_anthropic_stream_terminal(
            saw_message_stop,
            stop_reason.as_deref(),
            last_upstream_event.as_deref(),
        );
        if let AnthropicStreamTerminalState::Failed { code, message } = &terminal_state {
            if !response_started {
                for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                    yield Ok::<Bytes, Infallible>(event);
                }
            }
            yield Ok::<Bytes, Infallible>(response_failed_event(
                &mut sequence_number,
                &request_id,
                &model,
                code,
                message,
            ));
            progress.finish(captured_usage).await;
            store.update_request_stream(
                request_id.clone(),
                "failed".into(),
                Some(message.clone()),
                last_upstream_event,
            ).await;
            return;
        }

        if !response_started {
            for event in response_lifecycle_start_events(&request_id, &model, &mut sequence_number) {
                yield Ok::<Bytes, Infallible>(event);
            }
        }

        let mut output = Vec::new();
        if let Some(output_index) = text_output_index {
            for event in text_output_done_events(&item_id, output_index, &full_text, &mut sequence_number) {
                yield Ok::<Bytes, Infallible>(event);
            }
            output.push((output_index, message_output_item(&item_id, "completed", Some(&full_text))));
        }
        for call in tool_calls.iter().filter(|call| call.started) {
            let output_index = call.output_index.unwrap_or_else(|| next_output_index(text_output_index, &tool_calls));
            yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.function_call_arguments.done", json!({
                "type": "response.function_call_arguments.done",
                "item_id": call.item_id,
                "output_index": output_index,
                "arguments": call.arguments
            })));
            let item = function_call_item(&call.item_id, &call.call_id, &call.name, &call.arguments, "completed");
            yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.output_item.done", json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": item
            })));
            output.push((output_index, item));
        }
        let mut thinking_indices = thinking_blocks.keys().copied().collect::<Vec<_>>();
        thinking_indices.sort_unstable();
        let mut reasoning_output = Vec::new();
        for (index, block_index) in thinking_indices.into_iter().enumerate() {
            let Some(block) = thinking_blocks.get(&block_index) else {
                continue;
            };
            if !block.thinking.is_empty() && !block.signature.is_empty() {
                reasoning_output.push(anthropic_reasoning_output_item(
                    &format!("rsn_{}_{}", request_id, index),
                    &block.thinking,
                    &block.signature,
                ));
            }
        }
        output.sort_by_key(|(index, _)| *index);
        let mut output = output.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
        if !reasoning_output.is_empty() {
            reasoning_output.append(&mut output);
            output = reasoning_output;
        }
        let (status, incomplete_details, stream_state, stream_error, last_event) =
            match terminal_state {
                AnthropicStreamTerminalState::Completed => (
                    "completed",
                    None,
                    "completed",
                    None,
                    Some("response.completed".into()),
                ),
                AnthropicStreamTerminalState::Incomplete { reason } => {
                    let stream_error = if reason == "max_output_tokens" {
                        None
                    } else {
                        Some(format!(
                            "Anthropic stream ended before message_stop; last upstream event: {}",
                            last_upstream_event.as_deref().unwrap_or("none")
                        ))
                    };
                    (
                        "incomplete",
                        incomplete_details(reason),
                        "incomplete",
                        stream_error,
                        if reason == "max_output_tokens" {
                            Some("response.completed".into())
                        } else {
                            last_upstream_event.clone()
                        },
                    )
                }
                AnthropicStreamTerminalState::Failed { .. } => unreachable!(),
            };
        let usage_json = captured_usage.map(usage_to_responses_json);
        yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.completed", json!({
            "type": "response.completed",
            "response": response_object_with_incomplete_details(
                &request_id,
                &model,
                status,
                output,
                usage_json,
                incomplete_details
            )
        })));
        progress.finish(captured_usage).await;
        if saw_message_stop {
            record_claude_pressure_sample(&store, pressure_context.as_ref(), captured_usage.as_ref()).await;
        }
        store.update_request_stream(
            request_id.clone(),
            stream_state.into(),
            stream_error,
            last_event,
        ).await;
    };

    sse_response(Body::from_stream(stream))
}

/// Convert normalized usage back into an OpenAI Responses-style usage object
/// for the client-facing `response.completed` event.
fn usage_to_responses_json(usage: TokenUsage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "input_tokens_details": { "cached_tokens": usage.cache_read_tokens },
        "output_tokens": usage.output_tokens,
        "total_tokens": usage.total_tokens
    })
}

fn sse_response(body: Body) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(body)
        .unwrap()
}

fn json_response(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

fn sse_event(event: &str, data: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {}\n\n", data))
}

fn sequenced_sse_event(sequence_number: &mut u64, event: &str, mut data: Value) -> Bytes {
    if let Some(object) = data.as_object_mut() {
        object.insert("sequence_number".into(), json!(*sequence_number));
    }
    *sequence_number += 1;
    sse_event(event, data)
}

#[cfg(test)]
fn response_stream_start_events(
    request_id: &str,
    model: &str,
    item_id: &str,
    sequence_number: &mut u64,
) -> Vec<Bytes> {
    vec![
        sequenced_sse_event(
            sequence_number,
            "response.created",
            json!({
                "type": "response.created",
                "response": response_object(request_id, model, "in_progress", Vec::new(), None)
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.in_progress",
            json!({
                "type": "response.in_progress",
                "response": response_object(request_id, model, "in_progress", Vec::new(), None)
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": 0,
                "item": message_output_item(item_id, "in_progress", None)
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.content_part.added",
            json!({
                "type": "response.content_part.added",
                "item_id": item_id,
                "output_index": 0,
                "content_index": 0,
                "part": output_text_part("")
            }),
        ),
    ]
}

#[cfg(test)]
fn response_stream_done_events(
    request_id: &str,
    model: &str,
    item_id: &str,
    text: &str,
    sequence_number: &mut u64,
) -> Vec<Bytes> {
    let output = vec![message_output_item(item_id, "completed", Some(text))];
    vec![
        sequenced_sse_event(
            sequence_number,
            "response.output_text.done",
            json!({
                "type": "response.output_text.done",
                "item_id": item_id,
                "output_index": 0,
                "content_index": 0,
                "text": text
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.content_part.done",
            json!({
                "type": "response.content_part.done",
                "item_id": item_id,
                "output_index": 0,
                "content_index": 0,
                "part": output_text_part(text)
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": message_output_item(item_id, "completed", Some(text))
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.completed",
            json!({
                "type": "response.completed",
                "response": response_object(request_id, model, "completed", output, None)
            }),
        ),
    ]
}

fn response_lifecycle_start_events(
    request_id: &str,
    model: &str,
    sequence_number: &mut u64,
) -> Vec<Bytes> {
    vec![
        sequenced_sse_event(
            sequence_number,
            "response.created",
            json!({
                "type": "response.created",
                "response": response_object(request_id, model, "in_progress", Vec::new(), None)
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.in_progress",
            json!({
                "type": "response.in_progress",
                "response": response_object(request_id, model, "in_progress", Vec::new(), None)
            }),
        ),
    ]
}

fn text_output_start_events(
    item_id: &str,
    output_index: usize,
    sequence_number: &mut u64,
) -> Vec<Bytes> {
    vec![
        sequenced_sse_event(
            sequence_number,
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": message_output_item(item_id, "in_progress", None)
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.content_part.added",
            json!({
                "type": "response.content_part.added",
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "part": output_text_part("")
            }),
        ),
    ]
}

fn text_output_done_events(
    item_id: &str,
    output_index: usize,
    text: &str,
    sequence_number: &mut u64,
) -> Vec<Bytes> {
    vec![
        sequenced_sse_event(
            sequence_number,
            "response.output_text.done",
            json!({
                "type": "response.output_text.done",
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "text": text
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.content_part.done",
            json!({
                "type": "response.content_part.done",
                "item_id": item_id,
                "output_index": output_index,
                "content_index": 0,
                "part": output_text_part(text)
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": message_output_item(item_id, "completed", Some(text))
            }),
        ),
    ]
}

fn reasoning_output_start_events(
    item_id: &str,
    output_index: usize,
    sequence_number: &mut u64,
) -> Vec<Bytes> {
    vec![
        sequenced_sse_event(
            sequence_number,
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "output_index": output_index,
                "item": chat_reasoning_stream_item(item_id, "in_progress", None)
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.reasoning_summary_part.added",
            json!({
                "type": "response.reasoning_summary_part.added",
                "item_id": item_id,
                "output_index": output_index,
                "summary_index": 0,
                "part": { "type": "summary_text", "text": "" }
            }),
        ),
    ]
}

fn reasoning_output_done_events(
    item_id: &str,
    output_index: usize,
    reasoning_content: &str,
    sequence_number: &mut u64,
) -> Vec<Bytes> {
    vec![
        sequenced_sse_event(
            sequence_number,
            "response.reasoning_summary_part.done",
            json!({
                "type": "response.reasoning_summary_part.done",
                "item_id": item_id,
                "output_index": output_index,
                "summary_index": 0,
                "part": { "type": "summary_text", "text": "" }
            }),
        ),
        sequenced_sse_event(
            sequence_number,
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": chat_reasoning_stream_item(item_id, "completed", Some(reasoning_content))
            }),
        ),
    ]
}

fn next_output_index(text_output_index: Option<usize>, tool_calls: &[StreamingToolCall]) -> usize {
    let text_index = text_output_index.map(|index| index + 1).unwrap_or(0);
    let tool_index = tool_calls
        .iter()
        .filter_map(|call| call.output_index)
        .max()
        .map(|index| index + 1)
        .unwrap_or(0);
    text_index.max(tool_index)
}

fn next_chat_output_index(
    reasoning_output_index: Option<usize>,
    text_output_index: Option<usize>,
    tool_calls: &[StreamingToolCall],
) -> usize {
    let reasoning_index = reasoning_output_index.map(|index| index + 1).unwrap_or(0);
    reasoning_index.max(next_output_index(text_output_index, tool_calls))
}

fn ensure_streaming_tool_call(tool_calls: &mut Vec<StreamingToolCall>, index: usize) {
    while tool_calls.len() <= index {
        tool_calls.push(StreamingToolCall::default());
    }
}

fn chat_tool_call_deltas(value: &Value) -> Vec<ChatToolCallDelta> {
    value
        .pointer("/choices/0/delta/tool_calls")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .enumerate()
                .map(|(position, item)| ChatToolCallDelta {
                    index: item
                        .get("index")
                        .and_then(Value::as_u64)
                        .map(|index| index as usize)
                        .unwrap_or(position),
                    id: item
                        .get("id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    name: item
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    arguments: item
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn find_sse_boundary(input: &str) -> Option<(usize, usize)> {
    match (input.find("\n\n"), input.find("\r\n\r\n")) {
        (Some(lf), Some(crlf)) if crlf < lf => Some((crlf, 4)),
        (Some(lf), _) => Some((lf, 2)),
        (None, Some(crlf)) => Some((crlf, 4)),
        (None, None) => None,
    }
}

fn event_data_lines(event: &str) -> Vec<String> {
    event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
fn extract_stream_delta(protocol: &ProviderProtocol, data: &str) -> Option<String> {
    let value: Value = serde_json::from_str(data).ok()?;
    match protocol {
        ProviderProtocol::AnthropicMessages => {
            if value.get("type").and_then(Value::as_str) == Some("content_block_delta") {
                value
                    .pointer("/delta/text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            } else {
                None
            }
        }
        ProviderProtocol::OpenAiChatCompletions | ProviderProtocol::OpenAiResponses => value
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }
}

pub(crate) fn build_anthropic_body(request: &Value, upstream_model: &str, stream: bool) -> Value {
    let (system_instructions, messages) = anthropic_messages_from_request(request);
    let max_tokens = request
        .get("max_output_tokens")
        .or_else(|| request.get("max_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(1024);

    let mut body = json!({
        "model": upstream_model,
        "messages": messages,
        "stream": stream
    });

    body["system"] = Value::Array(claude_desktop_system_blocks(system_instructions.as_deref()));
    add_claude_desktop_metadata(&mut body, request);
    add_claude_desktop_latest_user_cache_control(&mut body);

    let tools = anthropic_tools_from_request(request);
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }

    body["thinking"] = json!({ "type": "adaptive" });
    body["output_config"] = json!({ "effort": "max" });
    body["max_tokens"] = json!(max_tokens);
    body
}

fn claude_desktop_system_blocks(instructions: Option<&str>) -> Vec<Value> {
    let mut blocks = vec![
        json!({
            "type": "text",
            "text": CLAUDE_DESKTOP_BILLING_HEADER
        }),
        json!({
            "type": "text",
            "text": CLAUDE_DESKTOP_IDENTITY,
            "cache_control": { "type": "ephemeral" }
        }),
    ];
    if let Some(instructions) = instructions.filter(|value| !value.is_empty()) {
        blocks.push(json!({
            "type": "text",
            "text": instructions,
            "cache_control": { "type": "ephemeral" }
        }));
    }
    blocks
}

fn add_claude_desktop_metadata(body: &mut Value, request: &Value) {
    let mut metadata = request
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    metadata.insert("user_id".into(), json!(claude_desktop_user_id(request)));
    body["metadata"] = Value::Object(metadata);
}

fn claude_desktop_user_id(request: &Value) -> String {
    let mut user = request
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let device_id = user
        .get("device_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(claude_desktop_device_id);
    let account_uuid = user
        .get("account_uuid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    user.insert("device_id".into(), json!(device_id));
    user.insert("account_uuid".into(), json!(account_uuid));
    user.insert("session_id".into(), json!(claude_code_session_id(request)));
    serde_json::to_string(&Value::Object(user)).unwrap_or_else(|_| "{}".into())
}

fn claude_desktop_device_id() -> String {
    static DEVICE_ID: OnceLock<String> = OnceLock::new();
    DEVICE_ID
        .get_or_init(|| sha256_hex(&Uuid::new_v4().to_string()))
        .clone()
}

fn add_claude_desktop_latest_user_cache_control(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages.iter_mut().rev() {
        if message.get("role").and_then(Value::as_str) == Some("user")
            && add_cache_control_to_anthropic_message(message)
        {
            break;
        }
    }
}

fn add_cache_control_to_anthropic_message(message: &mut Value) -> bool {
    let Some(content) = message.get_mut("content") else {
        return false;
    };
    match content {
        Value::String(text) => {
            if text.is_empty() {
                return false;
            }
            let text = std::mem::take(text);
            *content = json!([{
                "type": "text",
                "text": text,
                "cache_control": { "type": "ephemeral" }
            }]);
            true
        }
        Value::Array(parts) => parts
            .iter_mut()
            .any(add_cache_control_to_anthropic_content_part),
        _ => false,
    }
}

fn add_cache_control_to_anthropic_content_part(part: &mut Value) -> bool {
    let Some(obj) = part.as_object_mut() else {
        return false;
    };
    let cacheable = match obj.get("type").and_then(Value::as_str) {
        Some("text") => obj
            .get("text")
            .and_then(Value::as_str)
            .map(|text| !text.is_empty())
            .unwrap_or(false),
        Some("tool_result") => match obj.get("content") {
            Some(Value::String(text)) => !text.is_empty(),
            Some(Value::Array(parts)) => !parts.is_empty(),
            Some(Value::Object(_)) => true,
            _ => false,
        },
        _ => false,
    };
    if !cacheable {
        return false;
    }
    obj.insert("cache_control".into(), json!({ "type": "ephemeral" }));
    true
}

/// Read the raw reasoning effort Codex sends, checking every shape it's known
/// to use across versions: nested `reasoning.effort`, flat `reasoning_effort`,
/// top-level `effort`, or `output_config.effort`.
fn raw_reasoning_effort(request: &Value) -> Option<String> {
    request
        .pointer("/reasoning/effort")
        .or_else(|| request.get("reasoning_effort"))
        .or_else(|| request.get("effort"))
        .or_else(|| request.pointer("/output_config/effort"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

/// Validate the reasoning effort Codex sends. Claude supports these same tiers
/// plus `max`, so recognized values pass through unchanged.
fn map_reasoning_effort(request: &Value) -> Option<&'static str> {
    match raw_reasoning_effort(request)?.as_str() {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

fn map_openai_chat_reasoning_effort(request: &Value) -> Option<&'static str> {
    match raw_reasoning_effort(request)?.as_str() {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChatCompletionsCompatibilityProfile {
    JsonSchemaCapable,
    JsonObjectOnly,
}

#[derive(Default)]
struct ChatResponseFormatSelection {
    response_format: Option<Value>,
    requires_json_prompt: bool,
    schema_hint: Option<String>,
}

fn chat_completions_compatibility_profile(
    provider: &Provider,
    upstream_model: &str,
) -> ChatCompletionsCompatibilityProfile {
    let base_url = provider.base_url.to_ascii_lowercase();
    let identity =
        format!("{} {} {}", provider.id, provider.name, upstream_model).to_ascii_lowercase();
    let schema_capable = [
        "api.openai.com",
        "openrouter.ai",
        "api.groq.com",
        "api.x.ai",
    ]
    .iter()
    .any(|needle| base_url.contains(needle))
        || provider.kind == ProviderKind::OfficialOpenAi
        || provider.kind == ProviderKind::OfficialOpenAiAccount
        || identity.contains("openrouter")
        || identity.contains("groq")
        || identity.contains("xai")
        || identity.contains("x.ai");
    if schema_capable {
        ChatCompletionsCompatibilityProfile::JsonSchemaCapable
    } else {
        ChatCompletionsCompatibilityProfile::JsonObjectOnly
    }
}

#[cfg(test)]
fn build_chat_completions_body(request: &Value, upstream_model: &str, stream: bool) -> Value {
    build_chat_completions_body_with_profile(
        request,
        upstream_model,
        stream,
        ChatCompletionsCompatibilityProfile::JsonSchemaCapable,
    )
}

fn build_chat_completions_body_with_profile(
    request: &Value,
    upstream_model: &str,
    stream: bool,
    profile: ChatCompletionsCompatibilityProfile,
) -> Value {
    let mut messages = chat_messages_from_request(request);
    let response_format = chat_response_format_from_request(request, profile);
    apply_chat_response_format_prompt_hints(&mut messages, &response_format);
    let mut body = json!({
        "model": upstream_model,
        "messages": messages,
        "stream": stream
    });
    if let Some(effort) = map_openai_chat_reasoning_effort(request) {
        body["reasoning_effort"] = json!(effort);
    }
    let tools = chat_tools_from_request(request);
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
        body["tool_choice"] =
            chat_tool_choice_from_request(request).unwrap_or_else(|| json!("auto"));
    }
    if let Some(parallel_tool_calls) = request.get("parallel_tool_calls") {
        body["parallel_tool_calls"] = parallel_tool_calls.clone();
    }
    if let Some(max_tokens) = request
        .get("max_completion_tokens")
        .or_else(|| request.get("max_output_tokens"))
        .or_else(|| request.get("max_tokens"))
    {
        body["max_completion_tokens"] = max_tokens.clone();
    }
    for key in [
        "temperature",
        "top_p",
        "stop",
        "presence_penalty",
        "frequency_penalty",
        "logit_bias",
        "logprobs",
        "top_logprobs",
        "n",
        "seed",
        "user",
        "metadata",
        "store",
        "service_tier",
    ] {
        if let Some(value) = request.get(key) {
            body[key] = value.clone();
        }
    }
    if let Some(stream_options) = request.get("stream_options") {
        body["stream_options"] = stream_options.clone();
    }
    if let Some(response_format) = response_format.response_format {
        body["response_format"] = response_format;
    }
    body
}

fn chat_messages_from_request(request: &Value) -> Vec<Value> {
    let mut messages = Vec::new();
    if let Some(instructions) = request.get("instructions").and_then(Value::as_str) {
        if !instructions.is_empty() {
            messages.push(json!({ "role": "system", "content": instructions }));
        }
    }
    let source = request.get("input").or_else(|| request.get("messages"));
    match source {
        Some(Value::String(text)) => messages.push(json!({ "role": "user", "content": text })),
        Some(Value::Array(items)) => {
            let mut pending_reasoning: Option<String> = None;
            for item in items {
                if let Some(reasoning) = chat_reasoning_text_from_input_item(item) {
                    pending_reasoning = Some(reasoning);
                    continue;
                }
                append_chat_message_from_input_item(&mut messages, item, &mut pending_reasoning);
            }
        }
        _ => messages.push(json!({ "role": "user", "content": "" })),
    }
    let mut messages = normalize_chat_messages(messages);
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": "" }));
    }
    messages
}

fn append_chat_message_from_input_item(
    messages: &mut Vec<Value>,
    item: &Value,
    pending_reasoning: &mut Option<String>,
) {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            append_chat_function_call_message(messages, item, pending_reasoning.take().as_deref());
        }
        Some("function_call_output") => {
            if let Some(message) = chat_tool_message_from_function_call_output(item) {
                messages.push(message);
            }
            *pending_reasoning = None;
        }
        Some("input_text") | Some("text") => {
            let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
            messages.push(json!({ "role": "user", "content": text }));
            *pending_reasoning = None;
        }
        Some("input_image") | Some("image") | Some("image_url") => {
            if let Some(content) = chat_content_from_content(item) {
                messages.push(json!({ "role": "user", "content": content }));
            }
            *pending_reasoning = None;
        }
        Some("message") | None => {
            if let Some(message) =
                chat_message_from_message_item(item, pending_reasoning.as_deref())
            {
                if is_chat_assistant_like_input_item(item) {
                    *pending_reasoning = None;
                } else if chat_message_role(&message) != Some("assistant") {
                    *pending_reasoning = None;
                }
                messages.push(message);
            } else if !is_chat_assistant_like_input_item(item) {
                *pending_reasoning = None;
            }
        }
        _ => {
            *pending_reasoning = None;
        }
    }
}

fn chat_message_from_message_item(item: &Value, pending_reasoning: Option<&str>) -> Option<Value> {
    let reasoning_content = item
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or(pending_reasoning);

    let role = normalize_message_role(item.get("role").and_then(Value::as_str));
    let role = match role {
        "assistant" => "assistant",
        "system" => "system",
        "tool" => "tool",
        _ => "user",
    };
    let raw_content = item.get("content").unwrap_or(item);
    let content_value = chat_content_from_content(raw_content);
    if role == "assistant" {
        let tool_calls = chat_tool_calls_from_message_item(item);
        if content_value.is_none() && reasoning_content.is_none() && tool_calls.is_none() {
            return None;
        }
        let mut message =
            json!({ "role": "assistant", "content": content_value.unwrap_or(Value::Null) });
        if let Some(tool_calls) = tool_calls {
            message["tool_calls"] = tool_calls;
        }
        attach_reasoning_content(&mut message, reasoning_content);
        Some(message)
    } else if role == "tool" {
        let call_id = item
            .get("tool_call_id")
            .or_else(|| item.get("call_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        Some(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": text_from_content(raw_content)
        }))
    } else {
        Some(json!({ "role": role, "content": content_value.unwrap_or_else(|| json!("")) }))
    }
}

fn is_chat_assistant_like_input_item(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("function_call")
        || normalize_message_role(item.get("role").and_then(Value::as_str)) == "assistant"
}

fn append_chat_function_call_message(
    messages: &mut Vec<Value>,
    item: &Value,
    reasoning_content: Option<&str>,
) {
    let Some(tool_call) = chat_tool_call_from_function_call_item(item) else {
        return;
    };
    if let Some(last) = messages
        .last_mut()
        .filter(|message| chat_message_role(message) == Some("assistant"))
    {
        if last.get("tool_calls").and_then(Value::as_array).is_none() {
            last["tool_calls"] = Value::Array(Vec::new());
        }
        if last.get("content").is_none() {
            last["content"] = Value::Null;
        }
        if let Some(calls) = last.get_mut("tool_calls").and_then(Value::as_array_mut) {
            calls.push(tool_call);
        }
        if last
            .get("reasoning_content")
            .and_then(Value::as_str)
            .is_none()
        {
            attach_reasoning_content(last, reasoning_content);
        }
        return;
    }

    let mut message = json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [tool_call]
    });
    attach_reasoning_content(&mut message, reasoning_content);
    messages.push(message);
}

fn chat_tool_call_from_function_call_item(item: &Value) -> Option<Value> {
    let call_id = item.get("call_id").and_then(Value::as_str)?;
    let name = item.get("name").and_then(Value::as_str)?;
    let arguments = value_to_argument_string(item.get("arguments"));
    Some(json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": if arguments.trim().is_empty() { "{}" } else { arguments.as_str() }
        }
    }))
}

fn chat_tool_message_from_function_call_output(item: &Value) -> Option<Value> {
    let call_id = item.get("call_id").and_then(Value::as_str)?;
    let output = value_to_text(
        item.get("output")
            .or_else(|| item.get("content"))
            .unwrap_or(&Value::Null),
    );
    Some(json!({
        "role": "tool",
        "tool_call_id": call_id,
        "content": output
    }))
}

fn chat_tool_calls_from_message_item(item: &Value) -> Option<Value> {
    let calls = item.get("tool_calls").and_then(Value::as_array)?;
    let normalized = calls
        .iter()
        .filter_map(normalize_chat_tool_call)
        .collect::<Vec<_>>();
    (!normalized.is_empty()).then(|| Value::Array(normalized))
}

fn normalize_chat_tool_call(call: &Value) -> Option<Value> {
    let call_id = call.get("id").and_then(Value::as_str).unwrap_or_default();
    let name = call.pointer("/function/name").and_then(Value::as_str)?;
    let arguments = call
        .pointer("/function/arguments")
        .map(|value| value_to_argument_string(Some(value)))
        .unwrap_or_else(|| "{}".to_string());
    Some(json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": if arguments.trim().is_empty() { "{}" } else { arguments.as_str() }
        }
    }))
}

fn attach_reasoning_content(message: &mut Value, reasoning_content: Option<&str>) {
    if let Some(reasoning_content) = reasoning_content.filter(|value| !value.is_empty()) {
        message["reasoning_content"] = json!(reasoning_content);
    }
}

fn chat_reasoning_text_from_input_item(item: &Value) -> Option<String> {
    chat_reasoning_content_from_input_item(item).or_else(|| {
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            return None;
        }
        let mut parts = Vec::new();
        collect_chat_reasoning_text_parts(item.get("summary"), &mut parts);
        collect_chat_reasoning_text_parts(item.get("content"), &mut parts);
        let text = parts.join("\n");
        (!text.is_empty()).then_some(text)
    })
}

fn collect_chat_reasoning_text_parts(value: Option<&Value>, parts: &mut Vec<String>) {
    match value {
        Some(Value::String(text)) if !text.is_empty() => parts.push(text.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item
                    .get("text")
                    .or_else(|| item.get("summary_text"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    parts.push(text.to_string());
                }
            }
        }
        Some(Value::Object(item)) => {
            if let Some(text) = item
                .get("text")
                .or_else(|| item.get("summary_text"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            {
                parts.push(text.to_string());
            }
        }
        _ => {}
    }
}

fn chat_reasoning_content_from_input_item(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }
    let encoded = item
        .get("encrypted_content")
        .and_then(Value::as_str)?
        .strip_prefix(CHAT_REASONING_PREFIX)?;
    let bytes = BASE64_STANDARD.decode(encoded).ok()?;
    String::from_utf8(bytes)
        .ok()
        .filter(|value| !value.is_empty())
}

fn normalize_chat_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut tool_replies = HashMap::<String, Value>::new();
    for message in &messages {
        if chat_message_role(message) == Some("tool") {
            if let Some(call_id) = message.get("tool_call_id").and_then(Value::as_str) {
                if !call_id.is_empty() {
                    tool_replies.insert(call_id.to_string(), message.clone());
                }
            }
        }
    }

    let mut out = Vec::new();
    for mut message in messages {
        if chat_message_role(&message) == Some("tool") {
            continue;
        }
        let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array).cloned() else {
            push_normalized_chat_message(&mut out, message);
            continue;
        };
        let kept = tool_calls
            .into_iter()
            .filter(|call| {
                call.get("id")
                    .and_then(Value::as_str)
                    .map(|call_id| tool_replies.contains_key(call_id))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        if kept.is_empty() {
            message
                .as_object_mut()
                .map(|object| object.remove("tool_calls"));
            if !chat_message_has_usable_content(&message)
                && message
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .is_empty()
            {
                continue;
            }
            push_normalized_chat_message(&mut out, message);
            continue;
        }
        message["tool_calls"] = Value::Array(kept.clone());
        push_normalized_chat_message(&mut out, message);
        for call in kept {
            if let Some(call_id) = call.get("id").and_then(Value::as_str) {
                if let Some(reply) = tool_replies.get(call_id) {
                    out.push(reply.clone());
                }
            }
        }
    }
    out
}

fn push_normalized_chat_message(out: &mut Vec<Value>, message: Value) {
    if chat_message_role(&message) == Some("assistant")
        && !chat_message_has_tool_calls(&message)
        && out
            .last()
            .map(|last| {
                chat_message_role(last) == Some("assistant") && !chat_message_has_tool_calls(last)
            })
            .unwrap_or(false)
    {
        if let Some(last) = out.last_mut() {
            merge_plain_assistant_messages(last, message);
            return;
        }
    }
    out.push(message);
}

fn merge_plain_assistant_messages(target: &mut Value, source: Value) {
    let left = target
        .get("content")
        .map(text_from_chat_content)
        .unwrap_or_default();
    let right = source
        .get("content")
        .map(text_from_chat_content)
        .unwrap_or_default();
    let joined = [left, right]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    target["content"] = json!(joined);
    if target
        .get("reasoning_content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .is_empty()
    {
        if let Some(reasoning) = source
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            target["reasoning_content"] = json!(reasoning);
        }
    }
}

fn chat_message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

fn chat_message_has_tool_calls(message: &Value) -> bool {
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| !calls.is_empty())
        .unwrap_or(false)
}

fn chat_message_has_usable_content(message: &Value) -> bool {
    match message.get("content") {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(items)) => items.iter().any(|item| {
            item.get("text")
                .or_else(|| item.get("content"))
                .and_then(Value::as_str)
                .map(|text| !text.trim().is_empty())
                .unwrap_or(false)
                || item.get("image_url").is_some()
        }),
        Some(Value::Object(_)) => true,
        _ => false,
    }
}

fn chat_tools_from_request(request: &Value) -> Vec<Value> {
    let mut tools: Vec<Value> = request
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(chat_tool_from_responses_tool)
                .collect()
        })
        .unwrap_or_default();
    if let Some(functions) = request.get("functions").and_then(Value::as_array) {
        tools.extend(functions.iter().filter_map(chat_tool_from_legacy_function));
    }
    tools
}

fn chat_tool_from_responses_tool(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    let name = tool
        .get("name")
        .or_else(|| tool.pointer("/function/name"))
        .and_then(Value::as_str)?;
    let description = tool
        .get("description")
        .or_else(|| tool.pointer("/function/description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parameters = tool
        .get("parameters")
        .or_else(|| tool.pointer("/function/parameters"))
        .cloned()
        .unwrap_or_else(default_tool_parameters);
    let strict = tool
        .get("strict")
        .or_else(|| tool.pointer("/function/strict"))
        .cloned()
        .unwrap_or_else(|| json!(false));
    let function = json!({
        "name": name,
        "description": description,
        "parameters": parameters,
        "strict": strict
    });
    Some(json!({
        "type": "function",
        "function": function
    }))
}

fn chat_tool_from_legacy_function(function: &Value) -> Option<Value> {
    let name = function.get("name").and_then(Value::as_str)?;
    let description = function
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parameters = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(default_tool_parameters);
    let strict = function
        .get("strict")
        .cloned()
        .unwrap_or_else(|| json!(false));
    Some(json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
            "strict": strict
        }
    }))
}

fn chat_tool_choice_from_request(request: &Value) -> Option<Value> {
    request
        .get("tool_choice")
        .and_then(chat_tool_choice_value)
        .or_else(|| {
            request
                .get("function_call")
                .and_then(chat_function_call_choice_value)
        })
}

fn chat_tool_choice_value(choice: &Value) -> Option<Value> {
    match choice {
        Value::String(value) if matches!(value.as_str(), "auto" | "none" | "required") => {
            Some(choice.clone())
        }
        Value::Object(_) => {
            if choice.get("type").and_then(Value::as_str) == Some("function") {
                if let Some(name) = choice
                    .get("name")
                    .or_else(|| choice.pointer("/function/name"))
                    .and_then(Value::as_str)
                {
                    return Some(json!({
                        "type": "function",
                        "function": { "name": name }
                    }));
                }
            }
            None
        }
        _ => None,
    }
}

fn chat_function_call_choice_value(choice: &Value) -> Option<Value> {
    match choice {
        Value::String(value) if matches!(value.as_str(), "auto" | "none") => Some(choice.clone()),
        Value::Object(_) => choice.get("name").and_then(Value::as_str).map(|name| {
            json!({
                "type": "function",
                "function": { "name": name }
            })
        }),
        _ => None,
    }
}

fn chat_response_format_from_request(
    request: &Value,
    profile: ChatCompletionsCompatibilityProfile,
) -> ChatResponseFormatSelection {
    let Some(format) = request
        .get("response_format")
        .or_else(|| request.pointer("/text/format"))
    else {
        return ChatResponseFormatSelection::default();
    };
    match format.get("type").and_then(Value::as_str) {
        Some("json_object") => ChatResponseFormatSelection {
            response_format: Some(json!({ "type": "json_object" })),
            requires_json_prompt: true,
            schema_hint: None,
        },
        Some("json_schema") => {
            let schema = chat_json_schema_response_format(format);
            match profile {
                ChatCompletionsCompatibilityProfile::JsonSchemaCapable => {
                    ChatResponseFormatSelection {
                        response_format: schema,
                        requires_json_prompt: false,
                        schema_hint: None,
                    }
                }
                ChatCompletionsCompatibilityProfile::JsonObjectOnly => {
                    ChatResponseFormatSelection {
                        response_format: Some(json!({ "type": "json_object" })),
                        requires_json_prompt: true,
                        schema_hint: chat_json_schema_hint(format),
                    }
                }
            }
        }
        Some("text") => ChatResponseFormatSelection::default(),
        _ => ChatResponseFormatSelection::default(),
    }
}

fn chat_json_schema_response_format(format: &Value) -> Option<Value> {
    if format.get("json_schema").is_some() {
        return Some(format.clone());
    }
    let json_schema = chat_json_schema_object(format)?;
    Some(json!({
        "type": "json_schema",
        "json_schema": json_schema
    }))
}

fn chat_json_schema_object(format: &Value) -> Option<Value> {
    if let Some(json_schema) = format.get("json_schema") {
        return Some(json_schema.clone());
    }
    let mut json_schema = serde_json::Map::new();
    for key in ["name", "description", "schema", "strict"] {
        if let Some(value) = format.get(key) {
            json_schema.insert(key.to_string(), value.clone());
        }
    }
    (!json_schema.is_empty()).then(|| Value::Object(json_schema))
}

fn chat_json_schema_hint(format: &Value) -> Option<String> {
    let schema = chat_json_schema_object(format)?;
    serde_json::to_string(&schema).ok()
}

fn apply_chat_response_format_prompt_hints(
    messages: &mut Vec<Value>,
    selection: &ChatResponseFormatSelection,
) {
    if !selection.requires_json_prompt && selection.schema_hint.is_none() {
        return;
    }
    let mut hint = String::new();
    if selection.requires_json_prompt && !chat_messages_mention_json(messages) {
        hint.push_str("Return valid JSON only.");
    }
    if let Some(schema_hint) = selection.schema_hint.as_deref() {
        if !hint.is_empty() {
            hint.push('\n');
        }
        hint.push_str("Return JSON matching this schema:\n");
        hint.push_str(schema_hint);
    }
    if !hint.is_empty() {
        messages.push(json!({ "role": "system", "content": hint }));
    }
}

fn chat_messages_mention_json(messages: &[Value]) -> bool {
    messages.iter().any(|message| {
        message
            .get("content")
            .map(text_from_chat_content)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("json")
    })
}

fn chat_body_without_response_format(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.remove("response_format");
    }
    body
}

fn should_retry_chat_without_response_format(
    status: reqwest::StatusCode,
    text: &str,
    body: &Value,
) -> bool {
    if status.as_u16() != StatusCode::BAD_REQUEST.as_u16() || body.get("response_format").is_none()
    {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let format_related = lower.contains("response_format") || lower.contains("json_schema");
    let retryable_reason = lower.contains("unavailable")
        || lower.contains("unsupported")
        || lower.contains("not support")
        || lower.contains("not supported")
        || lower.contains("invalid_request_error");
    format_related && retryable_reason
}

fn anthropic_messages_from_request(request: &Value) -> (Option<String>, Vec<Value>) {
    let mut system_parts = Vec::new();
    if let Some(instructions) = request
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    {
        system_parts.push(instructions);
    }
    let mut messages = Vec::new();
    let mut current_role: Option<&'static str> = None;
    let mut current_parts = Vec::<Value>::new();
    let mut pending_thinking: Option<Value> = None;
    let mut pending_system_messages = Vec::<String>::new();
    let source = request.get("input").or_else(|| request.get("messages"));
    match source {
        Some(Value::String(text)) => {
            append_anthropic_message_parts(
                &mut messages,
                &mut current_role,
                &mut current_parts,
                &mut pending_system_messages,
                "user",
                vec![json!({ "type": "text", "text": text })],
                &mut pending_thinking,
            );
        }
        Some(Value::Array(items)) => {
            for item in items {
                append_anthropic_input_item(
                    &mut system_parts,
                    &mut messages,
                    &mut current_role,
                    &mut current_parts,
                    &mut pending_thinking,
                    &mut pending_system_messages,
                    item,
                );
            }
        }
        _ => {
            append_anthropic_message_parts(
                &mut messages,
                &mut current_role,
                &mut current_parts,
                &mut pending_system_messages,
                "user",
                vec![json!({ "type": "text", "text": "" })],
                &mut pending_thinking,
            );
        }
    }
    let flushed_role =
        flush_anthropic_message(&mut messages, &mut current_role, &mut current_parts);
    let _ = flushed_role;
    flush_pending_anthropic_system_messages(&mut messages, &mut pending_system_messages);
    if !pending_system_messages.is_empty() {
        system_parts.append(&mut pending_system_messages);
    }
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": "" }));
    }
    let system_instructions = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    (system_instructions, messages)
}

fn append_anthropic_input_item(
    system_parts: &mut Vec<String>,
    messages: &mut Vec<Value>,
    current_role: &mut Option<&'static str>,
    current_parts: &mut Vec<Value>,
    pending_thinking: &mut Option<Value>,
    pending_system_messages: &mut Vec<String>,
    item: &Value,
) {
    match item.get("type").and_then(Value::as_str) {
        Some("reasoning") => {
            *pending_thinking = anthropic_thinking_content_from_input_item(item);
        }
        Some("function_call") => {
            let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                return;
            };
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                return;
            };
            append_anthropic_message_parts(
                messages,
                current_role,
                current_parts,
                pending_system_messages,
                "assistant",
                vec![json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": arguments_to_object(item.get("arguments"))
                })],
                pending_thinking,
            );
        }
        Some("function_call_output") => {
            let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                return;
            };
            append_anthropic_message_parts(
                messages,
                current_role,
                current_parts,
                pending_system_messages,
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": value_to_text(item.get("output").or_else(|| item.get("content")).unwrap_or(&Value::Null))
                })],
                pending_thinking,
            );
        }
        _ => {
            let role = normalize_message_role(item.get("role").and_then(Value::as_str));
            let raw_content = item.get("content").unwrap_or(item);
            if role == "system" {
                *pending_thinking = None;
                let text = text_from_content(raw_content);
                if !text.is_empty() {
                    let is_initial_system =
                        messages.is_empty() && current_role.is_none() && current_parts.is_empty();
                    if is_initial_system {
                        system_parts.push(text);
                    } else {
                        pending_system_messages.push(text);
                    }
                }
                return;
            }
            let mut parts = anthropic_content_parts_from_content(raw_content);
            if parts.is_empty() && role != "assistant" {
                return;
            }
            let role = if role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            if parts.is_empty() && pending_thinking.is_none() {
                return;
            }
            append_anthropic_message_parts(
                messages,
                current_role,
                current_parts,
                pending_system_messages,
                role,
                std::mem::take(&mut parts),
                pending_thinking,
            );
        }
    }
}

fn append_anthropic_message_parts(
    messages: &mut Vec<Value>,
    current_role: &mut Option<&'static str>,
    current_parts: &mut Vec<Value>,
    pending_system_messages: &mut Vec<String>,
    role: &'static str,
    mut parts: Vec<Value>,
    pending_thinking: &mut Option<Value>,
) {
    if *current_role != Some(role) {
        let flushed_role = flush_anthropic_message(messages, current_role, current_parts);
        let _ = flushed_role;
        flush_pending_anthropic_system_messages(messages, pending_system_messages);
        *current_role = Some(role);
    }
    if role == "assistant" {
        if let Some(thinking) = pending_thinking.take() {
            current_parts.push(thinking);
        }
    } else {
        *pending_thinking = None;
    }
    current_parts.append(&mut parts);
}

fn flush_anthropic_message(
    messages: &mut Vec<Value>,
    current_role: &mut Option<&'static str>,
    current_parts: &mut Vec<Value>,
) -> Option<&'static str> {
    let Some(role) = current_role.take() else {
        return None;
    };
    if current_parts.is_empty() {
        return None;
    }
    let parts = std::mem::take(current_parts);
    let content = if parts.len() == 1
        && parts[0].get("type").and_then(Value::as_str) == Some("text")
        && !parts[0]
            .as_object()
            .map(|object| object.contains_key("cache_control"))
            .unwrap_or(false)
    {
        parts[0]
            .get("text")
            .and_then(Value::as_str)
            .map(|value| Value::String(value.to_string()))
            .unwrap_or_else(|| Value::Array(parts))
    } else {
        Value::Array(parts)
    };
    messages.push(json!({ "role": role, "content": content }));
    Some(role)
}

fn flush_pending_anthropic_system_messages(messages: &mut Vec<Value>, pending: &mut Vec<String>) {
    if pending.is_empty() {
        return;
    }
    if messages
        .last()
        .and_then(|message| message.get("role").and_then(Value::as_str))
        != Some("user")
    {
        return;
    }
    for text in pending.drain(..) {
        messages.push(json!({ "role": "system", "content": text }));
    }
}

fn anthropic_content_parts_from_content(value: &Value) -> Vec<Value> {
    match anthropic_content_from_content(value) {
        Some(Value::String(text)) => vec![json!({ "type": "text", "text": text })],
        Some(Value::Array(parts)) => parts,
        _ => Vec::new(),
    }
}

fn anthropic_thinking_content_from_input_item(item: &Value) -> Option<Value> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }
    let encoded = item
        .get("encrypted_content")
        .and_then(Value::as_str)?
        .strip_prefix(ANTHROPIC_THINKING_PREFIX)?;
    let bytes = BASE64_STANDARD.decode(encoded).ok()?;
    let value = serde_json::from_slice::<Value>(&bytes).ok()?;
    let thinking = value
        .get("thinking")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())?;
    let signature = value
        .get("signature")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())?;
    Some(json!({
        "type": "thinking",
        "thinking": thinking,
        "signature": signature
    }))
}

fn anthropic_tools_from_request(request: &Value) -> Vec<Value> {
    request
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(anthropic_tool_from_responses_tool)
                .collect()
        })
        .unwrap_or_default()
}

fn anthropic_tool_from_responses_tool(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    let name = tool
        .get("name")
        .or_else(|| tool.pointer("/function/name"))
        .and_then(Value::as_str)?;
    let description = tool
        .get("description")
        .or_else(|| tool.pointer("/function/description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input_schema = tool
        .get("parameters")
        .or_else(|| tool.pointer("/function/parameters"))
        .cloned()
        .unwrap_or_else(default_tool_parameters);
    Some(json!({
        "name": name,
        "description": description,
        "input_schema": input_schema
    }))
}

fn default_tool_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {}
    })
}

fn normalize_message_role(role: Option<&str>) -> &'static str {
    match role.unwrap_or("user") {
        "assistant" => "assistant",
        "tool" => "tool",
        "system" | "developer" | "latest_reminder" => "system",
        _ => "user",
    }
}

fn chat_content_from_content(value: &Value) -> Option<Value> {
    multimodal_content_from_content(value, MultimodalTarget::OpenAiChat)
}

fn anthropic_content_from_content(value: &Value) -> Option<Value> {
    multimodal_content_from_content(value, MultimodalTarget::Anthropic)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MultimodalTarget {
    OpenAiChat,
    Anthropic,
}

fn multimodal_content_from_content(value: &Value, target: MultimodalTarget) -> Option<Value> {
    match value {
        Value::String(text) => non_empty_string(text).map(Value::String),
        Value::Array(items) => {
            let mut parts = Vec::new();
            let mut has_non_text = false;
            for item in items {
                if let Some(text) = text_from_content_item(item) {
                    push_multimodal_text_part(&mut parts, target, text);
                    continue;
                }
                if is_image_content_item(item) {
                    has_non_text = true;
                    if target == MultimodalTarget::OpenAiChat && is_empty_base64_image_item(item) {
                        continue;
                    }
                    if let Some(part) = multimodal_image_part(item, target) {
                        parts.push(part);
                    } else {
                        push_multimodal_text_part(
                            &mut parts,
                            target,
                            unsupported_image_message(item, target),
                        );
                    }
                    continue;
                }
                if is_file_content_item(item) {
                    has_non_text = true;
                    push_multimodal_text_part(&mut parts, target, unsupported_file_message(item));
                }
            }
            multimodal_parts_to_content(parts, has_non_text, target)
        }
        Value::Object(_) => {
            if let Some(text) = text_from_content_item(value) {
                return Some(Value::String(text));
            }
            if is_image_content_item(value) {
                if target == MultimodalTarget::OpenAiChat && is_empty_base64_image_item(value) {
                    return None;
                }
                return multimodal_image_part(value, target)
                    .map(|part| Value::Array(vec![part]))
                    .or_else(|| {
                        Some(Value::Array(vec![multimodal_text_part(
                            target,
                            unsupported_image_message(value, target),
                        )]))
                    });
            }
            if is_file_content_item(value) {
                return Some(Value::String(unsupported_file_message(value)));
            }
            None
        }
        _ => None,
    }
}

fn multimodal_parts_to_content(
    parts: Vec<Value>,
    has_non_text: bool,
    target: MultimodalTarget,
) -> Option<Value> {
    if parts.is_empty() {
        return None;
    }
    if has_non_text {
        return Some(Value::Array(parts));
    }
    Some(Value::String(
        parts
            .iter()
            .filter_map(|part| match target {
                MultimodalTarget::OpenAiChat => part.get("text").and_then(Value::as_str),
                MultimodalTarget::Anthropic => part.get("text").and_then(Value::as_str),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    ))
}

fn push_multimodal_text_part(parts: &mut Vec<Value>, target: MultimodalTarget, text: String) {
    if !text.is_empty() {
        parts.push(multimodal_text_part(target, text));
    }
}

fn multimodal_text_part(target: MultimodalTarget, text: String) -> Value {
    match target {
        MultimodalTarget::OpenAiChat => json!({ "type": "text", "text": text }),
        MultimodalTarget::Anthropic => json!({ "type": "text", "text": text }),
    }
}

fn multimodal_image_part(item: &Value, target: MultimodalTarget) -> Option<Value> {
    match target {
        MultimodalTarget::OpenAiChat => image_url_from_content_item(item).map(|url| {
            json!({
                "type": "image_url",
                "image_url": { "url": url }
            })
        }),
        MultimodalTarget::Anthropic => {
            anthropic_image_source_from_content_item(item).map(|source| {
                json!({
                    "type": "image",
                    "source": source
                })
            })
        }
    }
}

fn text_from_content_item(item: &Value) -> Option<String> {
    match item {
        Value::String(text) => non_empty_string(text),
        Value::Object(_) => item
            .get("text")
            .or_else(|| item.get("input_text"))
            .or_else(|| item.get("output_text"))
            .and_then(Value::as_str)
            .and_then(non_empty_string),
        _ => None,
    }
}

fn non_empty_string(text: &str) -> Option<String> {
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn is_image_content_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("input_image" | "image" | "image_url")
    ) || item.get("image_url").is_some()
}

fn is_file_content_item(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("input_file" | "file")
    ) || item.get("file_id").is_some()
        || item.get("filename").is_some()
}

fn image_url_from_content_item(item: &Value) -> Option<String> {
    item.get("image_url")
        .and_then(Value::as_str)
        .or_else(|| item.pointer("/image_url/url").and_then(Value::as_str))
        .or_else(|| item.get("url").and_then(Value::as_str))
        .or_else(|| item.get("data_url").and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| base64_source_to_data_url(item.get("source")?))
}

fn base64_source_to_data_url(source: &Value) -> Option<String> {
    let source_type = source.get("type").and_then(Value::as_str)?;
    if source_type != "base64" {
        return None;
    }
    let media_type = source
        .get("media_type")
        .and_then(Value::as_str)
        .filter(|media_type| media_type.to_ascii_lowercase().starts_with("image/"))?;
    let data = source.get("data").and_then(Value::as_str)?;
    Some(format!("data:{media_type};base64,{data}"))
}

fn anthropic_image_source_from_content_item(item: &Value) -> Option<Value> {
    if let Some(source) = item.get("source") {
        if source.get("type").and_then(Value::as_str) == Some("base64") {
            let media_type = source.get("media_type").and_then(Value::as_str)?;
            if !media_type.to_ascii_lowercase().starts_with("image/") {
                return None;
            }
            let data = source.get("data").and_then(Value::as_str)?;
            return Some(json!({
                "type": "base64",
                "media_type": media_type,
                "data": data
            }));
        }
    }
    let image_url = image_url_from_content_item(item)?;
    let (media_type, data) = parse_image_data_url(&image_url)?;
    Some(json!({
        "type": "base64",
        "media_type": media_type,
        "data": data
    }))
}

fn parse_image_data_url(url: &str) -> Option<(String, String)> {
    let (metadata, data) = url.strip_prefix("data:")?.split_once(',')?;
    let metadata_lower = metadata.to_ascii_lowercase();
    if !metadata_lower.contains(";base64") {
        return None;
    }
    let media_type = metadata
        .split(';')
        .next()
        .filter(|media_type| media_type.to_ascii_lowercase().starts_with("image/"))?;
    Some((media_type.to_string(), data.to_string()))
}

fn is_empty_base64_image_item(item: &Value) -> bool {
    image_url_from_content_item(item)
        .map(|url| is_empty_base64_data_url(&url))
        .unwrap_or(false)
}

fn is_empty_base64_data_url(url: &str) -> bool {
    let Some((metadata, data)) = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
    else {
        return false;
    };
    metadata.to_ascii_lowercase().contains(";base64") && data.trim().is_empty()
}

fn unsupported_image_message(item: &Value, target: MultimodalTarget) -> String {
    let detail = item
        .get("file_id")
        .or_else(|| item.get("image_file_id"))
        .and_then(Value::as_str)
        .map(|id| format!(" file_id={id}"))
        .unwrap_or_default();
    match target {
        MultimodalTarget::OpenAiChat => {
            format!("[unsupported image input{detail}: Chat Completions routes need an image_url or data URL]")
        }
        MultimodalTarget::Anthropic => {
            format!(
                "[unsupported image input{detail}: Anthropic routes need a base64 data URL image]"
            )
        }
    }
}

fn unsupported_file_message(item: &Value) -> String {
    let detail = item
        .get("filename")
        .or_else(|| item.get("file_id"))
        .and_then(Value::as_str)
        .map(|value| format!(" {value}"))
        .unwrap_or_default();
    format!("[unsupported file input{detail}: file inputs require an OpenAI Responses route]")
}

fn text_from_content(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .or_else(|| item.get("input_text"))
                    .or_else(|| item.get("output_text"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => value
            .get("text")
            .or_else(|| value.get("input_text"))
            .or_else(|| value.get("output_text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

fn value_to_argument_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => "{}".to_string(),
        Some(value) => value.to_string(),
    }
}

fn arguments_to_object(value: Option<&Value>) -> Value {
    let parsed = match value {
        Some(Value::String(text)) if text.trim().is_empty() => json!({}),
        Some(Value::String(text)) => serde_json::from_str::<Value>(text).unwrap_or_else(|_| {
            json!({
                "input": text
            })
        }),
        Some(value) => value.clone(),
        None => json!({}),
    };
    if parsed.is_object() {
        parsed
    } else {
        json!({
            "input": parsed
        })
    }
}

fn text_from_chat_content(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .or_else(|| item.get("content"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn chat_completion_response_json(
    request_id: &str,
    model: &str,
    value: &Value,
    usage: Option<Value>,
) -> Value {
    let status = chat_completion_response_status(value);
    let usage = usage.map(|usage| {
        usage_to_responses_json(parse_usage(ProviderProtocol::OpenAiChatCompletions, &usage))
    });
    response_object_with_incomplete_details(
        request_id,
        model,
        status,
        chat_completion_output_items(value),
        usage,
        chat_completion_incomplete_details(status),
    )
}

fn chat_completion_output_items(value: &Value) -> Vec<Value> {
    let mut output = Vec::new();
    let message = value.pointer("/choices/0/message").unwrap_or(&Value::Null);
    if let Some(reasoning_content) = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        output.push(chat_reasoning_output_item("rsn_0", reasoning_content));
    }
    let text = message
        .get("content")
        .map(text_from_chat_content)
        .unwrap_or_default();
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, tool_call) in tool_calls.iter().enumerate() {
            if tool_call.get("type").and_then(Value::as_str) != Some("function") {
                continue;
            }
            let call_id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("call_{index}"));
            let Some(name) = tool_call.pointer("/function/name").and_then(Value::as_str) else {
                continue;
            };
            let arguments = tool_call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("{}");
            output.push(function_call_item(
                &format!("fc_{call_id}"),
                &call_id,
                name,
                arguments,
                "completed",
            ));
        }
    }
    let has_tool_calls = output
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"));
    if !text.is_empty() {
        let insert_at = output
            .iter()
            .position(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .unwrap_or(output.len());
        output.insert(
            insert_at,
            message_output_item("msg_0", "completed", Some(&text)),
        );
    } else if !has_tool_calls {
        if let Some(reasoning_content) = message
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            output.push(message_output_item(
                "msg_0",
                "completed",
                Some(reasoning_content),
            ));
        }
    }
    if output.is_empty() {
        output.push(message_output_item("msg_0", "completed", Some("")));
    }
    output
}

fn chat_completion_response_status(value: &Value) -> &'static str {
    match value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
    {
        Some("length") => "incomplete",
        _ => "completed",
    }
}

fn chat_completion_incomplete_details(status: &str) -> Option<Value> {
    (status == "incomplete").then(|| json!({ "reason": "max_output_tokens" }))
}

fn anthropic_response_json(
    request_id: &str,
    model: &str,
    value: &Value,
    usage: Option<Value>,
) -> Value {
    let (status, incomplete_details) = anthropic_response_status(value);
    response_object_with_incomplete_details(
        request_id,
        model,
        status,
        anthropic_output_items(value),
        usage,
        incomplete_details,
    )
}

fn anthropic_response_status(value: &Value) -> (&'static str, Option<Value>) {
    match value.get("stop_reason").and_then(Value::as_str) {
        Some("max_tokens") => ("incomplete", incomplete_details("max_output_tokens")),
        _ => ("completed", None),
    }
}

fn anthropic_output_items(value: &Value) -> Vec<Value> {
    let mut output = Vec::new();
    let mut text_index = 0_usize;
    let mut reasoning_index = 0_usize;
    if let Some(items) = value.get("content").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    let Some(thinking) = item.get("thinking").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(signature) = item.get("signature").and_then(Value::as_str) else {
                        continue;
                    };
                    if !thinking.is_empty() && !signature.is_empty() {
                        output.push(anthropic_reasoning_output_item(
                            &format!("rsn_{reasoning_index}"),
                            thinking,
                            signature,
                        ));
                        reasoning_index += 1;
                    }
                }
                Some("text") => {
                    let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                    if !text.is_empty() {
                        output.push(message_output_item(
                            &format!("msg_{text_index}"),
                            "completed",
                            Some(text),
                        ));
                        text_index += 1;
                    }
                }
                Some("tool_use") => {
                    let Some(call_id) = item.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(name) = item.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let arguments = item
                        .get("input")
                        .map(|value| value_to_argument_string(Some(value)))
                        .unwrap_or_else(|| "{}".to_string());
                    output.push(function_call_item(
                        &format!("fc_{call_id}"),
                        call_id,
                        name,
                        &arguments,
                        "completed",
                    ));
                }
                _ => {}
            }
        }
    }
    if output.is_empty() {
        output.push(message_output_item("msg_0", "completed", Some("")));
    }
    output
}

fn response_object(
    request_id: &str,
    model: &str,
    status: &str,
    output: Vec<Value>,
    usage: Option<Value>,
) -> Value {
    response_object_with_incomplete_details(request_id, model, status, output, usage, None)
}

fn response_object_with_incomplete_details(
    request_id: &str,
    model: &str,
    status: &str,
    output: Vec<Value>,
    usage: Option<Value>,
    incomplete_details: Option<Value>,
) -> Value {
    let output_text = output_text_from_items(&output);
    let mut response = json!({
        "id": format!("resp_{request_id}"),
        "object": "response",
        "created_at": Utc::now().timestamp(),
        "status": status,
        "model": model,
        "output": output,
        "output_text": output_text,
        "usage": usage
    });
    if let Some(incomplete_details) = incomplete_details {
        response["incomplete_details"] = incomplete_details;
    }
    response
}

fn output_text_from_items(output: &[Value]) -> String {
    output
        .iter()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter_map(|content| content.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("")
}

fn chat_reasoning_output_item(item_id: &str, reasoning_content: &str) -> Value {
    let encoded = BASE64_STANDARD.encode(reasoning_content.as_bytes());
    json!({
        "id": item_id,
        "type": "reasoning",
        "summary": [],
        "encrypted_content": format!("{CHAT_REASONING_PREFIX}{encoded}")
    })
}

fn chat_reasoning_stream_item(
    item_id: &str,
    status: &str,
    reasoning_content: Option<&str>,
) -> Value {
    let mut item = json!({
        "id": item_id,
        "type": "reasoning",
        "status": status,
        "summary": []
    });
    if let Some(reasoning_content) = reasoning_content {
        item["encrypted_content"] = json!(format!(
            "{CHAT_REASONING_PREFIX}{}",
            BASE64_STANDARD.encode(reasoning_content.as_bytes())
        ));
    }
    item
}

fn anthropic_reasoning_output_item(item_id: &str, thinking: &str, signature: &str) -> Value {
    let payload = json!({
        "thinking": thinking,
        "signature": signature
    });
    let encoded = BASE64_STANDARD.encode(payload.to_string().as_bytes());
    json!({
        "id": item_id,
        "type": "reasoning",
        "summary": [],
        "encrypted_content": format!("{ANTHROPIC_THINKING_PREFIX}{encoded}")
    })
}

fn message_output_item(item_id: &str, status: &str, text: Option<&str>) -> Value {
    let content = text
        .map(|value| vec![output_text_part(value)])
        .unwrap_or_default();
    json!({
        "id": item_id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": content
    })
}

fn output_text_part(text: &str) -> Value {
    json!({
        "type": "output_text",
        "text": text,
        "annotations": []
    })
}

fn function_call_item(
    item_id: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
    status: &str,
) -> Value {
    json!({
        "id": item_id,
        "type": "function_call",
        "status": status,
        "call_id": call_id,
        "name": name,
        "arguments": arguments
    })
}

fn endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn is_public_openai_api_key(auth: &str) -> bool {
    auth.trim()
        .strip_prefix("Bearer ")
        .or_else(|| auth.trim().strip_prefix("bearer "))
        .map(|token| token.starts_with("sk-"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{
        anthropic_messages_url, anthropic_model_for_request, anthropic_output_items,
        anthropic_response_json, anthropic_thinking_content_from_input_item,
        archived_tool_result_card, bridge_anthropic_context_body, build_anthropic_body,
        build_anthropic_count_tokens_body, build_chat_completions_body,
        build_chat_completions_body_with_profile, chat_body_without_response_format,
        chat_completion_output_items, chat_completion_response_json,
        chat_completions_compatibility_profile, chat_reasoning_content_from_input_item,
        chat_tool_call_deltas, classify_raw_stream_finish, claude_code_count_tokens_headers,
        claude_code_mirror_headers, collect_anthropic_tool_result_positions,
        context_bridge_diagnostics, context_full_error_message, converted_anthropic_sse,
        converted_chat_sse, endpoint, event_data_lines, extract_stream_delta, find_sse_boundary,
        forward_chat_completions, is_public_openai_api_key, json_size, lan_proxy_request_headers,
        map_openai_chat_reasoning_effort, map_reasoning_effort, official_responses_endpoint,
        post_bytes_with_retries, proxy_lan_models, proxy_raw, proxy_request_headers,
        response_stream_done_events, response_stream_start_events, responses_proxy_body,
        responses_proxy_url, route_response, should_precompress_from_pressure_sample,
        should_retry_chat_without_response_format, should_skip_proxy_request_header,
        summarize_tool_result, validate_lan_authorization, ChatCompletionsCompatibilityProfile,
        RawProxyContext, RawSseObserver, ResponsesAuthMode, RouteError, ServerState,
        CLAUDE_DESKTOP_COUNT_TOKENS_BETA, CLAUDE_DESKTOP_MESSAGES_BETA, RESPONSES_BODY_LIMIT_BYTES,
    };
    use crate::{
        router::{match_route, RouteMatch},
        store::AppStore,
        types::{
            default_config, ClaudeContextPressureSample, CodexInjectionMode, ContextArtifact,
            ModelEntry, Provider, ProviderKind, ProviderProtocol, RequestRecord, TokenUsage,
        },
    };
    use axum::{
        http::{header, HeaderMap, HeaderValue, StatusCode},
        response::IntoResponse,
    };
    use bytes::Bytes;
    use chrono::Utc;
    use futures_util::StreamExt;
    use reqwest::Client;
    use serde_json::{json, Value};
    use std::{
        net::SocketAddr,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    #[tokio::test]
    async fn responses_route_accepts_bodies_above_axum_default_limit() {
        let app = axum::Router::new().route(
            "/v1/responses",
            axum::routing::post(|body: Bytes| async move {
                assert!(body.len() > 2_097_152);
                assert!(body.len() < RESPONSES_BODY_LIMIT_BYTES);
                StatusCode::OK
            })
            .layer(axum::extract::DefaultBodyLimit::max(
                RESPONSES_BODY_LIMIT_BYTES,
            )),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/responses", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;

        let body = vec![b'a'; 2_097_153];
        let response = Client::new().post(url).body(body).send().await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        server.abort();
    }

    #[test]
    fn lan_auth_requires_bearer_key_for_non_loopback_clients() {
        let mut config = default_config();
        config.settings.allow_lan = true;
        config.settings.lan_api_key = "nr_test".into();
        let remote: SocketAddr = "192.168.1.30:50100".parse().unwrap();
        let mut headers = HeaderMap::new();

        let error = validate_lan_authorization(&config.settings, remote, &headers).unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED);

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer nr_test"),
        );
        assert!(validate_lan_authorization(&config.settings, remote, &headers).is_ok());
    }

    #[test]
    fn lan_auth_skips_loopback_clients() {
        let mut config = default_config();
        config.settings.allow_lan = true;
        config.settings.lan_api_key = "nr_test".into();
        let remote: SocketAddr = "127.0.0.1:50100".parse().unwrap();

        assert!(validate_lan_authorization(&config.settings, remote, &HeaderMap::new()).is_ok());
    }

    #[test]
    fn lan_proxy_headers_strip_inbound_authorization() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer codex"),
        );
        headers.insert("x-codex-test", HeaderValue::from_static("1"));

        let forwarded = lan_proxy_request_headers(&headers);

        assert!(!forwarded
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization")));
        assert!(forwarded
            .iter()
            .any(|(name, value)| name == "x-codex-test" && value == "1"));
    }

    #[tokio::test]
    async fn lan_models_proxy_returns_codex_slots() {
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(|headers: HeaderMap| async move {
                assert_eq!(
                    headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok()),
                    Some("Bearer remote-key")
                );
                axum::Json(json!({
                    "object": "list",
                    "data": [{"id": "remote-gpt", "object": "model"}]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;

        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = default_config();
        config.settings.lan_remote_host = "127.0.0.1".into();
        config.settings.lan_remote_port = port;
        config.settings.lan_remote_api_key = "remote-key".into();
        store.replace_config(config).await.unwrap();
        let config = store.config().await;
        let state = ServerState {
            store,
            client: Client::new(),
        };

        let response = proxy_lan_models(&state, &config.settings).await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["data"][0]["id"], "gpt-5.5");
        assert_eq!(value["data"][0]["display_name"], "remote-gpt");
        server.abort();
    }

    #[tokio::test]
    async fn lan_response_proxy_restores_real_remote_model() {
        let app = axum::Router::new()
            .route(
                "/v1/models",
                axum::routing::get(|| async {
                    axum::Json(json!({
                        "object": "list",
                        "data": [{"id": "remote-gpt", "object": "model"}]
                    }))
                }),
            )
            .route(
                "/v1/responses",
                axum::routing::post(|headers: HeaderMap, body: Bytes| async move {
                    assert_eq!(
                        headers
                            .get(header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok()),
                        Some("Bearer remote-key")
                    );
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(value["model"], "remote-gpt");
                    assert!(value.to_string().contains("input_image"));
                    axum::Json(json!({"id": "resp_lan", "status": "completed"}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;

        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = default_config();
        config.settings.codex_injection_mode = CodexInjectionMode::LanShare;
        config.settings.lan_remote_host = "127.0.0.1".into();
        config.settings.lan_remote_port = port;
        config.settings.lan_remote_api_key = "remote-key".into();
        store.replace_config(config).await.unwrap();
        let state = ServerState {
            store,
            client: Client::new(),
        };
        let body = json!({
            "model": "gpt-5.5",
            "input": [{"type": "input_image", "image_url": "data:image/png;base64,AA=="}],
            "stream": false
        });
        let body_bytes = Bytes::from(serde_json::to_vec(&body).unwrap());

        let (response, matched, _, _) =
            route_response(state, HeaderMap::new(), body, body_bytes, "lan-test".into())
                .await
                .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(matched.route_reason, "lan_share");
        assert_eq!(matched.model.id, "remote-gpt");
        assert_eq!(matched.requested_model, "gpt-5.5");
        assert_eq!(value["id"], "resp_lan");
        server.abort();
    }

    #[tokio::test]
    async fn anthropic_retries_with_context_bridge_when_upstream_context_is_full() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let message_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let beta_headers = Arc::new(Mutex::new(Vec::<String>::new()));
        let app = axum::Router::new()
            .route(
                "/v1/messages/count_tokens",
                axum::routing::post(
                    |headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
                        let beta = headers
                            .get("anthropic-beta")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default();
                        assert_eq!(beta, CLAUDE_DESKTOP_COUNT_TOKENS_BETA);
                        assert_eq!(
                            headers
                                .get(header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok()),
                            Some("Bearer sk-test")
                        );
                        assert!(headers.get("x-api-key").is_none());
                        assert!(headers.get("x-stainless-timeout").is_none());
                        assert!(body.get("context_management").is_none());
                        assert!(body.get("system").is_none());
                        assert_eq!(body["tools"].as_array().unwrap().len(), 0);
                        assert_eq!(body["thinking"]["type"], "enabled");
                        assert_eq!(body["thinking"]["budget_tokens"], 1024);
                        axum::Json(json!({ "input_tokens": 50_000 })).into_response()
                    },
                ),
            )
            .route(
                "/v1/messages",
                axum::routing::post({
                    let attempts = attempts.clone();
                    let message_bodies = message_bodies.clone();
                    let beta_headers = beta_headers.clone();
                    move |headers: HeaderMap, axum::Json(body): axum::Json<Value>| {
                        let attempts = attempts.clone();
                        let message_bodies = message_bodies.clone();
                        let beta_headers = beta_headers.clone();
                        async move {
                            let beta = headers
                                .get("anthropic-beta")
                                .and_then(|value| value.to_str().ok())
                                .unwrap_or_default()
                                .to_string();
                            assert_eq!(
                                headers
                                    .get(header::AUTHORIZATION)
                                    .and_then(|value| value.to_str().ok()),
                                Some("Bearer sk-test")
                            );
                            assert!(headers.get("x-api-key").is_none());
                            assert_eq!(
                                headers
                                    .get("x-stainless-timeout")
                                    .and_then(|value| value.to_str().ok()),
                                Some("900")
                            );
                            beta_headers.lock().unwrap().push(beta);
                            message_bodies.lock().unwrap().push(body.clone());
                            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                            if attempt == 0 {
                                assert!(body.get("context_management").is_none());
                                assert!(!body
                                    .to_string()
                                    .contains("Neko Route archived an older tool result"));
                                return (
                                    StatusCode::BAD_REQUEST,
                                    axum::Json(json!({
                                        "error": {
                                            "message": "Context window is full. Reduce conversation history."
                                        }
                                    })),
                                )
                                    .into_response();
                            }
                            assert!(body.get("context_management").is_none());
                            assert!(body
                                .to_string()
                                .contains("Neko Route archived an older tool result"));
                            axum::Json(json!({
                                "id": "msg_test",
                                "type": "message",
                                "role": "assistant",
                                "content": [{ "type": "text", "text": "OK" }],
                                "usage": { "input_tokens": 12, "output_tokens": 1 }
                            }))
                            .into_response()
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;

        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = default_config();
        config.providers.push(Provider {
            id: "anthropic-relay".into(),
            name: "Anthropic Relay".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::AnthropicMessages,
            base_url: format!("http://127.0.0.1:{port}/v1"),
            key_ref: Some("provider:anthropic-relay".into()),
            http_proxy: Default::default(),
        });
        config.models.push(ModelEntry {
            id: "claude-relay-opus".into(),
            display_name: "Claude Relay Opus".into(),
            description: "Relay".into(),
            context_window: 200_000,
            enabled: true,
            provider_id: "anthropic-relay".into(),
            upstream_model: Some("claude-opus-4-8".into()),
            timeout_ms: 0,
            retry_count: 0,
            reasoning_enabled: true,
            default_reasoning_level: "max".into(),
            supported_reasoning_levels: vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "max".into(),
            ],
            codex_alias: None,
        });
        store.replace_config(config).await.unwrap();
        store
            .key_store()
            .set_secret("provider:anthropic-relay", "sk-test")
            .unwrap();
        let state = ServerState {
            store,
            client: Client::new(),
        };
        let store_for_assert = state.store.clone();
        let request = json!({
            "model": "claude-relay-opus",
            "input": (0..10).flat_map(|index| {
                vec![
                    json!({
                        "type": "function_call",
                        "call_id": format!("toolu_{index}"),
                        "name": "exec_command",
                        "arguments": "{\"cmd\":\"large\"}"
                    }),
                    json!({
                        "type": "function_call_output",
                        "call_id": format!("toolu_{index}"),
                        "output": format!("tool output {index}\n{}", "x".repeat(12_000))
                    })
                ]
            }).chain(std::iter::once(json!({
                "role": "user",
                "content": "hello"
            }))).collect::<Vec<_>>(),
            "stream": false
        });
        let body_bytes = Bytes::from(serde_json::to_vec(&request).unwrap());

        let (response, matched, usage, context_bridge) = route_response(
            state,
            HeaderMap::new(),
            request,
            body_bytes,
            "ctx-retry".into(),
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(matched.route_reason, "direct");
        assert_eq!(value["output_text"], "OK");
        assert_eq!(usage.unwrap().input_tokens, 12);
        let context_bridge = context_bridge.unwrap();
        assert!(!context_bridge.context_management);
        assert!(context_bridge.protection_triggered);
        assert_eq!(
            context_bridge.compression_stage.as_deref(),
            Some("context_full_retry_preserve_6")
        );
        assert!(context_bridge.archived_tool_results > 0);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let bodies = message_bodies.lock().unwrap();
        assert!(bodies[0].get("context_management").is_none());
        assert!(bodies[1].get("context_management").is_none());
        for beta in beta_headers.lock().unwrap().iter() {
            assert_eq!(beta, CLAUDE_DESKTOP_MESSAGES_BETA);
        }
        let pressure = store_for_assert
            .claude_context_pressure(
                "anthropic-relay".into(),
                "claude-relay-opus".into(),
                "provider_model".into(),
            )
            .await
            .unwrap();
        assert!(pressure.requires_precompression);
        assert!(pressure.context_full_body_bytes > 0);
        assert_eq!(
            pressure.compression_stage.as_deref(),
            Some("context_full_retry_preserve_6")
        );
        server.abort();
    }

    #[tokio::test]
    async fn anthropic_precompresses_before_messages_when_pressure_requires_it() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let message_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = axum::Router::new()
            .route(
                "/v1/messages/count_tokens",
                axum::routing::post(|| async { axum::Json(json!({ "input_tokens": 50_000 })) }),
            )
            .route(
                "/v1/messages",
                axum::routing::post({
                    let attempts = attempts.clone();
                    let message_bodies = message_bodies.clone();
                    move |axum::Json(body): axum::Json<Value>| {
                        let attempts = attempts.clone();
                        let message_bodies = message_bodies.clone();
                        async move {
                            attempts.fetch_add(1, Ordering::SeqCst);
                            message_bodies.lock().unwrap().push(body.clone());
                            assert!(body
                                .to_string()
                                .contains("Neko Route archived an older tool result"));
                            axum::Json(json!({
                                "id": "msg_test",
                                "type": "message",
                                "role": "assistant",
                                "content": [{ "type": "text", "text": "OK" }],
                                "usage": { "input_tokens": 12, "output_tokens": 1 }
                            }))
                            .into_response()
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;

        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = default_config();
        config.providers.push(Provider {
            id: "anthropic-relay".into(),
            name: "Anthropic Relay".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::AnthropicMessages,
            base_url: format!("http://127.0.0.1:{port}/v1"),
            key_ref: Some("provider:anthropic-relay".into()),
            http_proxy: Default::default(),
        });
        config.models.push(ModelEntry {
            id: "claude-relay-opus".into(),
            display_name: "Claude Relay Opus".into(),
            description: "Relay".into(),
            context_window: 200_000,
            enabled: true,
            provider_id: "anthropic-relay".into(),
            upstream_model: Some("claude-opus-4-8".into()),
            timeout_ms: 0,
            retry_count: 0,
            reasoning_enabled: true,
            default_reasoning_level: "max".into(),
            supported_reasoning_levels: vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "max".into(),
            ],
            codex_alias: None,
        });
        store.replace_config(config).await.unwrap();
        store
            .key_store()
            .set_secret("provider:anthropic-relay", "sk-test")
            .unwrap();
        store
            .mark_claude_context_precompression(
                "anthropic-relay".into(),
                "claude-relay-opus".into(),
                "provider_model".into(),
                64_000,
                Some("context_full_retry_preserve_6".into()),
            )
            .await;
        let state = ServerState {
            store,
            client: Client::new(),
        };
        let request = json!({
            "model": "claude-relay-opus",
            "input": (0..10).flat_map(|index| {
                vec![
                    json!({
                        "type": "function_call",
                        "call_id": format!("toolu_{index}"),
                        "name": "exec_command",
                        "arguments": "{\"cmd\":\"large\"}"
                    }),
                    json!({
                        "type": "function_call_output",
                        "call_id": format!("toolu_{index}"),
                        "output": format!("tool output {index}\n{}", "x".repeat(12_000))
                    })
                ]
            }).chain(std::iter::once(json!({
                "role": "user",
                "content": "hello"
            }))).collect::<Vec<_>>(),
            "stream": false
        });
        let body_bytes = Bytes::from(serde_json::to_vec(&request).unwrap());

        let (_response, _matched, usage, context_bridge) = route_response(
            state,
            HeaderMap::new(),
            request,
            body_bytes,
            "ctx-precompress".into(),
        )
        .await
        .unwrap();
        let context_bridge = context_bridge.unwrap();

        assert_eq!(usage.unwrap().input_tokens, 12);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(message_bodies.lock().unwrap().len(), 1);
        assert!(context_bridge.protection_triggered);
        assert_eq!(
            context_bridge.compression_stage.as_deref(),
            Some("precompress_preserve_6")
        );
        assert!(context_bridge.archived_tool_results > 0);
        server.abort();
    }

    #[tokio::test]
    async fn anthropic_count_tokens_failure_uses_current_body_estimate_not_previous_ratio() {
        let message_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = axum::Router::new()
            .route(
                "/v1/messages/count_tokens",
                axum::routing::post(|| async {
                    (
                        StatusCode::NOT_FOUND,
                        axum::Json(json!({"error": {"message": "count_tokens not found"}})),
                    )
                        .into_response()
                }),
            )
            .route(
                "/v1/messages",
                axum::routing::post({
                    let message_bodies = message_bodies.clone();
                    move |axum::Json(body): axum::Json<Value>| {
                        let message_bodies = message_bodies.clone();
                        async move {
                            message_bodies.lock().unwrap().push(body.clone());
                            assert!(body.get("context_management").is_none());
                            assert!(!body
                                .to_string()
                                .contains("Neko Route archived an older tool result"));
                            axum::Json(json!({
                                "id": "msg_pressure",
                                "type": "message",
                                "role": "assistant",
                                "content": [{ "type": "text", "text": "OK" }],
                                "usage": { "input_tokens": 700000, "output_tokens": 1 }
                            }))
                            .into_response()
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;

        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = default_config();
        config.providers.push(Provider {
            id: "anthropic-relay".into(),
            name: "Anthropic Relay".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::AnthropicMessages,
            base_url: format!("http://127.0.0.1:{port}/v1"),
            key_ref: Some("provider:anthropic-relay".into()),
            http_proxy: Default::default(),
        });
        config.models.push(ModelEntry {
            id: "claude-relay-opus".into(),
            display_name: "Claude Relay Opus".into(),
            description: "Relay".into(),
            context_window: 1_000_000,
            enabled: true,
            provider_id: "anthropic-relay".into(),
            upstream_model: Some("claude-opus-4-8".into()),
            timeout_ms: 0,
            retry_count: 0,
            reasoning_enabled: true,
            default_reasoning_level: "max".into(),
            supported_reasoning_levels: vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "max".into(),
            ],
            codex_alias: None,
        });
        store.replace_config(config).await.unwrap();
        store
            .key_store()
            .set_secret("provider:anthropic-relay", "sk-test")
            .unwrap();
        store
            .upsert_claude_context_pressure(
                "anthropic-relay".into(),
                "claude-relay-opus".into(),
                "provider_model".into(),
                969_000,
                1_000_000,
            )
            .await;
        let state = ServerState {
            store,
            client: Client::new(),
        };
        let mut input = Vec::new();
        for index in 0..24 {
            input.push(json!({
                "type": "function_call",
                "call_id": format!("toolu_{index}"),
                "name": "exec_command",
                "arguments": "{}"
            }));
            input.push(json!({
                "type": "function_call_output",
                "call_id": format!("toolu_{index}"),
                "output": format!("tool output {index}\n{}", "x".repeat(12_000))
            }));
        }
        input.push(json!({"role": "user", "content": "continue"}));
        let request = json!({
            "model": "claude-relay-opus",
            "input": input,
            "stream": false
        });
        let body_bytes = Bytes::from(serde_json::to_vec(&request).unwrap());

        let (_response, _matched, usage, context_bridge) = route_response(
            state,
            HeaderMap::new(),
            request,
            body_bytes,
            "pressure-sample".into(),
        )
        .await
        .unwrap();
        let context_bridge = context_bridge.unwrap();

        assert_eq!(usage.unwrap().input_tokens, 700_000);
        assert!(!context_bridge.protection_triggered);
        assert_eq!(
            context_bridge.estimate_source.as_deref(),
            Some("body_size_chars")
        );
        assert_eq!(context_bridge.previous_success_input_tokens, Some(969_000));
        assert_eq!(context_bridge.archived_tool_results, 0);
        assert_eq!(message_bodies.lock().unwrap().len(), 1);
        server.abort();
    }

    #[tokio::test]
    async fn anthropic_context_window_full_returns_clean_error_with_diagnostics() {
        let app = axum::Router::new()
            .route(
                "/v1/messages/count_tokens",
                axum::routing::post(|| async { axum::Json(json!({ "input_tokens": 12000 })) }),
            )
            .route(
                "/v1/messages",
                axum::routing::post(|| async {
                    (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({
                            "error": {
                                "message": "Context window is full. Reduce conversation history, system prompt, or tools."
                            }
                        })),
                    )
                        .into_response()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;

        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let mut config = default_config();
        config.providers.push(Provider {
            id: "anthropic-relay".into(),
            name: "Anthropic Relay".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::AnthropicMessages,
            base_url: format!("http://127.0.0.1:{port}/v1"),
            key_ref: Some("provider:anthropic-relay".into()),
            http_proxy: Default::default(),
        });
        let mut model = config.models[1].clone();
        model.id = "claude-relay-opus".into();
        model.provider_id = "anthropic-relay".into();
        model.upstream_model = Some("claude-opus-4-8".into());
        config.models.push(model);
        store.replace_config(config).await.unwrap();
        store
            .key_store()
            .set_secret("provider:anthropic-relay", "sk-test")
            .unwrap();
        let state = ServerState {
            store,
            client: Client::new(),
        };
        let request = json!({
            "model": "claude-relay-opus",
            "input": "hello",
            "stream": false
        });
        let body_bytes = Bytes::from(serde_json::to_vec(&request).unwrap());

        let error = route_response(
            state,
            HeaderMap::new(),
            request,
            body_bytes,
            "ctx-full".into(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, "context_length_exceeded");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.message, context_full_error_message());
        assert!(error.context_bridge.is_some());
        assert!(!error.message.contains("\\\"error\\\""));
        server.abort();
    }

    #[test]
    fn anthropic_body_uses_desktop_system_blocks_and_mid_conversation_system_messages() {
        let request = json!({
            "model": "claude-3-5-haiku",
            "instructions": "main instructions",
            "input": [
                {"role": "developer", "content": "follow policy"},
                {"role": "user", "content": "Reply OK"},
                {"role": "latest_reminder", "content": "mid reminder"},
                {"role": "assistant", "content": "OK"}
            ],
            "stream": false
        });
        let body = build_anthropic_body(&request, "claude-3-5-haiku", false);

        assert!(body["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("x-anthropic-billing-header"));
        assert!(body["system"][0].get("cache_control").is_none());
        assert!(body["system"][1]["text"]
            .as_str()
            .unwrap()
            .contains("official CLI for Claude"));
        assert_eq!(body["system"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(
            body["system"][2]["text"],
            "main instructions\n\nfollow policy"
        );
        assert_eq!(body["system"][2]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "Reply OK");
        assert_eq!(body["messages"][1]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "mid reminder");
        assert_eq!(body["messages"][2]["role"], "assistant");
    }

    #[test]
    fn anthropic_mid_conversation_system_waits_for_tool_result_user_turn() {
        let request = json!({
            "model": "claude-3-5-haiku",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "toolu_1",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"pwd\"}"
                },
                {"role": "developer", "content": "Approved command prefix saved"},
                {
                    "type": "function_call_output",
                    "call_id": "toolu_1",
                    "output": "/tmp/project"
                }
            ],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-3-5-haiku", false);

        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
        assert_eq!(body["messages"][2]["role"], "system");
        assert_eq!(
            body["messages"][2]["content"],
            "Approved command prefix saved"
        );
    }

    #[test]
    fn anthropic_body_never_sends_initial_system_as_messages_zero() {
        let request = json!({
            "model": "claude-3-5-haiku",
            "input": [
                {"role": "developer", "content": "follow policy"},
                {"role": "user", "content": "你好"}
            ],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-3-5-haiku", false);

        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "你好");
        assert!(body["system"][2]["text"]
            .as_str()
            .unwrap()
            .contains("follow policy"));
    }

    #[test]
    fn anthropic_body_caches_latest_user_message() {
        let request = json!({
            "model": "claude-opus-4-8",
            "input": [
                {"role": "user", "content": "first question"},
                {"role": "assistant", "content": "old answer"},
                {"role": "user", "content": "current question"}
            ],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-opus-4-8", false);

        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][1]["content"], "old answer");
        assert_eq!(
            body["messages"][2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            body["messages"][2]["content"][0]["text"],
            "current question"
        );
    }

    #[test]
    fn anthropic_count_tokens_body_removes_final_request_only_fields_and_cache() {
        let request = json!({
            "model": "claude-opus-4-8",
            "instructions": "main instructions",
            "tools": [{
                "type": "function",
                "name": "exec_command",
                "description": "Run command",
                "parameters": { "type": "object", "properties": {} }
            }],
            "input": [
                {"role": "user", "content": "current question"}
            ],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-opus-4-8", false);
        let count_body = build_anthropic_count_tokens_body(&body);

        assert!(count_body.get("system").is_none());
        assert!(count_body.get("metadata").is_none());
        assert!(count_body.get("output_config").is_none());
        assert!(count_body.get("max_tokens").is_none());
        assert!(count_body.get("stream").is_none());
        assert_eq!(count_body["tools"].as_array().unwrap().len(), 0);
        assert_eq!(count_body["thinking"]["type"], "enabled");
        assert_eq!(count_body["thinking"]["budget_tokens"], 1024);
        assert!(!count_body.to_string().contains("cache_control"));
    }

    #[test]
    fn anthropic_body_can_cache_latest_tool_result() {
        let request = json!({
            "model": "claude-opus-4-8",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"pwd\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "/tmp/project"
                }
            ],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-opus-4-8", false);

        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
        assert_eq!(
            body["messages"][1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn maps_codex_reasoning_effort_to_claude_tiers() {
        assert_eq!(
            map_reasoning_effort(&json!({"reasoning":{"effort":"low"}})),
            Some("low")
        );
        assert_eq!(
            map_reasoning_effort(&json!({"reasoning":{"effort":"medium"}})),
            Some("medium")
        );
        assert_eq!(
            map_reasoning_effort(&json!({"reasoning":{"effort":"high"}})),
            Some("high")
        );
        assert_eq!(
            map_reasoning_effort(&json!({"reasoning":{"effort":"xhigh"}})),
            Some("xhigh")
        );
        // flat alias, odd casing, top-level effort, and output_config.effort all read
        assert_eq!(
            map_reasoning_effort(&json!({"reasoning_effort":"HIGH"})),
            Some("high")
        );
        assert_eq!(map_reasoning_effort(&json!({"effort":"low"})), Some("low"));
        assert_eq!(
            map_reasoning_effort(&json!({"output_config":{"effort":"medium"}})),
            Some("medium")
        );
        // already-max passes through; absent -> none
        assert_eq!(
            map_reasoning_effort(&json!({"reasoning":{"effort":"max"}})),
            Some("max")
        );
        assert_eq!(map_reasoning_effort(&json!({})), None);
    }

    #[test]
    fn claude_precompression_hint_uses_eighty_percent_body_threshold() {
        let sample = ClaudeContextPressureSample {
            input_tokens: 900_000,
            body_bytes: 1_000_000,
            requires_precompression: true,
            context_full_body_bytes: 1_000,
            compression_stage: Some("context_full_retry_preserve_6".into()),
        };

        assert!(!should_precompress_from_pressure_sample(Some(&sample), 799));
        assert!(should_precompress_from_pressure_sample(Some(&sample), 800));

        let disabled = ClaudeContextPressureSample {
            requires_precompression: false,
            ..sample
        };
        assert!(!should_precompress_from_pressure_sample(
            Some(&disabled),
            1_000
        ));
        assert!(!should_precompress_from_pressure_sample(None, 1_000));
    }

    #[test]
    fn chat_completions_uses_openai_reasoning_effort_only() {
        let request = json!({
            "model": "deepseek-v4-pro",
            "input": "hi",
            "reasoning": { "effort": "xhigh" },
            "stream": false
        });
        let body = build_chat_completions_body(&request, "deepseek-upstream", false);

        assert_eq!(map_openai_chat_reasoning_effort(&request), Some("xhigh"));
        assert_eq!(body["reasoning_effort"], "xhigh");

        let request = json!({
            "model": "deepseek-v4-pro",
            "input": "hi",
            "reasoning": { "effort": "max" },
            "stream": false
        });
        let body = build_chat_completions_body(&request, "deepseek-upstream", false);

        assert_eq!(map_openai_chat_reasoning_effort(&request), None);
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn anthropic_body_uses_claude_code_adaptive_max_profile() {
        let request = json!({
            "model": "claude-opus-4-8",
            "input": "hi",
            "reasoning": { "effort": "high" },
            "max_output_tokens": 1024,
            "stream": false
        });
        let body = build_anthropic_body(&request, "claude-opus-4-8", false);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(body["thinking"].get("budget_tokens").is_none());
        assert_eq!(body["output_config"]["effort"], "max");
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["max_tokens"], 1024);
        assert!(body.get("context_management").is_none());
    }

    #[test]
    fn anthropic_one_million_context_strips_model_suffix() {
        let (model, one_million) = anthropic_model_for_request("claude-opus-4-8[1m]", 258_000);

        assert_eq!(model, "claude-opus-4-8");
        assert!(one_million);
    }

    #[test]
    fn anthropic_one_million_context_uses_context_window_marker() {
        let (model, one_million) = anthropic_model_for_request("claude-opus-4-8", 1_000_000);

        assert_eq!(model, "claude-opus-4-8");
        assert!(one_million);
    }

    #[test]
    fn anthropic_mirror_headers_use_claude_desktop_messages_shape() {
        let request = json!({
            "metadata": {
                "user_id": "{\"session_id\":\"session-from-metadata\"}"
            }
        });
        let headers = claude_code_mirror_headers(
            vec![
                ("content-type".into(), "application/json".into()),
                ("x-api-key".into(), "sk-test".into()),
                ("anthropic-beta".into(), "oauth-2025-04-20".into()),
            ],
            &request,
        );
        let get = |name: &str| {
            headers
                .iter()
                .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
                .unwrap()
        };

        assert_eq!(get("accept"), "application/json");
        assert_eq!(get("authorization"), "Bearer sk-test");
        assert_eq!(get("content-type"), "application/json");
        assert_eq!(
            get("user-agent"),
            "claude-cli/2.1.170 (external, claude-desktop-3p, agent-sdk/0.3.170)"
        );
        assert_eq!(get("x-claude-code-session-id"), "session-from-metadata");
        assert_eq!(get("x-stainless-lang"), "js");
        assert_eq!(get("x-stainless-package-version"), "0.94.0");
        assert_eq!(get("x-stainless-retry-count"), "0");
        assert_eq!(get("x-stainless-runtime"), "node");
        assert_eq!(get("x-stainless-runtime-version"), "v24.3.0");
        assert_eq!(get("x-stainless-timeout"), "900");
        assert_eq!(get("anthropic-beta"), CLAUDE_DESKTOP_MESSAGES_BETA);
        assert_eq!(get("anthropic-dangerous-direct-browser-access"), "true");
        assert_eq!(get("anthropic-version"), "2023-06-01");
        assert_eq!(get("x-app"), "cli");
        assert!(!headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("x-api-key")));
    }

    #[test]
    fn anthropic_count_tokens_headers_use_desktop_count_profile() {
        let request = json!({
            "metadata": {
                "user_id": "{\"session_id\":\"session-from-metadata\"}"
            }
        });
        let headers = claude_code_count_tokens_headers(
            vec![("x-api-key".into(), "sk-test".into())],
            &request,
        );
        let get = |name: &str| {
            headers
                .iter()
                .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
                .unwrap()
        };

        assert_eq!(get("authorization"), "Bearer sk-test");
        assert_eq!(get("anthropic-beta"), CLAUDE_DESKTOP_COUNT_TOKENS_BETA);
        assert!(headers
            .iter()
            .all(|(name, _)| !name.eq_ignore_ascii_case("x-stainless-timeout")));
    }

    #[test]
    fn anthropic_messages_url_always_adds_beta_query() {
        assert_eq!(
            anthropic_messages_url("https://relay.example/v1", true),
            "https://relay.example/v1/messages?beta=true"
        );
        assert_eq!(
            anthropic_messages_url("https://relay.example/v1", false),
            "https://relay.example/v1/messages?beta=true"
        );
    }

    #[test]
    fn context_bridge_passes_through_when_no_budget_is_requested() {
        let mut messages = Vec::new();
        for index in 0..42 {
            messages.push(json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": format!("toolu_{index}"),
                    "name": "exec_command",
                    "input": { "cmd": format!("run-{index}") }
                }]
            }));
            messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": format!("toolu_{index}"),
                    "content": format!("tool output {index}\npath: /tmp/file-{index}.txt\n{}", "x".repeat(12_000))
                }]
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": "continue from the last test result"
        }));
        let mut body = json!({
            "model": "claude-opus-4-8",
            "messages": messages,
            "stream": false
        });
        let original_request = json!({});

        let (diagnostics, artifacts) = bridge_anthropic_context_body(
            &mut body,
            &original_request,
            "req_bridge",
            "claude-opus-4-8",
            8,
            Some(u64::MAX),
            &[],
            None,
        );
        let positions = collect_anthropic_tool_result_positions(&body);

        assert_eq!(diagnostics.tool_result_count, 42);
        assert_eq!(diagnostics.archived_tool_results, 0);
        assert_eq!(diagnostics.kept_tool_results, 42);
        assert!(artifacts.is_empty());
        assert_eq!(diagnostics.strategy, "pass_through");
        assert!(positions[0].content.contains("tool output 0"));
        assert!(positions[41].content.contains("tool output 41"));
    }

    #[test]
    fn context_bridge_archives_old_tool_results_only_when_over_budget() {
        let mut messages = Vec::new();
        for index in 0..42 {
            messages.push(json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": format!("toolu_{index}"),
                    "name": "exec_command",
                    "input": { "cmd": format!("run-{index}") }
                }]
            }));
            messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": format!("toolu_{index}"),
                    "content": format!("tool output {index}\npath: /tmp/file-{index}.txt\n{}", "x".repeat(12_000))
                }]
            }));
        }
        messages.push(json!({
            "role": "user",
            "content": "continue from the last test result"
        }));
        let mut body = json!({
            "model": "claude-opus-4-8",
            "messages": messages,
            "stream": false
        });
        let target = (json_size(&body) as u64).saturating_sub(180_000);
        let original_request = json!({});

        let (diagnostics, artifacts) = bridge_anthropic_context_body(
            &mut body,
            &original_request,
            "req_bridge",
            "claude-opus-4-8",
            8,
            Some(target),
            &[],
            Some("test_over_budget".into()),
        );
        let positions = collect_anthropic_tool_result_positions(&body);

        assert_eq!(diagnostics.tool_result_count, 42);
        assert!(diagnostics.archived_tool_results > 0);
        assert!(diagnostics.archived_tool_results < 34);
        assert_eq!(
            diagnostics.kept_tool_results,
            42 - diagnostics.archived_tool_results
        );
        assert_eq!(artifacts.len() as u64, diagnostics.archived_tool_results);
        assert!(diagnostics.final_body_bytes <= target);
        assert!(artifacts[0].content_text.contains("tool output 0"));
        assert!(artifacts[0].summary.contains("/tmp/file-0.txt"));
        assert!(positions[0]
            .content
            .contains("Neko Route archived an older tool result"));
        assert!(positions[34].content.contains("tool output 34"));
        assert!(positions[41].content.contains("tool output 41"));
    }

    #[test]
    fn context_bridge_recalls_relevant_archived_artifacts() {
        let artifact = ContextArtifact {
            hash: "abc123".into(),
            created_at: Utc::now(),
            request_id: "old_req".into(),
            model: "gpt-5.4".into(),
            tool_name: Some("exec_command".into()),
            tool_args: Some("{\"cmd\":\"pressure test\"}".into()),
            content_bytes: 128,
            content_text: "needle-token pressure test completed at 512K".into(),
            summary: "pressure test summary needle-token".into(),
        };
        let mut body = json!({
            "model": "claude-opus-4-8",
            "messages": [{
                "role": "user",
                "content": "what happened with needle-token?"
            }],
            "stream": false
        });

        let (diagnostics, artifacts) = bridge_anthropic_context_body(
            &mut body,
            &json!({}),
            "req_recall",
            "claude-opus-4-8",
            8,
            Some(0),
            &[artifact],
            Some("test_recall".into()),
        );

        assert!(artifacts.is_empty());
        assert_eq!(diagnostics.recalled_artifacts, 1);
        assert_eq!(diagnostics.archived_tool_results, 0);
        assert!(body["system"].to_string().contains("needle-token"));
        assert!(body["system"].to_string().contains("sha256:abc123"));
    }

    #[test]
    fn context_bridge_diagnostics_marks_function_call_output_single_dot() {
        let request = json!({
            "model": "claude-opus-4-8",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "toolu_dot",
                    "name": "exec_command",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "toolu_dot",
                    "output": "."
                }
            ],
            "stream": false
        });
        let body = build_anthropic_body(&request, "claude-opus-4-8", false);
        let diagnostics = context_bridge_diagnostics(&body, &request, json_size(&body) as u64, 1);

        assert_eq!(diagnostics.last_message_role.as_deref(), Some("user"));
        assert_eq!(
            diagnostics.last_message_content_type.as_deref(),
            Some("tool_result")
        );
        assert!(diagnostics.last_message_from_function_call_output);
        assert!(!diagnostics.single_dot_user_message);
        assert_eq!(diagnostics.latest_tool_result_count, 1);
        assert_eq!(diagnostics.latest_tool_result_text_length, 1);
        assert!(diagnostics.latest_tool_result_single_dot);
    }

    #[test]
    fn context_bridge_diagnostics_marks_plain_user_single_dot() {
        let request = json!({
            "model": "claude-opus-4-8",
            "input": ".",
            "stream": false
        });
        let body = build_anthropic_body(&request, "claude-opus-4-8", false);
        let diagnostics = context_bridge_diagnostics(&body, &request, json_size(&body) as u64, 0);

        assert_eq!(diagnostics.last_message_role.as_deref(), Some("user"));
        assert_eq!(
            diagnostics.last_message_content_type.as_deref(),
            Some("text")
        );
        assert!(!diagnostics.last_message_from_function_call_output);
        assert!(diagnostics.single_dot_user_message);
        assert_eq!(diagnostics.latest_tool_result_count, 0);
    }

    #[test]
    fn context_bridge_summary_is_never_only_single_dot() {
        let summary = summarize_tool_result(".", Some("exec_command"), Some("{}"), false);
        let card = archived_tool_result_card("abc", &summary, 1, Some("exec_command"), Some("{}"));

        assert_ne!(summary.trim(), ".");
        assert_ne!(card.trim(), ".");
        assert!(card.contains("Neko Route archived an older tool result"));
    }

    #[test]
    fn anthropic_messages_no_longer_use_classic_relay_thinking() {
        let request = json!({
            "model": "claude-3-5-sonnet",
            "input": "hi",
            "reasoning": { "effort": "high" },
            "max_output_tokens": 1024,
            "stream": false
        });
        let body = build_anthropic_body(&request, "claude-3-5-sonnet", false);

        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(body["thinking"].get("budget_tokens").is_none());
        assert_eq!(body["output_config"]["effort"], "max");
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["max_tokens"], 1024);
    }

    #[test]
    fn anthropic_messages_default_to_claude_code_max_effort() {
        let request = json!({ "model": "claude-3-5-sonnet", "input": "hi", "stream": false });
        let body = build_anthropic_body(&request, "claude-3-5-sonnet", false);
        assert_eq!(body["output_config"]["effort"], "max");
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["thinking"]["type"], "adaptive");
    }

    #[test]
    fn anthropic_messages_keep_requested_max_tokens() {
        let request = json!({
            "model": "claude-opus-4-8",
            "input": "hi",
            "reasoning": { "effort": "high" },
            "max_output_tokens": 1024,
            "stream": false
        });
        let body = build_anthropic_body(&request, "claude-opus-4-8", false);

        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(body["thinking"].get("budget_tokens").is_none());
        assert_eq!(body["output_config"]["effort"], "max");
        assert!(body.get("reasoning_effort").is_none());
        assert_eq!(body["max_tokens"], 1024);
    }

    #[test]
    fn anthropic_messages_include_adaptive_thinking_when_codex_omits_reasoning() {
        let request = json!({ "model": "claude-opus-4-8", "input": "hi", "stream": false });
        let body = build_anthropic_body(&request, "claude-opus-4-8", false);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "max");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn extracts_chat_stream_delta() {
        let data = r#"{"choices":[{"delta":{"content":"OK"}}]}"#;
        assert_eq!(
            extract_stream_delta(&ProviderProtocol::OpenAiChatCompletions, data).unwrap(),
            "OK"
        );
    }

    #[test]
    fn extracts_anthropic_stream_delta() {
        let data = r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"OK"}}"#;
        assert_eq!(
            extract_stream_delta(&ProviderProtocol::AnthropicMessages, data).unwrap(),
            "OK"
        );
    }

    #[test]
    fn builds_chat_completions_body_from_responses_input() {
        let request = json!({
            "model": "gpt-test",
            "instructions": "follow policy",
            "input": [
                {"role": "developer", "content": "internal rule"},
                {"role": "user", "content": [{"type": "input_text", "text": "Reply OK"}]}
            ],
            "max_output_tokens": 77,
            "temperature": 0.2,
            "stream": false
        });

        let body = build_chat_completions_body(&request, "upstream-chat", false);

        assert_eq!(body["model"], "upstream-chat");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "system");
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(body["messages"][2]["content"], "Reply OK");
        assert_eq!(body["max_completion_tokens"], 77);
        assert_eq!(body["temperature"], 0.2);
    }

    #[test]
    fn chat_completions_converts_responses_images_to_image_url_parts() {
        let request = json!({
            "model": "gpt-test",
            "input": [{
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "Describe this" },
                    {
                        "type": "input_image",
                        "image_url": "data:image/png;base64,AAAA"
                    }
                ]
            }],
            "stream": false
        });

        let body = build_chat_completions_body(&request, "upstream-chat", false);
        let content = body["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Describe this");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn wraps_chat_completion_text_as_responses_json() {
        let upstream = json!({
            "choices": [
                { "message": { "content": "OK" } }
            ],
            "usage": { "prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3 }
        });
        let response = chat_completion_response_json(
            "abc",
            "chat-model",
            &upstream,
            upstream.get("usage").cloned(),
        );

        assert_eq!(response["output_text"], "OK");
        assert_eq!(response["output"][0]["content"][0]["text"], "OK");
        assert_eq!(response["usage"]["total_tokens"], 3);
        assert_eq!(response["usage"]["input_tokens"], 2);
        assert_eq!(response["usage"]["output_tokens"], 1);
    }

    #[test]
    fn chat_completion_reasoning_only_falls_back_to_visible_message() {
        let upstream = json!({
            "choices": [{
                "message": {
                    "content": "",
                    "reasoning_content": "reasoning-only answer"
                },
                "finish_reason": "stop"
            }]
        });

        let response = chat_completion_response_json("abc", "deepseek-reasoner", &upstream, None);

        assert_eq!(response["status"], "completed");
        assert_eq!(response["output"][0]["type"], "reasoning");
        assert_eq!(
            chat_reasoning_content_from_input_item(&response["output"][0]).as_deref(),
            Some("reasoning-only answer")
        );
        assert_eq!(
            response["output"][1]["content"][0]["text"],
            "reasoning-only answer"
        );
        assert_eq!(response["output_text"], "reasoning-only answer");
    }

    #[test]
    fn chat_completion_length_finish_maps_to_responses_incomplete() {
        let upstream = json!({
            "choices": [{
                "message": { "content": "partial" },
                "finish_reason": "length"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 2,
                "total_tokens": 12,
                "prompt_tokens_details": { "cached_tokens": 4 }
            }
        });

        let response = chat_completion_response_json(
            "abc",
            "chat-model",
            &upstream,
            upstream.get("usage").cloned(),
        );

        assert_eq!(response["status"], "incomplete");
        assert_eq!(
            response["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        assert_eq!(response["usage"]["input_tokens"], 10);
        assert_eq!(response["usage"]["output_tokens"], 2);
        assert_eq!(
            response["usage"]["input_tokens_details"]["cached_tokens"],
            4
        );
    }

    #[test]
    fn chat_completion_reasoning_content_is_hidden_and_restored() {
        let upstream = json!({
            "choices": [{
                "message": {
                    "content": "OK",
                    "reasoning_content": "private thinking"
                }
            }]
        });
        let response = chat_completion_response_json("abc", "deepseek-v4-pro", &upstream, None);

        assert_eq!(response["output_text"], "OK");
        assert_eq!(response["output"][0]["type"], "reasoning");
        assert_eq!(response["output"][1]["content"][0]["text"], "OK");
        assert_eq!(
            chat_reasoning_content_from_input_item(&response["output"][0]).as_deref(),
            Some("private thinking")
        );
        assert!(!response["output"].to_string().contains("private thinking"));

        let next_request = json!({
            "model": "deepseek-v4-pro",
            "input": response["output"].clone(),
            "stream": false
        });
        let body = build_chat_completions_body(&next_request, "deepseek-upstream", false);

        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"], "OK");
        assert_eq!(body["messages"][0]["reasoning_content"], "private thinking");
    }

    #[test]
    fn chat_body_includes_function_tools_and_prior_tool_turns() {
        let request = json!({
            "model": "chat-route",
            "tools": [
                {
                    "type": "function",
                    "name": "exec_command",
                    "description": "Run a shell command",
                    "parameters": {
                        "type": "object",
                        "properties": { "cmd": { "type": "string" } },
                        "required": ["cmd"]
                    },
                    "strict": false
                },
                { "type": "custom", "name": "apply_patch" }
            ],
            "tool_choice": { "type": "function", "name": "exec_command" },
            "parallel_tool_calls": true,
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"pwd\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "/tmp/project"
                }
            ],
            "stream": false
        });

        let body = build_chat_completions_body(&request, "upstream-chat", false);

        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tools"][0]["function"]["name"], "exec_command");
        assert_eq!(body["tool_choice"]["function"]["name"], "exec_command");
        assert_eq!(body["parallel_tool_calls"], true);
        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "call_1");
    }

    #[test]
    fn chat_tool_calls_return_responses_function_items() {
        let upstream = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "exec_command",
                            "arguments": "{\"cmd\":\"pwd\"}"
                        }
                    }]
                }
            }]
        });

        let output = chat_completion_output_items(&upstream);

        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["call_id"], "call_1");
        assert_eq!(output[0]["name"], "exec_command");
        assert_eq!(output[0]["arguments"], "{\"cmd\":\"pwd\"}");
    }

    #[test]
    fn chat_tool_call_reasoning_content_is_restored() {
        let upstream = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "reasoning_content": "need a tool",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "exec_command",
                            "arguments": "{\"cmd\":\"pwd\"}"
                        }
                    }]
                }
            }]
        });
        let output = chat_completion_output_items(&upstream);

        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[1]["type"], "function_call");

        let next_request = json!({
            "model": "deepseek-v4-pro",
            "input": [
                output[0].clone(),
                output[1].clone(),
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "/tmp/project"
                }
            ],
            "stream": false
        });
        let body = build_chat_completions_body(&next_request, "deepseek-upstream", false);

        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"], Value::Null);
        assert_eq!(body["messages"][0]["reasoning_content"], "need a tool");
        assert_eq!(body["messages"][0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "call_1");
    }

    #[test]
    fn direct_chat_reasoning_content_passes_through() {
        let request = json!({
            "model": "deepseek-v4-pro",
            "messages": [{
                "role": "assistant",
                "content": null,
                "reasoning_content": "already preserved"
            }],
            "stream": false
        });

        let body = build_chat_completions_body(&request, "deepseek-upstream", false);

        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"], Value::Null);
        assert_eq!(
            body["messages"][0]["reasoning_content"],
            "already preserved"
        );
    }

    fn chat_messages(body: &Value) -> &[Value] {
        body["messages"].as_array().unwrap()
    }

    fn assert_chat_tool_invariants(messages: &[Value]) {
        for (index, message) in messages.iter().enumerate() {
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for (offset, call) in calls.iter().enumerate() {
                    let reply_index = index + 1 + offset;
                    assert!(reply_index < messages.len(), "tool call has no reply");
                    assert_eq!(messages[reply_index]["role"], "tool");
                    assert_eq!(messages[reply_index]["tool_call_id"], call["id"]);
                }
            }
            if message.get("role").and_then(Value::as_str) == Some("tool") {
                let announced = messages.iter().any(|candidate| {
                    candidate
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .map(|calls| {
                            calls
                                .iter()
                                .any(|call| call.get("id") == message.get("tool_call_id"))
                        })
                        .unwrap_or(false)
                });
                assert!(announced, "orphan tool reply survived");
            }
        }
    }

    #[test]
    fn chat_body_maps_responses_fields_to_chat_completions() {
        let request = json!({
            "model": "gpt-test",
            "instructions": "system root",
            "input": [
                {"role": "developer", "content": [{"type":"input_text","text":"developer rule"}]},
                {"role": "user", "content": "hello"}
            ],
            "max_output_tokens": 256,
            "service_tier": "flex",
            "metadata": { "trace": "abc" },
            "user": "user_1",
            "store": false,
            "stream_options": { "include_usage": true },
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": { "type": "object" },
                    "strict": true
                }
            },
            "stream": true
        });

        let body = build_chat_completions_body(&request, "upstream-chat", true);

        assert_eq!(body["model"], "upstream-chat");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_completion_tokens"], 256);
        assert_eq!(body["service_tier"], "flex");
        assert_eq!(body["metadata"]["trace"], "abc");
        assert_eq!(body["user"], "user_1");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["name"], "answer");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "system");
        assert_eq!(body["messages"][2]["role"], "user");
    }

    #[test]
    fn chat_profile_allowlist_keeps_json_schema_capable_providers() {
        let provider = Provider {
            id: "openai-compatible".into(),
            name: "OpenAI".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: "https://api.openai.com/v1".into(),
            key_ref: None,
            http_proxy: Default::default(),
        };
        assert_eq!(
            chat_completions_compatibility_profile(&provider, "gpt-5.5"),
            ChatCompletionsCompatibilityProfile::JsonSchemaCapable
        );

        let provider = Provider {
            base_url: "https://openrouter.ai/api/v1".into(),
            name: "OpenRouter".into(),
            ..provider
        };
        assert_eq!(
            chat_completions_compatibility_profile(&provider, "openrouter-model"),
            ChatCompletionsCompatibilityProfile::JsonSchemaCapable
        );
    }

    #[test]
    fn chat_profile_unknown_custom_providers_default_to_json_object_only() {
        for (id, name, base_url) in [
            ("deepseek", "DeepSeek", "https://api.deepseek.com/v1"),
            ("kimi", "Kimi", "https://api.moonshot.cn/v1"),
            ("custom", "Custom Provider", "https://example.test/v1"),
        ] {
            let provider = Provider {
                id: id.into(),
                name: name.into(),
                kind: ProviderKind::Custom,
                protocol: ProviderProtocol::OpenAiChatCompletions,
                base_url: base_url.into(),
                key_ref: None,
                http_proxy: Default::default(),
            };
            assert_eq!(
                chat_completions_compatibility_profile(&provider, "chat-model"),
                ChatCompletionsCompatibilityProfile::JsonObjectOnly
            );
        }
    }

    #[test]
    fn chat_response_format_preserves_json_schema_for_capable_profile() {
        let request = json!({
            "model": "chat-route",
            "input": "Return JSON",
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": { "type": "object", "properties": { "ok": { "type": "boolean" } } },
                    "strict": true
                }
            }
        });

        let body = build_chat_completions_body_with_profile(
            &request,
            "upstream-chat",
            false,
            ChatCompletionsCompatibilityProfile::JsonSchemaCapable,
        );

        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["name"], "answer");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    }

    #[test]
    fn chat_response_format_downgrades_json_schema_to_json_object_with_hints() {
        let request = json!({
            "model": "chat-route",
            "input": "Return the answer",
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": { "type": "object", "properties": { "ok": { "type": "boolean" } } },
                    "strict": true
                }
            }
        });

        let body = build_chat_completions_body_with_profile(
            &request,
            "upstream-chat",
            false,
            ChatCompletionsCompatibilityProfile::JsonObjectOnly,
        );
        let messages = chat_messages(&body);
        let hint = messages.last().unwrap()["content"].as_str().unwrap();

        assert_eq!(body["response_format"], json!({ "type": "json_object" }));
        assert!(hint.contains("Return valid JSON only."));
        assert!(hint.contains("Return JSON matching this schema:"));
        assert!(hint.contains("\"name\":\"answer\""));
        assert!(hint.contains("\"strict\":true"));
    }

    #[test]
    fn chat_response_format_text_omits_response_format() {
        let request = json!({
            "model": "chat-route",
            "input": "hello",
            "text": {
                "format": { "type": "text" }
            }
        });

        let body = build_chat_completions_body_with_profile(
            &request,
            "upstream-chat",
            false,
            ChatCompletionsCompatibilityProfile::JsonObjectOnly,
        );

        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn chat_response_format_retry_only_handles_format_related_400() {
        let body = json!({
            "model": "deepseek-chat",
            "response_format": { "type": "json_schema" }
        });
        assert!(should_retry_chat_without_response_format(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"This response_format type is unavailable now","type":"invalid_request_error"}}"#,
            &body,
        ));
        assert!(chat_body_without_response_format(body.clone())
            .get("response_format")
            .is_none());
        assert!(!should_retry_chat_without_response_format(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"bad model","type":"invalid_request_error"}}"#,
            &body,
        ));
        assert!(!should_retry_chat_without_response_format(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"response_format unsupported"}}"#,
            &body,
        ));
    }

    #[tokio::test]
    async fn chat_forward_retries_without_response_format_after_format_400() {
        let hits = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post({
                let hits = hits.clone();
                move |body: Bytes| {
                    let hits = hits.clone();
                    async move {
                        let hit = hits.fetch_add(1, Ordering::SeqCst);
                        let value: Value = serde_json::from_slice(&body).unwrap();
                        if hit == 0 {
                            assert_eq!(value["response_format"]["type"], "json_object");
                            (
                                StatusCode::BAD_REQUEST,
                                axum::Json(json!({
                                    "error": {
                                        "message": "This response_format type is unavailable now",
                                        "type": "invalid_request_error"
                                    }
                                })),
                            )
                                .into_response()
                        } else {
                            assert!(value.get("response_format").is_none());
                            axum::Json(json!({
                                "choices": [{
                                    "message": { "content": "OK" },
                                    "finish_reason": "stop"
                                }],
                                "usage": {
                                    "prompt_tokens": 3,
                                    "completion_tokens": 1,
                                    "total_tokens": 4
                                }
                            }))
                            .into_response()
                        }
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let state = ServerState {
            store,
            client: Client::new(),
        };
        let provider = Provider {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url,
            key_ref: None,
            http_proxy: Default::default(),
        };
        let matched = RouteMatch {
            model: ModelEntry {
                id: "deepseek-v4-pro".into(),
                display_name: "DeepSeek V4 Pro".into(),
                description: String::new(),
                context_window: 400_000,
                enabled: true,
                provider_id: provider.id.clone(),
                upstream_model: Some("deepseek-chat".into()),
                timeout_ms: 0,
                retry_count: 0,
                reasoning_enabled: false,
                default_reasoning_level: String::new(),
                supported_reasoning_levels: Vec::new(),
                codex_alias: None,
            },
            provider,
            upstream_model: "deepseek-chat".into(),
            timeout_ms: 0,
            retry_count: 0,
            requested_model: "deepseek-v4-pro".into(),
            route_reason: "direct".into(),
            locked_from_model: None,
        };
        let request = json!({
            "model": "deepseek-v4-pro",
            "input": "hi",
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": { "type": "object" }
                }
            },
            "stream": false
        });

        let (response, usage) =
            forward_chat_completions(&state, &matched, request, "req_1".into(), "deepseek-v4-pro")
                .await
                .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(value["output_text"], "OK");
        assert_eq!(usage.unwrap().total_tokens, 4);
        server.abort();
    }

    #[tokio::test]
    async fn chat_forward_does_not_retry_unrelated_400() {
        let hits = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post({
                let hits = hits.clone();
                move || {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::BAD_REQUEST,
                            axum::Json(json!({
                                "error": {
                                    "message": "bad model",
                                    "type": "invalid_request_error"
                                }
                            })),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let state = ServerState {
            store,
            client: Client::new(),
        };
        let provider = Provider {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url,
            key_ref: None,
            http_proxy: Default::default(),
        };
        let matched = RouteMatch {
            model: ModelEntry {
                id: "deepseek-v4-pro".into(),
                display_name: "DeepSeek V4 Pro".into(),
                description: String::new(),
                context_window: 400_000,
                enabled: true,
                provider_id: provider.id.clone(),
                upstream_model: Some("deepseek-chat".into()),
                timeout_ms: 0,
                retry_count: 0,
                reasoning_enabled: false,
                default_reasoning_level: String::new(),
                supported_reasoning_levels: Vec::new(),
                codex_alias: None,
            },
            provider,
            upstream_model: "deepseek-chat".into(),
            timeout_ms: 0,
            retry_count: 0,
            requested_model: "deepseek-v4-pro".into(),
            route_reason: "direct".into(),
            locked_from_model: None,
        };
        let request = json!({
            "model": "deepseek-v4-pro",
            "input": "hi",
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": { "type": "object" }
                }
            },
            "stream": false
        });

        let error =
            forward_chat_completions(&state, &matched, request, "req_1".into(), "deepseek-v4-pro")
                .await
                .unwrap_err();

        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("bad model"));
        server.abort();
    }

    #[test]
    fn chat_body_converts_tools_functions_and_strict_defaults() {
        let request = json!({
            "model": "chat-route",
            "tools": [{
                "type": "function",
                "name": "lookup",
                "parameters": { "type": "object" }
            }],
            "functions": [{
                "name": "legacy_lookup",
                "description": "legacy function"
            }],
            "function_call": { "name": "legacy_lookup" },
            "input": "hi",
            "stream": false
        });

        let body = build_chat_completions_body(&request, "upstream-chat", false);

        assert_eq!(body["tools"].as_array().unwrap().len(), 2);
        assert_eq!(body["tools"][0]["function"]["name"], "lookup");
        assert_eq!(body["tools"][0]["function"]["strict"], false);
        assert_eq!(body["tools"][1]["function"]["name"], "legacy_lookup");
        assert_eq!(body["tools"][1]["function"]["strict"], false);
        assert_eq!(body["tool_choice"]["function"]["name"], "legacy_lookup");
    }

    #[test]
    fn chat_body_normalizes_parallel_tool_calls_with_intervening_message() {
        let request = json!({
            "model": "chat-route",
            "input": [
                {"role":"user","content":"inspect"},
                {"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]},
                {"type":"function_call","call_id":"A","name":"exec","arguments":"{}"},
                {"type":"function_call","call_id":"B","name":"exec","arguments":"{}"},
                {"role":"developer","content":"Approved command prefix saved"},
                {"type":"function_call_output","call_id":"A","output":"oa"},
                {"type":"function_call_output","call_id":"B","output":"ob"}
            ],
            "stream": false
        });

        let body = build_chat_completions_body(&request, "upstream-chat", false);
        let messages = chat_messages(&body);

        assert_chat_tool_invariants(messages);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["reasoning_content"], "plan");
        assert_eq!(messages[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "A");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "B");
        assert_eq!(messages[4]["role"], "system");
    }

    #[test]
    fn chat_body_drops_dangling_tool_calls_and_orphan_results() {
        let request = json!({
            "model": "chat-route",
            "input": [
                {"role":"user","content":"q"},
                {"type":"function_call","call_id":"missing","name":"exec","arguments":"{}"},
                {"type":"function_call_output","call_id":"ghost","output":"orphan"}
            ],
            "stream": false
        });

        let body = build_chat_completions_body(&request, "upstream-chat", false);
        let messages = chat_messages(&body);

        assert_chat_tool_invariants(messages);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert!(body.to_string().find("missing").is_none());
        assert!(body.to_string().find("ghost").is_none());
    }

    #[test]
    fn chat_body_skips_empty_base64_image_parts() {
        let request = json!({
            "model": "chat-route",
            "input": [{
                "role": "user",
                "content": [
                    {"type":"input_text","text":"describe"},
                    {"type":"input_image","image_url":"data:image/png;base64,   "}
                ]
            }],
            "stream": false
        });

        let body = build_chat_completions_body(&request, "upstream-chat", false);
        let content = body["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "describe");
    }

    #[test]
    fn anthropic_body_merges_adjacent_assistant_and_tool_result_turns() {
        let request = json!({
            "model": "claude-route",
            "input": [
                {
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "I will inspect it." }]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"pwd\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "/tmp/project"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_2",
                    "output": "second result"
                }
            ],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-upstream", false);

        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][1]["type"], "tool_use");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
        assert_eq!(body["messages"][1]["content"][1]["type"], "tool_result");
    }

    #[test]
    fn anthropic_thinking_with_signature_is_hidden_and_restored() {
        let upstream = json!({
            "content": [
                {
                    "type": "thinking",
                    "thinking": "private chain",
                    "signature": "sig_123"
                },
                { "type": "text", "text": "OK" }
            ]
        });
        let output = anthropic_output_items(&upstream);

        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[1]["content"][0]["text"], "OK");
        assert!(upstream.to_string().contains("private chain"));
        assert!(!serde_json::to_string(&output)
            .unwrap()
            .contains("private chain"));
        assert_eq!(
            anthropic_thinking_content_from_input_item(&output[0]).unwrap()["signature"],
            "sig_123"
        );

        let next_request = json!({
            "model": "claude-route",
            "input": output,
            "stream": false
        });
        let body = build_anthropic_body(&next_request, "claude-upstream", false);

        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"][0]["type"], "thinking");
        assert_eq!(
            body["messages"][0]["content"][0]["thinking"],
            "private chain"
        );
        assert_eq!(body["messages"][0]["content"][0]["signature"], "sig_123");
        assert_eq!(body["messages"][0]["content"][1]["text"], "OK");
    }

    #[test]
    fn anthropic_reasoning_without_signature_is_not_sent_as_thinking() {
        let request = json!({
            "model": "claude-route",
            "input": [
                {
                    "type": "reasoning",
                    "summary": [],
                    "encrypted_content": "neko-route-chat-reasoning:v1:cHJpdmF0ZQ=="
                },
                {
                    "role": "assistant",
                    "content": "OK"
                }
            ],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-upstream", false);

        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"], "OK");
        assert!(!body.to_string().contains("\"type\":\"thinking\""));
    }

    #[test]
    fn parses_chat_stream_tool_call_deltas() {
        let upstream = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "exec_command",
                            "arguments": "{\"cmd\""
                        }
                    }]
                }
            }]
        });

        let deltas = chat_tool_call_deltas(&upstream);

        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].index, 0);
        assert_eq!(deltas[0].id.as_deref(), Some("call_1"));
        assert_eq!(deltas[0].name.as_deref(), Some("exec_command"));
        assert_eq!(deltas[0].arguments.as_deref(), Some("{\"cmd\""));
    }

    #[test]
    fn anthropic_body_includes_tools_and_tool_turns() {
        let request = json!({
            "model": "claude-route",
            "tools": [{
                "type": "function",
                "name": "exec_command",
                "description": "Run a shell command",
                "parameters": {
                    "type": "object",
                    "properties": { "cmd": { "type": "string" } },
                    "required": ["cmd"]
                }
            }],
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"pwd\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "/tmp/project"
                }
            ],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-upstream", false);

        assert_eq!(body["tools"][0]["name"], "exec_command");
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][0]["content"][0]["input"]["cmd"], "pwd");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn anthropic_body_converts_data_url_images_to_image_blocks() {
        let request = json!({
            "model": "claude-route",
            "input": [{
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "Describe this" },
                    {
                        "type": "input_image",
                        "image_url": "data:image/jpeg;base64,BBBB"
                    }
                ]
            }],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-upstream", false);
        let content = body["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Describe this");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(content[1]["source"]["data"], "BBBB");
    }

    #[test]
    fn anthropic_body_keeps_clear_text_for_unconvertible_images() {
        let request = json!({
            "model": "claude-route",
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "image_url": "https://example.com/image.png"
                }]
            }],
            "stream": false
        });

        let body = build_anthropic_body(&request, "claude-upstream", false);
        let content = body["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[0]["type"], "text");
        assert!(content[0]["text"]
            .as_str()
            .unwrap()
            .contains("Anthropic routes need a base64 data URL image"));
    }

    #[test]
    fn anthropic_tool_use_returns_responses_function_items() {
        let upstream = json!({
            "content": [{
                "type": "tool_use",
                "id": "call_1",
                "name": "exec_command",
                "input": { "cmd": "pwd" }
            }]
        });

        let output = anthropic_output_items(&upstream);

        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["call_id"], "call_1");
        assert_eq!(output[0]["name"], "exec_command");
        assert_eq!(output[0]["arguments"], "{\"cmd\":\"pwd\"}");
    }

    #[test]
    fn parses_sse_data_lines() {
        let event = "event: message\ndata: {\"x\":1}\n\n";
        assert_eq!(event_data_lines(event), vec![r#"{"x":1}"#]);
    }

    #[tokio::test]
    async fn chat_stream_reasoning_content_is_hidden_and_restored() {
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::get(|| async {
                let stream = async_stream::stream! {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hidden \"}}]}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"stream reasoning\"}}]}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\n",
                    ));
                };
                axum::response::Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();

        let upstream = Client::new().get(url).send().await.unwrap();
        let response = converted_chat_sse(upstream, "req_1", "deepseek-v4-pro", store);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        let completed = completed_response_from_sse(&text);
        let output = completed["response"]["output"].as_array().unwrap();

        assert_eq!(completed["response"]["output_text"], "OK");
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[1]["content"][0]["text"], "OK");
        assert_eq!(
            chat_reasoning_content_from_input_item(&output[0]).as_deref(),
            Some("hidden stream reasoning")
        );
        assert!(!text.contains("hidden stream reasoning"));
        server.abort();
    }

    #[tokio::test]
    async fn chat_stream_reasoning_only_synthesizes_visible_text() {
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::get(|| async {
                let stream = async_stream::stream! {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking before final\"}}]}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n",
                    ));
                };
                axum::response::Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();

        let upstream = Client::new().get(url).send().await.unwrap();
        let response = converted_chat_sse(upstream, "req_1", "deepseek-reasoner", store);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        let completed = completed_response_from_sse(&text);
        let output = completed["response"]["output"].as_array().unwrap();

        assert!(text.contains("event: response.output_item.added"));
        assert!(text.contains("event: response.output_text.delta"));
        assert_eq!(completed["response"]["status"], "incomplete");
        assert_eq!(
            completed["response"]["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        assert_eq!(completed["response"]["usage"]["input_tokens"], 1);
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[1]["type"], "message");
        assert_eq!(output[1]["content"][0]["text"], "thinking before final");
        server.abort();
    }

    #[tokio::test]
    async fn chat_stream_tool_call_lifecycle_and_usage_complete() {
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::get(|| async {
                let stream = async_stream::stream! {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"exec\",\"arguments\":\"\"}}]}}]}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}]}}]}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":5,\"total_tokens\":9}}\n\n",
                    ));
                };
                axum::response::Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();

        let upstream = Client::new().get(url).send().await.unwrap();
        let response = converted_chat_sse(upstream, "req_1", "chat-tool", store);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        let completed = completed_response_from_sse(&text);
        let output = completed["response"]["output"].as_array().unwrap();

        assert!(text.contains("event: response.function_call_arguments.done"));
        assert!(text.contains("event: response.output_item.done"));
        assert_eq!(completed["response"]["usage"]["input_tokens"], 4);
        assert_eq!(completed["response"]["usage"]["output_tokens"], 5);
        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["call_id"], "call_a");
        assert_eq!(output[0]["arguments"], "{\"cmd\":\"ls\"}");
        server.abort();
    }

    #[tokio::test]
    async fn anthropic_stream_thinking_is_hidden_and_restored() {
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::get(|| async {
                let stream = async_stream::stream! {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hidden \"}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"stream\"}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig_stream\"}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":2}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"message_stop\"}\n\n",
                    ));
                };
                axum::response::Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();

        let upstream = Client::new().get(url).send().await.unwrap();
        let response = converted_anthropic_sse(upstream, "req_1", "claude-opus-4-8", store, None);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        let completed = completed_response_from_sse(&text);
        let output = completed["response"]["output"].as_array().unwrap();

        assert_eq!(completed["response"]["status"], "completed");
        assert_eq!(completed["response"]["output_text"], "OK");
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[1]["content"][0]["text"], "OK");
        assert_eq!(
            anthropic_thinking_content_from_input_item(&output[0]).unwrap()["thinking"],
            "hidden stream"
        );
        assert!(!text.contains("hidden stream"));
        server.abort();
    }

    #[tokio::test]
    async fn anthropic_stream_read_error_marks_interrupted_and_failed() {
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::get(|| async {
                let stream = async_stream::stream! {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
                    ));
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    yield Err::<Bytes, std::io::Error>(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "broken stream",
                    ));
                };
                axum::response::Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        push_pending_stream_request(
            &store,
            "req_anthropic_error",
            ProviderProtocol::AnthropicMessages,
            "claude-opus-4-8",
        )
        .await;

        let upstream = Client::new().get(url).send().await.unwrap();
        let response = converted_anthropic_sse(
            upstream,
            "req_anthropic_error",
            "claude-opus-4-8",
            store.clone(),
            None,
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        let failed = failed_response_from_sse(&text);
        let page = store.request_log_page(1, 1).await;

        assert_eq!(
            failed["response"]["error"]["code"],
            "upstream_stream_interrupted"
        );
        assert_eq!(page.records[0].stream_state.as_deref(), Some("interrupted"));
        assert!(page.records[0].stream_error.is_some());
        assert_eq!(
            page.records[0].last_event.as_deref(),
            Some("content_block_delta")
        );
        server.abort();
    }

    #[tokio::test]
    async fn anthropic_stream_without_message_stop_marks_incomplete() {
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::get(|| async {
                let stream = async_stream::stream! {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
                    ));
                };
                axum::response::Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        push_pending_stream_request(
            &store,
            "req_anthropic_incomplete",
            ProviderProtocol::AnthropicMessages,
            "claude-opus-4-8",
        )
        .await;

        let upstream = Client::new().get(url).send().await.unwrap();
        let response = converted_anthropic_sse(
            upstream,
            "req_anthropic_incomplete",
            "claude-opus-4-8",
            store.clone(),
            None,
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        let completed = completed_response_from_sse(&text);
        let page = store.request_log_page(1, 1).await;

        assert_eq!(completed["response"]["status"], "incomplete");
        assert_eq!(
            completed["response"]["incomplete_details"]["reason"],
            "stream_ended_without_message_stop"
        );
        assert_eq!(page.records[0].stream_state.as_deref(), Some("incomplete"));
        assert!(page.records[0]
            .stream_error
            .as_deref()
            .unwrap()
            .contains("content_block_delta"));
        assert_eq!(
            page.records[0].last_event.as_deref(),
            Some("content_block_delta")
        );
        server.abort();
    }

    #[tokio::test]
    async fn anthropic_stream_max_tokens_maps_to_incomplete() {
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::get(|| async {
                let stream = async_stream::stream! {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":8}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"message_stop\"}\n\n",
                    ));
                };
                axum::response::Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        push_pending_stream_request(
            &store,
            "req_anthropic_max_tokens",
            ProviderProtocol::AnthropicMessages,
            "claude-opus-4-8",
        )
        .await;

        let upstream = Client::new().get(url).send().await.unwrap();
        let response = converted_anthropic_sse(
            upstream,
            "req_anthropic_max_tokens",
            "claude-opus-4-8",
            store.clone(),
            None,
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        let completed = completed_response_from_sse(&text);
        let page = store.request_log_page(1, 1).await;

        assert_eq!(completed["response"]["status"], "incomplete");
        assert_eq!(
            completed["response"]["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        assert_eq!(completed["response"]["output_text"], "partial");
        assert_eq!(page.records[0].stream_state.as_deref(), Some("incomplete"));
        assert!(page.records[0].stream_error.is_none());
        assert_eq!(
            page.records[0].last_event.as_deref(),
            Some("response.completed")
        );
        server.abort();
    }

    #[tokio::test]
    async fn anthropic_stream_tool_use_completes_function_call() {
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::get(|| async {
                let stream = async_stream::stream! {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"exec_command\",\"input\":{}}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\\\"pwd\\\"}\"}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":5}}\n\n",
                    ));
                    yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"type\":\"message_stop\"}\n\n",
                    ));
                };
                axum::response::Response::builder()
                    .status(200)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        push_pending_stream_request(
            &store,
            "req_anthropic_tool",
            ProviderProtocol::AnthropicMessages,
            "claude-opus-4-8",
        )
        .await;

        let upstream = Client::new().get(url).send().await.unwrap();
        let response = converted_anthropic_sse(
            upstream,
            "req_anthropic_tool",
            "claude-opus-4-8",
            store.clone(),
            None,
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&body);
        let completed = completed_response_from_sse(&text);
        let output = completed["response"]["output"].as_array().unwrap();
        let page = store.request_log_page(1, 1).await;

        assert_eq!(completed["response"]["status"], "completed");
        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["call_id"], "toolu_1");
        assert_eq!(output[0]["name"], "exec_command");
        assert_eq!(output[0]["arguments"], "{\"cmd\":\"pwd\"}");
        assert_eq!(page.records[0].stream_state.as_deref(), Some("completed"));
        assert_eq!(
            page.records[0].last_event.as_deref(),
            Some("response.completed")
        );
        server.abort();
    }

    #[test]
    fn anthropic_non_stream_max_tokens_maps_to_incomplete() {
        let upstream = json!({
            "stop_reason": "max_tokens",
            "content": [{
                "type": "text",
                "text": "partial"
            }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 3
            }
        });

        let response = anthropic_response_json(
            "req_1",
            "claude-opus-4-8",
            &upstream,
            upstream.get("usage").cloned(),
        );

        assert_eq!(response["status"], "incomplete");
        assert_eq!(
            response["incomplete_details"]["reason"],
            "max_output_tokens"
        );
        assert_eq!(response["output_text"], "partial");
    }

    #[test]
    fn finds_lf_and_crlf_sse_boundaries() {
        assert_eq!(find_sse_boundary("data: a\n\nrest"), Some((7, 2)));
        assert_eq!(find_sse_boundary("data: a\r\n\r\nrest"), Some((7, 4)));
    }

    #[test]
    fn responses_stream_includes_output_lifecycle_events() {
        let mut sequence = 0;
        let mut events = response_stream_start_events("abc", "gpt-5.5", "msg_abc", &mut sequence);
        events.extend(response_stream_done_events(
            "abc",
            "gpt-5.5",
            "msg_abc",
            "OK",
            &mut sequence,
        ));
        let output = events
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes.as_ref()).to_string())
            .collect::<Vec<_>>()
            .join("");

        assert!(output.contains("event: response.output_item.added"));
        assert!(output.contains("event: response.content_part.added"));
        assert!(output.contains("event: response.output_text.done"));
        assert!(output.contains("event: response.output_item.done"));
        assert!(output.contains("\"output_text\":\"OK\""));
    }

    #[test]
    fn distinguishes_api_keys_from_codex_account_tokens() {
        assert!(is_public_openai_api_key("Bearer sk-test"));
        assert!(!is_public_openai_api_key("Bearer eyJhbGciOi"));
    }

    #[test]
    fn stored_openai_account_uses_chatgpt_codex_endpoint() {
        let provider = Provider {
            id: "openai-account".into(),
            name: "OpenAI Account".into(),
            kind: ProviderKind::OfficialOpenAiAccount,
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            key_ref: None,
            http_proxy: Default::default(),
        };
        let headers = HeaderMap::new();

        let url = responses_proxy_url(
            ResponsesAuthMode::StoredOfficialAccount,
            &headers,
            &provider,
        )
        .unwrap();

        assert_eq!(url, "https://chatgpt.com/backend-api/codex/responses");
    }

    #[test]
    fn direct_proxy_keeps_original_body_without_model_override() {
        let config = default_config();
        let matched = match_route(&config, "gpt-5.5").unwrap();
        let raw = Bytes::from_static(br#"{"model":"gpt-5.5","stream":true}"#);
        let body = json!({"model": "gpt-5.5", "stream": true});

        let proxied = responses_proxy_body(body, raw.clone(), "gpt-5.5", &matched).unwrap();

        assert_eq!(proxied, raw);
    }

    #[test]
    fn direct_proxy_rewrites_only_upstream_model() {
        let mut config = default_config();
        config
            .models
            .iter_mut()
            .find(|model| model.id == "gpt-5.5")
            .unwrap()
            .upstream_model = Some("gpt-upstream".into());
        let matched = match_route(&config, "gpt-5.5").unwrap();
        let raw = Bytes::from_static(br#"{"model":"gpt-5.5","input":"OK","stream":false}"#);
        let body = json!({"model": "gpt-5.5", "input": "OK", "stream": false});

        let proxied = responses_proxy_body(body, raw, "gpt-5.5", &matched).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&proxied).unwrap();

        assert_eq!(value["model"], "gpt-upstream");
        assert_eq!(value["input"], "OK");
        assert_eq!(value["stream"], false);
    }

    #[test]
    fn responses_proxy_preserves_image_and_file_inputs_when_rewriting_model() {
        let mut config = default_config();
        config
            .models
            .iter_mut()
            .find(|model| model.id == "gpt-5.5")
            .unwrap()
            .upstream_model = Some("gpt-upstream".into());
        let matched = match_route(&config, "gpt-5.5").unwrap();
        let body = json!({
            "model": "gpt-5.5",
            "input": [{
                "role": "user",
                "content": [
                    { "type": "input_text", "text": "Use both attachments" },
                    { "type": "input_image", "image_url": "data:image/png;base64,AAAA" },
                    { "type": "input_file", "file_id": "file_123" }
                ]
            }],
            "stream": false
        });
        let raw = Bytes::from(serde_json::to_vec(&body).unwrap());

        let proxied = responses_proxy_body(body, raw, "gpt-5.5", &matched).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&proxied).unwrap();
        let content = value["input"][0]["content"].as_array().unwrap();

        assert_eq!(value["model"], "gpt-upstream");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,AAAA");
        assert_eq!(content[2]["type"], "input_file");
        assert_eq!(content[2]["file_id"], "file_123");
    }

    #[test]
    fn internal_codex_proxy_rewrites_to_locked_default_model() {
        let mut config = default_config();
        config.providers.push(Provider {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: "https://deepseek.example/v1".into(),
            key_ref: Some("provider:deepseek".into()),
            http_proxy: Default::default(),
        });
        let mut deepseek = config.models[0].clone();
        deepseek.id = "deepseek-v4-pro".into();
        deepseek.provider_id = "deepseek".into();
        deepseek.upstream_model = Some("deepseek-chat".into());
        config.models.push(deepseek);
        config.settings.codex_default_model = Some("deepseek-v4-pro".into());

        let matched = match_route(&config, "gpt-5.4-mini").unwrap();
        let raw = Bytes::from_static(br#"{"model":"gpt-5.4-mini","input":"OK","stream":false}"#);
        let body = json!({"model": "gpt-5.4-mini", "input": "OK", "stream": false});

        let proxied = responses_proxy_body(body, raw, "gpt-5.4-mini", &matched).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&proxied).unwrap();

        assert_eq!(matched.model.id, "deepseek-v4-pro");
        assert_eq!(matched.route_reason, "codex_internal_locked");
        assert_eq!(value["model"], "deepseek-chat");
        assert_eq!(value["input"], "OK");
    }

    #[test]
    fn upstream_error_keeps_matched_model_for_request_log() {
        let mut config = default_config();
        config.settings.codex_default_model = Some("claude-opus-4-8".into());

        let matched = match_route(&config, "gpt-5.4").unwrap();
        let error = RouteError::new(StatusCode::UNAUTHORIZED, "upstream_error", "bad token")
            .with_match(&matched);

        assert_eq!(error.record_model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(error.requested_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(error.route_reason.as_deref(), Some("codex_internal_locked"));
        assert_eq!(
            error.provider_name.as_deref(),
            Some("Claude Code CLI Official")
        );
    }

    #[test]
    fn proxy_request_headers_are_not_fully_raw_for_sensitive_fields() {
        assert!(should_skip_proxy_request_header("authorization"));
        assert!(should_skip_proxy_request_header("content-type"));
        assert!(should_skip_proxy_request_header("host"));
        assert!(!should_skip_proxy_request_header("x-codex-installation-id"));
    }

    #[tokio::test]
    async fn official_proxy_request_headers_force_identity_encoding() {
        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        let state = super::ServerState {
            store,
            client: Client::new(),
        };
        let config = default_config();
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == "openai-official")
            .unwrap();
        let mut inbound = HeaderMap::new();
        inbound.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer session-token"),
        );
        inbound.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, br"),
        );

        let headers = proxy_request_headers(
            &state,
            &state.client,
            &inbound,
            provider,
            ResponsesAuthMode::CodexOfficial,
        )
        .await
        .unwrap();
        let encodings = headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("accept-encoding"))
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>();

        assert_eq!(encodings, vec!["identity"]);
    }

    #[tokio::test]
    async fn raw_stream_body_error_is_not_retried_after_response_starts() {
        let hits = Arc::new(AtomicUsize::new(0));
        let app = axum::Router::new().route(
            "/responses",
            axum::routing::post({
                let hits = hits.clone();
                move || {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let stream = async_stream::stream! {
                            yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                                b"event: response.output_text.delta\ndata: {\"delta\":\"x\"}\n\n",
                            ));
                            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                            yield Err::<Bytes, std::io::Error>(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "upstream cut connection",
                            ));
                        };
                        axum::response::Response::builder()
                            .status(200)
                            .header(header::CONTENT_TYPE, "text/event-stream")
                            .body(axum::body::Body::from_stream(stream))
                            .unwrap()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/responses", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;

        let response = post_bytes_with_retries(
            &Client::new(),
            &url,
            Vec::new(),
            Bytes::from_static(b"{}"),
            5_000,
            3,
        )
        .await
        .unwrap();
        let mut stream = response.bytes_stream();

        assert!(stream.next().await.unwrap().is_ok());
        assert!(stream.next().await.unwrap().is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        server.abort();
    }

    fn raw_context(streaming: bool) -> RawProxyContext {
        RawProxyContext {
            request_id: "req_1".into(),
            status: 200,
            content_type: Some("text/event-stream".into()),
            streaming,
        }
    }

    fn completed_response_from_sse(text: &str) -> Value {
        text.split("\n\n")
            .flat_map(event_data_lines)
            .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
            .find(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
            .expect("response.completed event")
    }

    fn failed_response_from_sse(text: &str) -> Value {
        text.split("\n\n")
            .flat_map(event_data_lines)
            .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
            .find(|value| value.get("type").and_then(Value::as_str) == Some("response.failed"))
            .expect("response.failed event")
    }

    async fn push_pending_stream_request(
        store: &AppStore,
        id: &str,
        protocol: ProviderProtocol,
        model: &str,
    ) {
        store
            .push_request(RequestRecord {
                id: id.into(),
                started_at: Utc::now(),
                model: model.into(),
                requested_model: None,
                route_reason: Some("direct".into()),
                provider_id: Some("provider-1".into()),
                provider_name: Some("Provider 1".into()),
                provider_protocol: Some(protocol),
                status: 200,
                latency_ms: 10,
                streaming: true,
                error: None,
                reasoning_effort: Some("max".into()),
                stream_state: Some("pending".into()),
                stream_error: None,
                last_event: None,
                stream_bytes: 0,
                context_bridge: None,
                usage: TokenUsage::default(),
            })
            .await;
    }

    #[test]
    fn raw_stream_completed_event_marks_completed() {
        let mut observer = RawSseObserver::default();
        observer.observe(&Bytes::from_static(
            b"event: response.created\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
        ));

        let outcome = classify_raw_stream_finish(&raw_context(true), &observer, None).unwrap();

        assert_eq!(outcome.state, "completed");
        assert_eq!(outcome.last_event.as_deref(), Some("response.completed"));
        assert!(outcome.stream_error.is_none());
    }

    #[test]
    fn raw_stream_failed_event_records_error_message() {
        let mut observer = RawSseObserver::default();
        observer.observe(&Bytes::from_static(
            br#"event: error
data: {"type":"error","error":{"code":"context_length_exceeded","message":"Your input exceeds the context window."}}

event: response.failed
data: {"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window."}}}

"#,
        ));

        let outcome = classify_raw_stream_finish(&raw_context(true), &observer, None).unwrap();

        assert_eq!(outcome.state, "failed");
        assert_eq!(outcome.last_event.as_deref(), Some("response.failed"));
        assert_eq!(
            outcome.stream_error.as_deref(),
            Some("context_length_exceeded: Your input exceeds the context window.")
        );
    }

    #[test]
    fn raw_stream_read_error_marks_interrupted() {
        let mut observer = RawSseObserver::default();
        observer.observe(&Bytes::from_static(
            b"event: response.output_text.delta\ndata: {\"delta\":\"x\"}\n\n",
        ));

        let outcome =
            classify_raw_stream_finish(&raw_context(true), &observer, Some("decode failed".into()))
                .unwrap();

        assert_eq!(outcome.state, "interrupted");
        assert_eq!(outcome.stream_error.as_deref(), Some("decode failed"));
        assert_eq!(
            outcome.last_event.as_deref(),
            Some("response.output_text.delta")
        );
    }

    #[test]
    fn raw_stream_without_terminal_event_marks_incomplete() {
        let mut observer = RawSseObserver::default();
        observer.observe(&Bytes::from_static(
            b"event: response.output_text.delta\ndata: {\"delta\":\"x\"}\n\n",
        ));

        let outcome = classify_raw_stream_finish(&raw_context(true), &observer, None).unwrap();

        assert_eq!(outcome.state, "incomplete");
        assert_eq!(
            outcome.stream_error.as_deref(),
            Some("stream ended before terminal event")
        );
    }

    #[test]
    fn raw_non_stream_json_end_marks_completed() {
        let context = RawProxyContext {
            content_type: Some("application/json".into()),
            ..raw_context(false)
        };
        let observer = RawSseObserver::default();

        let outcome = classify_raw_stream_finish(&context, &observer, None).unwrap();

        assert_eq!(outcome.state, "completed");
    }

    #[tokio::test]
    async fn raw_stream_records_api_byte_progress() {
        let first = Bytes::from_static(
            b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\n",
        );
        let second = Bytes::from_static(
            b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":2,\"output_tokens\":1,\"total_tokens\":3}}}\n\n",
        );
        let expected_len = (first.len() + second.len()) as u64;
        let app = axum::Router::new().route(
            "/responses",
            axum::routing::post({
                let first = first.clone();
                let second = second.clone();
                move || {
                    let first = first.clone();
                    let second = second.clone();
                    async move {
                        let stream = async_stream::stream! {
                            yield Ok::<Bytes, std::io::Error>(first);
                            yield Ok::<Bytes, std::io::Error>(second);
                        };
                        axum::response::Response::builder()
                            .status(200)
                            .header(header::CONTENT_TYPE, "text/event-stream")
                            .body(axum::body::Body::from_stream(stream))
                            .unwrap()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/responses", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::task::yield_now().await;

        let temp = tempfile::tempdir().unwrap();
        let store = AppStore::load(temp.path().to_path_buf()).unwrap();
        store
            .push_request(RequestRecord {
                id: "req_1".into(),
                started_at: Utc::now(),
                model: "gpt-5.5".into(),
                requested_model: None,
                route_reason: Some("direct".into()),
                provider_id: Some("openai-official".into()),
                provider_name: Some("OpenAI Official Account".into()),
                provider_protocol: Some(ProviderProtocol::OpenAiResponses),
                status: 200,
                latency_ms: 10,
                streaming: true,
                error: None,
                reasoning_effort: Some("xhigh".into()),
                stream_state: Some("pending".into()),
                stream_error: None,
                last_event: None,
                stream_bytes: 0,
                context_bridge: None,
                usage: TokenUsage::default(),
            })
            .await;
        let response = Client::new().post(&url).send().await.unwrap();
        let proxied = proxy_raw(response, store.clone(), raw_context(true));
        let _ = axum::body::to_bytes(proxied.into_body(), usize::MAX)
            .await
            .unwrap();

        let page = store.request_log_page(1, 1).await;
        assert_eq!(page.records[0].stream_bytes, expected_len);
        assert_eq!(page.records[0].usage.total_tokens, 3);
        server.abort();
    }

    #[test]
    fn official_endpoint_uses_token_type() {
        let config = default_config();
        let provider = config
            .providers
            .iter()
            .find(|provider| provider.id == "openai-official")
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer sk-test"),
        );
        assert_eq!(
            official_responses_endpoint(&headers, provider).unwrap(),
            "https://api.openai.com/v1/responses"
        );

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer eyJhbGciOi"),
        );
        assert_eq!(
            official_responses_endpoint(&headers, provider).unwrap(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
    }

    #[test]
    fn joins_endpoint_without_double_slash() {
        assert_eq!(
            endpoint("https://example.com/v1/", "/responses"),
            "https://example.com/v1/responses"
        );
    }
}
