use crate::{
    claude_auth, official_auth, official_usage,
    redact::redact,
    router::{match_route, RouteMatch},
    store::{validate_bind_settings, AppStore},
    types::{Provider, ProviderKind, ProviderProtocol, RequestRecord, TokenUsage},
    usage::{parse_usage, usage_from_responses_text},
};
use axum::{
    body::Body,
    extract::State,
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
use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, time::sleep};
use uuid::Uuid;

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
    provider_id: Option<String>,
    provider_name: Option<String>,
    provider_protocol: Option<ProviderProtocol>,
}

impl RouteError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: redact(&message.into()),
            provider_id: None,
            provider_name: None,
            provider_protocol: None,
        }
    }

    fn with_provider(mut self, provider: &Provider) -> Self {
        self.provider_id = Some(provider.id.clone());
        self.provider_name = Some(provider.name.clone());
        self.provider_protocol = Some(provider.protocol.clone());
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

pub async fn run(store: AppStore) -> Result<(), String> {
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
        .route("/v1/responses", post(responses))
        .with_state(state);

    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())
}

async fn health(State(state): State<ServerState>) -> Json<Value> {
    let config = state.store.config().await;
    let keys = state.store.key_statuses(&config);
    Json(json!({
        "ok": true,
        "service": "neko-route",
        "providers": config.providers.len(),
        "models": config.models.iter().filter(|model| model.enabled).count(),
        "keys": keys,
    }))
}

async fn models(State(state): State<ServerState>) -> Json<Value> {
    let config = state.store.config().await;
    let data = config
        .models
        .into_iter()
        .filter(|model| model.enabled)
        .map(|model| {
            json!({
                "id": model.id,
                "object": "model",
                "created": 0,
                "owned_by": "neko-route"
            })
        })
        .collect::<Vec<_>>();
    Json(json!({ "object": "list", "data": data }))
}

/// Privacy-safe diagnostic: append reasoning-relevant fields of an incoming
/// Codex request to ~/.neko-route-debug/reasoning-trace.jsonl, AND dump the
/// full pretty-printed body to ~/.neko-route-debug/full/<model>-<n>.json so we
/// can inspect the exact wire shape with no ambiguity.
fn trace_reasoning(body: &Value, model: &str) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let dir = home.join(".neko-route-debug");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let top_keys: Vec<&str> = body
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let entry = json!({
        "ts": Utc::now().to_rfc3339(),
        "model": model,
        "top_level_keys": top_keys,
        "reasoning": body.get("reasoning"),
        "reasoning_effort": body.get("reasoning_effort"),
        "effort": body.get("effort"),
        "output_config": body.get("output_config"),
    });

    // Full body dump (latest few requests). Rotate by keeping a timestamped name.
    let full_dir = dir.join("full");
    if std::fs::create_dir_all(&full_dir).is_ok() {
        let stamp = Utc::now().format("%H%M%S%3f");
        let safe_model = model.replace(['/', ':'], "_");
        let path = full_dir.join(format!("{safe_model}-{stamp}.json"));
        if let Ok(pretty) = serde_json::to_string_pretty(body) {
            let _ = std::fs::write(path, pretty);
        }
    }

    if let Ok(line) = serde_json::to_string(&entry) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("reasoning-trace.jsonl"))
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Dump the RAW inbound request: exact bytes to <id>.raw, and a sidecar <id>.meta
/// with byte length + key headers. This proves whether what we capture is the
/// complete, uncompressed request body Codex sent.
fn dump_raw_request(headers: &HeaderMap, body: &Bytes, request_id: &str) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let dir = home.join(".neko-route-debug").join("raw");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let stamp = Utc::now().format("%H%M%S%3f");
    let _ = std::fs::write(dir.join(format!("{stamp}-{request_id}.raw")), &body[..]);

    let header_of = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("(absent)")
            .to_string()
    };
    // All header names (values omitted except non-sensitive ones) so we can spot
    // a reasoning value hiding in a header rather than the body.
    let all_header_names: Vec<String> = headers.keys().map(|k| k.as_str().to_string()).collect();
    let meta = json!({
        "ts": Utc::now().to_rfc3339(),
        "body_bytes_len": body.len(),
        "content_length_header": header_of("content-length"),
        "content_encoding": header_of("content-encoding"),
        "content_type": header_of("content-type"),
        "transfer_encoding": header_of("transfer-encoding"),
        "all_header_names": all_header_names,
    });
    if let Ok(pretty) = serde_json::to_string_pretty(&meta) {
        let _ = std::fs::write(dir.join(format!("{stamp}-{request_id}.meta")), pretty);
    }
}

