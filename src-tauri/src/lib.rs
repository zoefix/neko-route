mod catalog;
mod claude_auth;
mod codex_alias;
mod codex_config;
mod key_store;
mod official_auth;
mod official_usage;
mod pricing;
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
        AppConfig, AppSnapshot, Provider, ProviderKind, ProviderProtocol, RequestLogPage,
        TokenUsage,
    },
    usage::parse_usage,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
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
use std::{env, ffi::OsStr, path::PathBuf};
use tauri::{
    menu::MenuBuilder,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use tokio::sync::Mutex;

#[tauri::command]
async fn get_snapshot(store: tauri::State<'_, AppStore>) -> Result<AppSnapshot, String> {
    Ok(store.snapshot().await)
}

#[tauri::command]
async fn save_config(
    store: tauri::State<'_, AppStore>,
    config: AppConfig,
) -> Result<AppSnapshot, String> {
    store.replace_config(config).await?;
    Ok(store.snapshot().await)
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
    let client = reqwest::Client::new();
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
    let client = reqwest::Client::new();
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
    let client = reqwest::Client::new();
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
    let client = reqwest::Client::new();
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
        let running = codex_desktop_running();
        if running {
            stop_codex_desktop()?;
        }
        start_codex_desktop()?;
        Ok(CodexAppRestartResult {
            action: codex_restart_action(running),
        })
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

#[derive(Serialize)]
struct TestModelResult {
    ok: bool,
    status: u16,
    latency_ms: u128,
    reply: String,
    error: Option<String>,
    usage: TokenUsage,
    provider_name: String,
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

    test_matched_model(store.inner(), matched).await
}

async fn test_matched_model(
    store: &AppStore,
    matched: RouteMatch,
) -> Result<TestModelResult, String> {
    let client = reqwest::Client::new();
    let (url, headers, payload) = test_request_parts(store, &client, &matched).await?;

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
        });
    }

    let reply = test_reply_from_value(&matched.provider.protocol, &value);
    let usage = value
        .get("usage")
        .filter(|u| u.is_object())
        .map(|u| parse_usage(matched.provider.protocol.clone(), u))
        .unwrap_or_default();

    Ok(TestModelResult {
        ok: true,
        status,
        latency_ms,
        reply,
        error: None,
        usage,
        provider_name: matched.provider.name.clone(),
    })
}

async fn test_request_parts(
    store: &AppStore,
    client: &reqwest::Client,
    matched: &RouteMatch,
) -> Result<(String, Vec<(String, String)>, Value), String> {
    let (base_url, headers) = test_provider_upstream(store, client, matched).await?;
    let upstream = &matched.upstream_model;
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
        ProviderProtocol::AnthropicMessages => Ok((
            endpoint(&base_url, "messages"),
            with_anthropic_version(headers),
            json!({
                "model": upstream,
                "messages": [{ "role": "user", "content": "hi" }],
                "stream": false,
                "max_tokens": 16
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
                match &matched.provider.protocol {
                    ProviderProtocol::AnthropicMessages => {
                        headers.push(("x-api-key".into(), secret))
                    }
                    _ => headers.push(("authorization".into(), format!("Bearer {secret}"))),
                }
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

fn with_anthropic_version(mut headers: Vec<(String, String)>) -> Vec<(String, String)> {
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("anthropic-version"))
    {
        headers.push(("anthropic-version".into(), "2023-06-01".into()));
    }
    headers
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
    let mut value = last?;
    fill_output_text(
        &mut value,
        fallback_output_text(final_text.as_deref(), &delta_text),
    );
    Some(value)
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
        ProviderProtocol::OpenAiChatCompletions => value
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
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

    if let Some(models) = openai_official_upstream_models(provider) {
        return Ok(UpstreamModelList {
            models,
            error: None,
        });
    }

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
            let auth = official_auth::auth_for_provider(&reqwest::Client::new(), provider).await?;
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
                if anthropic {
                    headers.push(("x-api-key".into(), secret));
                } else {
                    headers.push(("authorization".into(), format!("Bearer {secret}")));
                }
            }
            (provider.base_url.clone(), headers, anthropic)
        }
    };

    let client = reqwest::Client::new();
    let models = fetch_upstream_models(&client, &base_url, &headers, anthropic).await?;
    Ok(UpstreamModelList {
        models,
        error: None,
    })
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
fn codex_desktop_running() -> bool {
    windows_codex_process_names().iter().any(|process_name| {
        let filter = format!("IMAGENAME eq {process_name}");
        windows_command("tasklist")
            .args(["/FI", &filter, "/NH"])
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .is_some_and(|output| {
                output
                    .to_ascii_lowercase()
                    .contains(&process_name.to_ascii_lowercase())
            })
    })
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
fn dedupe_windows_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
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
fn start_codex_desktop() -> Result<(), String> {
    let candidates = dedupe_windows_paths(
        windows_codex_app_path_from_registry()
            .into_iter()
            .chain(windows_codex_paths_from_path())
            .chain(windows_codex_common_paths())
            .chain(windows_codex_shortcut_paths())
            .collect(),
    );

    let mut errors = Vec::new();
    for path in candidates.iter().filter(|path| path.exists()) {
        match shell_execute_windows(path.as_os_str()) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }

    for target in ["Codex.exe", "Codex"] {
        match shell_execute_windows(OsStr::new(target)) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(format!("{target}: {error}")),
        }
    }

    if errors.is_empty() {
        Err("Could not start Codex desktop app".into())
    } else {
        Err(format!(
            "Could not start Codex desktop app. Tried: {}",
            errors.join("; ")
        ))
    }
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
    let client = reqwest::Client::new();
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
    store.inject_codex_config_for(&config, default_model.as_deref())
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
        codex_responses_test_payload, codex_restart_action, openai_official_test_headers,
        openai_official_upstream_models, provider_error_message, provider_status_error,
        test_reply_from_value, test_response_value,
    };
    use crate::types::{Provider, ProviderKind, ProviderProtocol};

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
    fn codex_restart_action_reflects_previous_running_state() {
        assert_eq!(codex_restart_action(false), "started");
        assert_eq!(codex_restart_action(true), "restarted");
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
        };
        let added = Provider {
            id: "openai-account".into(),
            name: "OpenAI Account".into(),
            kind: ProviderKind::OfficialOpenAiAccount,
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            key_ref: Some("official-token:openai-account".into()),
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
        .text("add_provider", "添加服务商")
        .text("add_model", "添加模型")
        .separator()
        .text("show_main", "显示主窗口")
        .text("quit", "退出")
        .build()?;

    let menu_exit_requested = exit_requested.clone();
    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Neko Route")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "add_provider" => {
                show_main_window(app);
                let _ = app.emit("neko-route://add-provider", ());
            }
            "add_model" => {
                show_main_window(app);
                let _ = app.emit("neko-route://add-model", ());
            }
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
            let server_store = store.clone();
            let catalog_store = store.clone();
            app.manage(store);
            app.manage(OpenAiOAuthSessions::default());
            app.manage(ClaudeOAuthSessions::default());
            tauri::async_runtime::spawn(async move {
                let codex_home = codex_config::resolve_codex_home();
                let _ = catalog_store.export_catalog_to(&codex_home).await;
            });
            tauri::async_runtime::spawn(async move {
                if let Err(error) = server::run(server_store.clone()).await {
                    let config = server_store.config().await;
                    let bind_url = format!(
                        "http://{}:{}/v1",
                        config.settings.bind_host, config.settings.port
                    );
                    server_store.set_server_error(bind_url, error).await;
                }
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
            set_provider_key,
            delete_provider_key,
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
            list_upstream_models,
            refresh_provider_usage,
            export_catalog,
            install_codex_config,
            restore_codex_config,
            read_codex_config,
            save_codex_config,
            import_sessions,
            clear_request_logs,
            get_request_logs
        ])
        .run(tauri::generate_context!())
        .expect("error while running Neko Route");
}