async fn responses(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body_bytes: Bytes,
) -> Response {
    let started = std::time::Instant::now();
    let request_id = Uuid::new_v4().to_string();

    // DIAGNOSTIC: dump the RAW request bytes verbatim (no parse, no re-serialize)
    // plus size + key headers, so we can prove whether the captured body is the
    // complete package or truncated/compressed.
    dump_raw_request(&headers, &body_bytes, &request_id);

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

    // Privacy-safe trace: capture ONLY reasoning-related fields from the
    // incoming Codex request (never prompts/code) so we can verify the wire
    // shape. Written to ~/.neko-route-debug/reasoning-trace.jsonl.
    trace_reasoning(&parsed_body, &model);
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
        error,
        usage,
    ) = match &result {
        Ok((response, matched, usage)) => (
            response.status().as_u16(),
            matched.model.id.clone(),
            (matched.model.id != matched.requested_model).then(|| matched.requested_model.clone()),
            Some(matched.route_reason.clone()),
            Some(matched.provider.id.clone()),
            Some(matched.provider.name.clone()),
            Some(matched.provider.protocol.clone()),
            None,
            usage.unwrap_or_default(),
        ),
        Err(error) => (
            error.status.as_u16(),
            model.clone(),
            None,
            None,
            error.provider_id.clone(),
            error.provider_name.clone(),
            error.provider_protocol.clone(),
            Some(error.message.clone()),
            TokenUsage::default(),
        ),
    };
    let stream_state = initial_stream_state(status, provider_protocol.as_ref());

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
            usage,
        })
        .await;

    match result {
        Ok((response, _, _)) => response,
        Err(error) => error.into_response(),
    }
}

async fn route_response(
    state: ServerState,
    headers: HeaderMap,
    mut body: Value,
    body_bytes: Bytes,
    request_id: String,
) -> Result<(Response, RouteMatch, Option<TokenUsage>), RouteError> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            RouteError::new(StatusCode::BAD_REQUEST, "invalid_request", "Missing model")
        })?
        .to_string();
    let config = state.store.config().await;
    let matched = match_route(&config, &model).map_err(|message| {
        RouteError::new(StatusCode::NOT_FOUND, "model_not_configured", message)
    })?;
    let response_result = match &matched.provider.kind {
        ProviderKind::OfficialOpenAi => {
            forward_responses_proxy(
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
        }
        ProviderKind::OfficialOpenAiAccount => {
            forward_responses_proxy(
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
        }
        ProviderKind::OfficialAnthropicCli
        | ProviderKind::OfficialAnthropicDesktop
        | ProviderKind::OfficialAnthropicAccount => {
            body["model"] = Value::String(matched.upstream_model.clone());
            forward_anthropic(&state, &matched, body, request_id, &model).await
        }
        ProviderKind::Custom => match &matched.provider.protocol {
            ProviderProtocol::OpenAiResponses => {
                forward_responses_proxy(
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
            }
            ProviderProtocol::OpenAiChatCompletions => {
                body["model"] = Value::String(matched.upstream_model.clone());
                forward_chat_completions(&state, &matched, body, request_id, &model).await
            }
            ProviderProtocol::AnthropicMessages => {
                body["model"] = Value::String(matched.upstream_model.clone());
                forward_anthropic(&state, &matched, body, request_id, &model).await
            }
        },
    };
    let (response, usage) =
        response_result.map_err(|error| error.with_provider(&matched.provider))?;

    Ok((response, matched, usage))
}

fn initial_stream_state(status: u16, protocol: Option<&ProviderProtocol>) -> Option<String> {
    if (200..300).contains(&status) && protocol == Some(&ProviderProtocol::OpenAiResponses) {
        Some("pending".into())
    } else {
        None
    }
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
    let headers =
        proxy_request_headers(state, inbound_headers, &matched.provider, auth_mode).await?;
    let upstream = post_bytes_with_retries(
        &state.client,
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
    let context = RawProxyContext::from_response(
        &upstream,
        request_id.clone(),
        requested_model.to_string(),
        matched.provider.id.clone(),
        matched.provider.name.clone(),
        streaming,
        matches!(
            auth_mode,
            ResponsesAuthMode::CodexOfficial | ResponsesAuthMode::StoredOfficialAccount
        ),
    );
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
            let auth = official_auth::auth_for_provider(&state.client, provider)
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

/// Diagnostic: record the reasoning-shaping fields we send UPSTREAM (to the
/// relay/Anthropic), so we can confirm our outbound request is correct even
/// when Codex sends us `reasoning: null`. Written to
/// ~/.neko-route-debug/outbound-trace.jsonl.
fn trace_outbound(body: &Value, upstream_model: &str) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let dir = home.join(".neko-route-debug");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let entry = json!({
        "ts": Utc::now().to_rfc3339(),
        "upstream_model": upstream_model,
        "thinking": body.get("thinking"),
        "output_config": body.get("output_config"),
        "reasoning_effort": body.get("reasoning_effort"),
        "max_tokens": body.get("max_tokens"),
    });
    if let Ok(line) = serde_json::to_string(&entry) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("outbound-trace.jsonl"))
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

async fn forward_anthropic(
    state: &ServerState,
    matched: &RouteMatch,
    body: Value,
    request_id: String,
    requested_model: &str,
) -> Result<(Response, Option<TokenUsage>), RouteError> {
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    // Official Anthropic (real Opus 4.8) uses output_config.effort; custom
    // relays get the classic thinking shape they understand.
    let official = matches!(
        matched.provider.kind,
        ProviderKind::OfficialAnthropicCli
            | ProviderKind::OfficialAnthropicDesktop
            | ProviderKind::OfficialAnthropicAccount
    );
    let anthropic_body = build_anthropic_body(&body, &matched.upstream_model, stream, official);
    trace_outbound(&anthropic_body, &matched.upstream_model);
    let (base_url, mut headers) = anthropic_upstream(state, &matched.provider).await?;
    let url = endpoint(&base_url, "messages");
    headers.push(("anthropic-version".into(), "2023-06-01".into()));
    let upstream = post_json_with_retries(
        &state.client,
        &url,
        headers,
        anthropic_body,
        matched.timeout_ms,
        matched.retry_count,
    )
    .await?;
    if !upstream.status().is_success() {
        return Err(upstream_error(upstream).await);
    }
    if stream {
        Ok((
            converted_anthropic_sse(upstream, &request_id, requested_model, state.store.clone()),
            None,
        ))
    } else {
        let value = upstream_json(upstream).await?;
        let usage = value
            .get("usage")
            .map(|u| parse_usage(ProviderProtocol::AnthropicMessages, u));
        Ok((
            json_response(anthropic_response_json(
                &request_id,
                requested_model,
                &value,
                value.get("usage").cloned(),
            )),
            usage,
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
    let chat_body = build_chat_completions_body(&body, &matched.upstream_model, stream);
    let url = endpoint(&matched.provider.base_url, "chat/completions");
    let headers = provider_headers(state, &matched.provider).await?;
    let upstream = post_json_with_retries(
        &state.client,
        &url,
        headers,
        chat_body,
        matched.timeout_ms,
        matched.retry_count,
    )
    .await?;
    if !upstream.status().is_success() {
        return Err(upstream_error(upstream).await);
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
    match &provider.protocol {
        ProviderProtocol::AnthropicMessages => headers.push(("x-api-key".into(), secret)),
        _ => headers.push(("authorization".into(), format!("Bearer {secret}"))),
    }
    Ok(headers)
}

async fn anthropic_upstream(
    state: &ServerState,
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
            let auth = official_auth::auth_for_provider(&state.client, provider)
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
    model: String,
    provider_id: String,
    provider_name: String,
    status: u16,
    content_type: Option<String>,
    content_encoding: Option<String>,
    transfer_encoding: Option<String>,
    streaming: bool,
    official: bool,
}

impl RawProxyContext {
    fn from_response(
        response: &reqwest::Response,
        request_id: String,
        model: String,
        provider_id: String,
        provider_name: String,
        streaming: bool,
        official: bool,
    ) -> Self {
        Self {
            request_id,
            model,
            provider_id,
            provider_name,
            status: response.status().as_u16(),
            content_type: response_header(response, header::CONTENT_TYPE.as_str()),
            content_encoding: response_header(response, header::CONTENT_ENCODING.as_str()),
            transfer_encoding: response_header(response, header::TRANSFER_ENCODING.as_str()),
            streaming,
            official,
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
    if context.official {
        trace_official_stream_outcome(&context, &outcome);
    }
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
        return Some(RawStreamOutcome::new(
            terminal_stream_state(event).unwrap_or("completed"),
            None,
            observer.last_event.clone(),
        ));
    }

    Some(RawStreamOutcome::new(
        "incomplete",
        Some("stream ended before terminal event".into()),
        observer.last_event.clone(),
    ))
}

fn trace_official_stream_outcome(context: &RawProxyContext, outcome: &RawStreamOutcome) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let dir = home.join(".neko-route-debug");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let entry = json!({
        "ts": Utc::now().to_rfc3339(),
        "provider_id": context.provider_id.clone(),
        "provider_name": context.provider_name.clone(),
        "model": context.model.clone(),
        "status": context.status,
        "streaming": context.streaming,
        "content_type": context.content_type.clone(),
        "content_encoding": context.content_encoding.clone(),
        "transfer_encoding": context.transfer_encoding.clone(),
        "stream_state": outcome.state,
        "stream_error": outcome.stream_error.clone(),
        "last_event": outcome.last_event.clone(),
    });
    if let Ok(line) = serde_json::to_string(&entry) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("official-stream-trace.jsonl"))
        {
            let _ = writeln!(f, "{line}");
        }
    }
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
        let mut sequence_number = 0_u64;
        let mut full_text = String::new();
        let mut reasoning_content = String::new();
        let mut response_started = false;
        let mut reasoning_output_index: Option<usize> = None;
        let mut text_output_index: Option<usize> = None;
        let mut tool_calls: Vec<StreamingToolCall> = Vec::new();
        let mut pending = String::new();
        let mut captured_usage: Option<TokenUsage> = None;
        let mut progress = StreamProgressTracker::new(store.clone(), request_id.clone());
        let mut upstream = response.bytes_stream();

        while let Some(chunk) = upstream.next().await {
            let Ok(bytes) = chunk else {
                continue;
            };
            progress.observe_bytes(&bytes).await;
            pending.push_str(&String::from_utf8_lossy(&bytes));
            while let Some((index, boundary_len)) = find_sse_boundary(&pending) {
                let event = pending[..index].to_string();
                pending = pending[index + boundary_len..].to_string();
                for data in event_data_lines(&event) {
                    if data == "[DONE]" {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<Value>(&data) else {
                        continue;
                    };
                    if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
                        let usage = parse_usage(ProviderProtocol::OpenAiChatCompletions, usage);
                        captured_usage = Some(usage);
                        progress.observe_usage(usage).await;
                    }
                    if let Some(delta) = value
                        .pointer("/choices/0/delta/reasoning_content")
                        .and_then(Value::as_str)
                    {
                        if !delta.is_empty() {
                            reasoning_output_index.get_or_insert_with(|| {
                                next_chat_output_index(None, text_output_index, &tool_calls)
                            });
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

        let mut output = Vec::new();
        if !reasoning_content.is_empty() {
            let output_index = reasoning_output_index.unwrap_or_else(|| next_chat_output_index(None, text_output_index, &tool_calls));
            output.push((output_index, chat_reasoning_output_item(&format!("rsn_{request_id}"), &reasoning_content)));
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
        yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.completed", json!({
            "type": "response.completed",
            "response": response_object(&request_id, &model, "completed", output, usage_json)
        })));
        progress.finish(captured_usage).await;
        store.update_request_stream(
            request_id.clone(),
            "completed".into(),
            None,
            Some("response.completed".into()),
        ).await;
    };

    sse_response(Body::from_stream(stream))
}

fn converted_anthropic_sse(
    response: reqwest::Response,
    request_id: &str,
    model: &str,
    store: AppStore,
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
        let mut pending = String::new();
        let mut anthropic_usage = serde_json::Map::new();
        let mut progress = StreamProgressTracker::new(store.clone(), request_id.clone());
        let mut upstream = response.bytes_stream();

        while let Some(chunk) = upstream.next().await {
            let Ok(bytes) = chunk else {
                continue;
            };
            progress.observe_bytes(&bytes).await;
            pending.push_str(&String::from_utf8_lossy(&bytes));
            while let Some((index, boundary_len)) = find_sse_boundary(&pending) {
                let event = pending[..index].to_string();
                pending = pending[index + boundary_len..].to_string();
                for data in event_data_lines(&event) {
                    let Ok(value) = serde_json::from_str::<Value>(&data) else {
                        continue;
                    };
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
        output.sort_by_key(|(index, _)| *index);
        let output = output.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
        let captured_usage = if anthropic_usage.is_empty() {
            None
        } else {
            Some(parse_usage(ProviderProtocol::AnthropicMessages, &Value::Object(anthropic_usage)))
        };
        let usage_json = captured_usage.map(usage_to_responses_json);
        yield Ok::<Bytes, Infallible>(sequenced_sse_event(&mut sequence_number, "response.completed", json!({
            "type": "response.completed",
            "response": response_object(&request_id, &model, "completed", output, usage_json)
        })));
        progress.finish(captured_usage).await;
        store.update_request_stream(
            request_id.clone(),
            "completed".into(),
            None,
            Some("response.completed".into()),
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

fn build_anthropic_body(
    request: &Value,
    upstream_model: &str,
    stream: bool,
    official: bool,
) -> Value {
    let (system_parts, messages) = anthropic_messages_from_request(request);
    let mut max_tokens = request
        .get("max_output_tokens")
        .or_else(|| request.get("max_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(1024);

    let mut body = json!({
        "model": upstream_model,
        "messages": messages,
        "stream": stream
    });

    // System as cacheable block(s): tag the last block with cache_control so the
    // upstream (or relay) creates/reads a prompt cache over the stable prefix.
    if !system_parts.is_empty() {
        let joined = system_parts.join("\n\n");
        body["system"] = json!([{
            "type": "text",
            "text": joined,
            "cache_control": { "type": "ephemeral" }
        }]);
    }

    let mut tools = anthropic_tools_from_request(request);
    if !tools.is_empty() {
        // Mark the last tool with cache_control: tools sit ahead of system in the
        // cache prefix, so this caches the whole (tools + system) block.
        if let Some(last) = tools.last_mut() {
            if let Some(obj) = last.as_object_mut() {
                obj.insert("cache_control".into(), json!({ "type": "ephemeral" }));
            }
        }
        body["tools"] = Value::Array(tools);
    }

    let effort = map_reasoning_effort(request);
    if official {
        // Real Anthropic (Opus 4.8 adaptive) takes the modern effort tier and
        // rejects manual thinking budgets. Only override when Codex sent one.
        if let Some(effort) = effort {
            body["output_config"] = json!({ "effort": effort });
        }
    } else {
        // Third-party relays: ALWAYS send reasoning so their dashboards populate
        // and the model thinks. Default to `xhigh` when Codex omitted it (matches
        // the reference custom-provider behavior). Send all shapes a relay might
        // read: output_config.effort, top-level reasoning_effort, and the classic
        // thinking budget.
        let effort = effort.unwrap_or("xhigh");
        let budget = effort_budget_tokens(effort);
        body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        body["output_config"] = json!({ "effort": effort });
        body["reasoning_effort"] = json!(effort);
        max_tokens = max_tokens.max(budget + 8192);
    }

    body["max_tokens"] = json!(max_tokens);
    body
}

/// Concrete thinking budgets for the classic relay shape.
fn effort_budget_tokens(effort: &str) -> u64 {
    match effort {
        "low" => 4_096,
        "medium" => 8_192,
        "high" => 16_384,
        "xhigh" => 24_576,
        "max" => 32_768,
        _ => 8_192,
    }
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

fn build_chat_completions_body(request: &Value, upstream_model: &str, stream: bool) -> Value {
    let mut body = json!({
        "model": upstream_model,
        "messages": chat_messages_from_request(request),
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
        .get("max_output_tokens")
        .or_else(|| request.get("max_tokens"))
    {
        body["max_tokens"] = max_tokens.clone();
    }
    for key in [
        "temperature",
        "top_p",
        "stop",
        "presence_penalty",
        "frequency_penalty",
    ] {
        if let Some(value) = request.get(key) {
            body[key] = value.clone();
        }
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
                if let Some(reasoning) = chat_reasoning_content_from_input_item(item) {
                    pending_reasoning = Some(reasoning);
                    continue;
                }
                let reasoning = if is_chat_assistant_like_input_item(item) {
                    pending_reasoning.take()
                } else {
                    None
                };
                if let Some(message) = chat_message_from_input_item(item, reasoning.as_deref()) {
                    messages.push(message);
                }
            }
        }
        _ => messages.push(json!({ "role": "user", "content": "" })),
    }
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": "" }));
    }
    messages
}

fn chat_message_from_input_item(item: &Value, pending_reasoning: Option<&str>) -> Option<Value> {
    let reasoning_content = item
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or(pending_reasoning);
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            let call_id = item.get("call_id").and_then(Value::as_str)?.to_string();
            let name = item.get("name").and_then(Value::as_str)?.to_string();
            let arguments = value_to_argument_string(item.get("arguments"));
            let mut message = json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments
                    }
                }]
            });
            attach_reasoning_content(&mut message, reasoning_content);
            Some(message)
        }
        Some("function_call_output") => {
            let call_id = item.get("call_id").and_then(Value::as_str)?.to_string();
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
        _ => {
            let role = normalize_message_role(item.get("role").and_then(Value::as_str));
            let role = match role {
                "assistant" => "assistant",
                "system" => "system",
                "tool" => "tool",
                _ => "user",
            };
            let content = text_from_content(item.get("content").unwrap_or(item));
            if role == "assistant" {
                let tool_calls = item.get("tool_calls").cloned();
                if content.is_empty() && reasoning_content.is_none() && tool_calls.is_none() {
                    return None;
                }
                let mut message = if content.is_empty() {
                    json!({ "role": "assistant", "content": Value::Null })
                } else {
                    json!({ "role": "assistant", "content": content })
                };
                if let Some(tool_calls) = tool_calls {
                    message["tool_calls"] = tool_calls;
                }
                attach_reasoning_content(&mut message, reasoning_content);
                Some(message)
            } else if content.is_empty() {
                None
            } else if role == "tool" {
                Some(json!({
                    "role": "tool",
                    "tool_call_id": item.get("tool_call_id").or_else(|| item.get("call_id")).and_then(Value::as_str).unwrap_or("tool_call"),
                    "content": content
                }))
            } else {
                Some(json!({ "role": role, "content": content }))
            }
        }
    }
}

fn is_chat_assistant_like_input_item(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("function_call")
        || normalize_message_role(item.get("role").and_then(Value::as_str)) == "assistant"
}

fn attach_reasoning_content(message: &mut Value, reasoning_content: Option<&str>) {
    if let Some(reasoning_content) = reasoning_content.filter(|value| !value.is_empty()) {
        message["reasoning_content"] = json!(reasoning_content);
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

fn chat_tools_from_request(request: &Value) -> Vec<Value> {
    request
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(chat_tool_from_responses_tool)
                .collect()
        })
        .unwrap_or_default()
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
    let mut function = json!({
        "name": name,
        "description": description,
        "parameters": parameters
    });
    if let Some(strict) = tool
        .get("strict")
        .or_else(|| tool.pointer("/function/strict"))
    {
        function["strict"] = strict.clone();
    }
    Some(json!({
        "type": "function",
        "function": function
    }))
}

fn chat_tool_choice_from_request(request: &Value) -> Option<Value> {
    let choice = request.get("tool_choice")?;
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

fn anthropic_messages_from_request(request: &Value) -> (Vec<String>, Vec<Value>) {
    let mut system_parts = Vec::new();
    let mut messages = Vec::new();
    if let Some(instructions) = request.get("instructions").and_then(Value::as_str) {
        if !instructions.is_empty() {
            system_parts.push(instructions.to_string());
        }
    }
    let source = request.get("input").or_else(|| request.get("messages"));
    match source {
        Some(Value::String(text)) => messages.push(json!({ "role": "user", "content": text })),
        Some(Value::Array(items)) => {
            for item in items {
                match anthropic_message_from_input_item(item) {
                    AnthropicInputMessage::System(text) => system_parts.push(text),
                    AnthropicInputMessage::Message(message) => messages.push(message),
                    AnthropicInputMessage::Empty => {}
                }
            }
        }
        _ => messages.push(json!({ "role": "user", "content": "" })),
    }
    if messages.is_empty() {
        messages.push(json!({ "role": "user", "content": "" }));
    }
    (system_parts, messages)
}

enum AnthropicInputMessage {
    System(String),
    Message(Value),
    Empty,
}

fn anthropic_message_from_input_item(item: &Value) -> AnthropicInputMessage {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => {
            let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                return AnthropicInputMessage::Empty;
            };
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                return AnthropicInputMessage::Empty;
            };
            AnthropicInputMessage::Message(json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": arguments_to_object(item.get("arguments"))
                }]
            }))
        }
        Some("function_call_output") => {
            let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                return AnthropicInputMessage::Empty;
            };
            AnthropicInputMessage::Message(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": value_to_text(item.get("output").or_else(|| item.get("content")).unwrap_or(&Value::Null))
                }]
            }))
        }
        _ => {
            let role = normalize_message_role(item.get("role").and_then(Value::as_str));
            let content = text_from_content(item.get("content").unwrap_or(item));
            if content.is_empty() {
                AnthropicInputMessage::Empty
            } else if role == "system" {
                AnthropicInputMessage::System(content)
            } else {
                let role = if role == "assistant" {
                    "assistant"
                } else {
                    "user"
                };
                AnthropicInputMessage::Message(json!({ "role": role, "content": content }))
            }
        }
    }
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
    response_object(
        request_id,
        model,
        "completed",
        chat_completion_output_items(value),
        usage,
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
    if !text.is_empty() {
        output.push(message_output_item("msg_0", "completed", Some(&text)));
    }
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
    if output.is_empty() {
        output.push(message_output_item("msg_0", "completed", Some("")));
    }
    output
}

fn anthropic_response_json(
    request_id: &str,
    model: &str,
    value: &Value,
    usage: Option<Value>,
) -> Value {
    response_object(
        request_id,
        model,
        "completed",
        anthropic_output_items(value),
        usage,
    )
}

fn anthropic_output_items(value: &Value) -> Vec<Value> {
    let mut output = Vec::new();
    let mut text_index = 0_usize;
    if let Some(items) = value.get("content").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
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
    let output_text = output_text_from_items(&output);
    json!({
        "id": format!("resp_{request_id}"),
        "object": "response",
        "created_at": Utc::now().timestamp(),
        "status": status,
        "model": model,
        "output": output,
        "output_text": output_text,
        "usage": usage
    })
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
        anthropic_output_items, build_anthropic_body, build_chat_completions_body,
        chat_completion_output_items, chat_completion_response_json,
        chat_reasoning_content_from_input_item, chat_tool_call_deltas, classify_raw_stream_finish,
        converted_chat_sse, endpoint, event_data_lines, extract_stream_delta, find_sse_boundary,
        is_public_openai_api_key, map_openai_chat_reasoning_effort, map_reasoning_effort,
        official_responses_endpoint, post_bytes_with_retries, proxy_raw, proxy_request_headers,
        response_stream_done_events, response_stream_start_events, responses_proxy_body,
        responses_proxy_url, should_skip_proxy_request_header, RawProxyContext, RawSseObserver,
        ResponsesAuthMode,
    };
    use crate::{
        router::match_route,
        store::AppStore,
        types::{
            default_config, Provider, ProviderKind, ProviderProtocol, RequestRecord, TokenUsage,
        },
    };
    use axum::http::{header, HeaderMap, HeaderValue};
    use bytes::Bytes;
    use chrono::Utc;
    use futures_util::StreamExt;
    use reqwest::Client;
    use serde_json::{json, Value};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn moves_developer_messages_to_anthropic_system() {
        let request = json!({
            "model": "claude-3-5-haiku",
            "input": [
                {"role": "developer", "content": "follow policy"},
                {"role": "user", "content": "Reply OK"}
            ],
            "stream": false
        });
        let body = build_anthropic_body(&request, "claude-3-5-haiku", false, false);
        assert_eq!(body["system"][0]["text"], "follow policy");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["messages"][0]["role"], "user");
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
    fn official_anthropic_uses_output_config_effort() {
        let request = json!({
            "model": "claude-opus-4-8",
            "input": "hi",
            "reasoning": { "effort": "high" },
            "stream": false
        });
        let body = build_anthropic_body(&request, "claude-opus-4-8", false, true);
        assert_eq!(body["output_config"]["effort"], "high");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn relay_uses_classic_thinking_with_budget() {
        let request = json!({
            "model": "claude-opus-4-8",
            "input": "hi",
            "reasoning": { "effort": "high" },
            "max_output_tokens": 1024,
            "stream": false
        });
        let body = build_anthropic_body(&request, "claude-opus-4-8", false, false);
        // Relays get BOTH thinking + effort so their reasoning column populates.
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 16_384);
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["reasoning_effort"], "high");
        // max_tokens bumped above the budget so the request is valid.
        assert!(body["max_tokens"].as_u64().unwrap() > 16_384);
    }

    #[test]
    fn relay_defaults_reasoning_when_codex_omits_it() {
        // No reasoning in the request: relays still get a sensible default so
        // their dashboards populate and the model thinks.
        let request = json!({ "model": "claude-opus-4-8", "input": "hi", "stream": false });
        let body = build_anthropic_body(&request, "claude-opus-4-8", false, false);
        assert_eq!(body["output_config"]["effort"], "xhigh");
        assert_eq!(body["reasoning_effort"], "xhigh");
        assert_eq!(body["thinking"]["type"], "enabled");
    }

    #[test]
    fn official_omits_reasoning_when_codex_omits_it() {
        // Official Anthropic must NOT receive a fabricated effort or manual
        // thinking (Opus 4.8 adaptive rejects the latter).
        let request = json!({ "model": "claude-opus-4-8", "input": "hi", "stream": false });
        let body = build_anthropic_body(&request, "claude-opus-4-8", false, true);
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
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
        assert_eq!(body["max_tokens"], 77);
        assert_eq!(body["temperature"], 0.2);
    }

    #[test]
    fn wraps_chat_completion_text_as_responses_json() {
        let upstream = json!({
            "choices": [
                { "message": { "content": "OK" } }
            ],
            "usage": { "total_tokens": 3 }
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
            "input": output,
            "stream": false
        });
        let body = build_chat_completions_body(&next_request, "deepseek-upstream", false);

        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"], Value::Null);
        assert_eq!(body["messages"][0]["reasoning_content"], "need a tool");
        assert_eq!(body["messages"][0]["tool_calls"][0]["id"], "call_1");
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

        let body = build_anthropic_body(&request, "claude-upstream", false, false);

        assert_eq!(body["tools"][0]["name"], "exec_command");
        assert_eq!(body["messages"][0]["role"], "assistant");
        assert_eq!(body["messages"][0]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][0]["content"][0]["input"]["cmd"], "pwd");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
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
            enabled: true,
            key_ref: None,
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
    fn internal_codex_proxy_rewrites_to_locked_default_model() {
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

        let headers =
            proxy_request_headers(&state, &inbound, provider, ResponsesAuthMode::CodexOfficial)
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
            model: "gpt-5.5".into(),
            provider_id: "openai-official".into(),
            provider_name: "OpenAI Official Account".into(),
            status: 200,
            content_type: Some("text/event-stream".into()),
            content_encoding: None,
            transfer_encoding: None,
            streaming,
            official: true,
        }
    }

    fn completed_response_from_sse(text: &str) -> Value {
        text.split("\n\n")
            .flat_map(event_data_lines)
            .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
            .find(|value| value.get("type").and_then(Value::as_str) == Some("response.completed"))
            .expect("response.completed event")
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
