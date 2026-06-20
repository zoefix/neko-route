use crate::{
    codex_config,
    types::{KeyStatus, Provider, ProviderKind},
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use keyring::Entry;
use reqwest::Client;
use serde_json::{json, Map, Value};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use url::Url;
use uuid::Uuid;

const KEYCHAIN_SERVICE: &str = "Neko Route Official Tokens";
const TOKEN_FILE_REF_PREFIX: &str = "file:v1:";
const DIRECT_KEYCHAIN_UTF16_LIMIT: usize = 2400;
const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const OPENAI_AUTHORIZE_SCOPE: &str = "openid profile email offline_access";
const OPENAI_REFRESH_SCOPE: &str = "openid profile email";
const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const OPENAI_CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_BASE_URL: &str = "https://claude.ai";
const ANTHROPIC_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const ANTHROPIC_AUTHORIZE_SCOPE: &str =
    "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const ANTHROPIC_COOKIE_SCOPE: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
const REFRESH_SKEW_SECONDS: i64 = 300;
const OPENAI_OAUTH_SESSION_TTL_SECONDS: i64 = 30 * 60;
const CLAUDE_OAUTH_SESSION_TTL_SECONDS: i64 = 30 * 60;
const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20,claude-code-20250219";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficialAccountKind {
    OpenAi,
    Anthropic,
}

#[derive(Debug, Clone)]
pub struct OfficialAuth {
    pub base_url: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ParsedToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub token_type: String,
}

#[derive(Debug, Clone)]
pub struct OpenAiOAuthSession {
    pub session_id: String,
    pub state: String,
    pub code_verifier: String,
    pub auth_url: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ClaudeOAuthSession {
    pub session_id: String,
    pub state: String,
    pub code_verifier: String,
    pub auth_url: String,
    pub expires_at: DateTime<Utc>,
}

pub fn account_kind(provider: &Provider) -> Option<OfficialAccountKind> {
    match provider.kind {
        ProviderKind::OfficialOpenAiAccount => Some(OfficialAccountKind::OpenAi),
        ProviderKind::OfficialAnthropicAccount => Some(OfficialAccountKind::Anthropic),
        _ => None,
    }
}

pub fn token_ref(provider_id: &str) -> String {
    format!("official-token:{provider_id}")
}

pub fn openai_codex_base_url() -> &'static str {
    OPENAI_CODEX_BASE_URL
}

pub fn openai_codex_usage_url() -> &'static str {
    OPENAI_CODEX_USAGE_URL
}

pub fn openai_builtin_models() -> Vec<(String, String)> {
    [
        ("gpt-5.5", "GPT-5.5"),
        ("gpt-5.4", "GPT-5.4"),
        ("gpt-5.4-mini", "GPT-5.4 Mini"),
        ("gpt-5.3-codex", "GPT-5.3 Codex"),
        ("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark"),
        ("codex-auto-review", "Codex Auto Review"),
        ("gpt-5.2", "GPT-5.2"),
        ("codex-mini-latest", "Codex Mini"),
    ]
    .into_iter()
    .map(|(id, label)| (id.to_string(), label.to_string()))
    .collect()
}

pub fn set_provider_token(provider_id: &str, raw_json: &str) -> Result<(), String> {
    let value = parse_json_value(raw_json)?;
    set_provider_token_value(provider_id, value)
}

pub fn set_provider_token_value(provider_id: &str, value: Value) -> Result<(), String> {
    let token = parse_token_value(&value)?;
    let normalized = normalized_token_value(value, &token);
    write_keychain_value(&token_ref(provider_id), &normalized)
}

pub fn start_openai_oauth_session() -> OpenAiOAuthSession {
    let session_id = random_oauth_id();
    let state = random_oauth_secret();
    let code_verifier = random_oauth_secret();
    let code_challenge = openai_oauth_code_challenge(&code_verifier);
    let mut url = Url::parse(OPENAI_AUTHORIZE_URL).expect("OpenAI OAuth authorize URL is valid");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OPENAI_CLIENT_ID)
        .append_pair("redirect_uri", OPENAI_REDIRECT_URI)
        .append_pair("scope", OPENAI_AUTHORIZE_SCOPE)
        .append_pair("state", &state)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true");
    OpenAiOAuthSession {
        session_id,
        state,
        code_verifier,
        auth_url: url.to_string(),
        expires_at: Utc::now() + Duration::seconds(OPENAI_OAUTH_SESSION_TTL_SECONDS),
    }
}

pub fn start_claude_oauth_session() -> ClaudeOAuthSession {
    let session_id = random_oauth_id();
    let state = random_oauth_secret();
    let code_verifier = random_oauth_secret();
    let code_challenge = claude_oauth_code_challenge(&code_verifier);
    let mut url = Url::parse(ANTHROPIC_AUTHORIZE_URL).expect("Claude OAuth authorize URL is valid");
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", ANTHROPIC_CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", ANTHROPIC_REDIRECT_URI)
        .append_pair("scope", ANTHROPIC_AUTHORIZE_SCOPE)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);
    ClaudeOAuthSession {
        session_id,
        state,
        code_verifier,
        auth_url: url.to_string(),
        expires_at: Utc::now() + Duration::seconds(CLAUDE_OAUTH_SESSION_TTL_SECONDS),
    }
}

pub fn openai_oauth_code_challenge(code_verifier: &str) -> String {
    oauth_code_challenge(code_verifier)
}

pub fn claude_oauth_code_challenge(code_verifier: &str) -> String {
    oauth_code_challenge(code_verifier)
}

fn oauth_code_challenge(code_verifier: &str) -> String {
    let hash = Sha256::digest(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

fn random_oauth_id() -> String {
    Uuid::new_v4().simple().to_string()
}

fn random_oauth_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn extract_openai_oauth_code(input: &str, expected_state: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("OpenAI OAuth callback URL or code is required".into());
    }

    if let Ok(url) = Url::parse(trimmed) {
        return extract_openai_oauth_code_from_pairs(url.query_pairs(), Some(expected_state));
    }

    if trimmed.contains('=') {
        let query = trimmed
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or(trimmed);
        let pairs = url::form_urlencoded::parse(query.as_bytes());
        return extract_openai_oauth_code_from_pairs(pairs, Some(expected_state));
    }

    Ok(trimmed.to_string())
}

fn extract_openai_oauth_code_from_pairs<'a, I>(
    pairs: I,
    expected_state: Option<&str>,
) -> Result<String, String>
where
    I: IntoIterator<Item = (std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>)>,
{
    let mut code = None;
    let mut state = None;
    for (key, value) in pairs {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(expected) = expected_state {
        if let Some(actual) = state.as_deref() {
            if actual != expected {
                return Err("OpenAI OAuth state does not match this authorization session".into());
            }
        }
    }
    code.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "OpenAI OAuth callback does not contain code".to_string())
}

pub fn extract_claude_oauth_code(input: &str, expected_state: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Claude OAuth callback URL or code is required".into());
    }

    if let Ok(url) = Url::parse(trimmed) {
        return extract_claude_oauth_code_from_pairs(url.query_pairs(), Some(expected_state));
    }

    if trimmed.contains('=') {
        let query = trimmed
            .split_once('?')
            .map(|(_, query)| query)
            .unwrap_or(trimmed);
        let pairs = url::form_urlencoded::parse(query.as_bytes());
        return extract_claude_oauth_code_from_pairs(pairs, Some(expected_state));
    }

    let (code, state) = split_claude_code_state(trimmed);
    if let Some(actual) = state.as_deref() {
        if actual != expected_state {
            return Err("Claude OAuth state does not match this authorization session".into());
        }
    }
    code.filter(|value| !value.is_empty())
        .map(|value| combine_claude_code_state(&value, state.as_deref()))
        .ok_or_else(|| "Claude OAuth callback does not contain code".to_string())
}

fn extract_claude_oauth_code_from_pairs<'a, I>(
    pairs: I,
    expected_state: Option<&str>,
) -> Result<String, String>
where
    I: IntoIterator<Item = (std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>)>,
{
    let mut code = None;
    let mut state = None;
    for (key, value) in pairs {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(expected) = expected_state {
        if let Some(actual) = state.as_deref() {
            if actual != expected {
                return Err("Claude OAuth state does not match this authorization session".into());
            }
        }
    }
    code.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| combine_claude_code_state(&value, state.as_deref()))
        .ok_or_else(|| "Claude OAuth callback does not contain code".to_string())
}

fn split_claude_code_state(value: &str) -> (Option<String>, Option<String>) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    if let Some((code, state)) = trimmed.split_once('#') {
        let code = code.trim();
        let state = state.trim();
        return (
            (!code.is_empty()).then(|| code.to_string()),
            (!state.is_empty()).then(|| state.to_string()),
        );
    }
    (Some(trimmed.to_string()), None)
}

fn combine_claude_code_state(code: &str, state: Option<&str>) -> String {
    match state.map(str::trim).filter(|value| !value.is_empty()) {
        Some(state) => format!("{code}#{state}"),
        None => code.to_string(),
    }
}

pub async fn exchange_openai_oauth_code(
    client: &Client,
    code: &str,
    code_verifier: &str,
) -> Result<Value, String> {
    exchange_openai_oauth_code_at(client, OPENAI_TOKEN_URL, code, code_verifier).await
}

async fn exchange_openai_oauth_code_at(
    client: &Client,
    token_url: &str,
    code: &str,
    code_verifier: &str,
) -> Result<Value, String> {
    let response = client
        .post(token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", OPENAI_CLIENT_ID),
            ("code", code),
            ("redirect_uri", OPENAI_REDIRECT_URI),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|error| format!("Could not exchange OpenAI OAuth code: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Could not read OpenAI OAuth response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "OpenAI OAuth returned {}: {}",
            status.as_u16(),
            truncate(&text, 240)
        ));
    }
    let mut value = serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("OpenAI OAuth returned invalid JSON: {error}"))?;
    let token = parse_token_value(&value)?;
    if let Value::Object(object) = &mut value {
        object.insert("client_id".into(), Value::String(OPENAI_CLIENT_ID.into()));
        object.insert(
            "redirect_uri".into(),
            Value::String(OPENAI_REDIRECT_URI.into()),
        );
    }
    Ok(normalized_token_value(value, &token))
}

pub async fn exchange_claude_oauth_code(
    client: &Client,
    code: &str,
    code_verifier: &str,
) -> Result<Value, String> {
    exchange_claude_oauth_code_at(
        client,
        ANTHROPIC_TOKEN_URL,
        code,
        code_verifier,
        ANTHROPIC_AUTHORIZE_SCOPE,
    )
    .await
}

async fn exchange_claude_oauth_code_at(
    client: &Client,
    token_url: &str,
    code: &str,
    code_verifier: &str,
    scope: &str,
) -> Result<Value, String> {
    let (code, state) = split_claude_code_state(code);
    let code = code.ok_or_else(|| "Claude OAuth authorization code is required".to_string())?;
    let mut payload = Map::new();
    payload.insert("code".into(), Value::String(code));
    payload.insert(
        "grant_type".into(),
        Value::String("authorization_code".into()),
    );
    payload.insert(
        "client_id".into(),
        Value::String(ANTHROPIC_CLIENT_ID.into()),
    );
    payload.insert(
        "redirect_uri".into(),
        Value::String(ANTHROPIC_REDIRECT_URI.into()),
    );
    payload.insert("code_verifier".into(), Value::String(code_verifier.into()));
    if let Some(state) = state {
        payload.insert("state".into(), Value::String(state));
    }

    let response = client
        .post(token_url)
        .header("accept", "application/json, text/plain, */*")
        .header("content-type", "application/json")
        .header("user-agent", "axios/1.13.6")
        .json(&Value::Object(payload))
        .send()
        .await
        .map_err(|error| format!("Could not exchange Claude OAuth code: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Could not read Claude OAuth response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "Claude OAuth returned {}: {}",
            status.as_u16(),
            truncate(&text, 240)
        ));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("Claude OAuth returned invalid JSON: {error}"))?;
    claude_oauth_token_value(value, token_url, scope, None)
}

pub async fn exchange_claude_cookie_session_key(
    client: &Client,
    session_key: &str,
) -> Result<Value, String> {
    exchange_claude_cookie_session_key_at(
        client,
        ANTHROPIC_BASE_URL,
        ANTHROPIC_TOKEN_URL,
        session_key,
    )
    .await
}

async fn exchange_claude_cookie_session_key_at(
    client: &Client,
    base_url: &str,
    token_url: &str,
    session_key: &str,
) -> Result<Value, String> {
    let session_key = session_key.trim();
    if session_key.is_empty() {
        return Err("Claude sessionKey is required".into());
    }
    let organization_uuid =
        fetch_claude_organization_uuid_at(client, base_url, session_key).await?;
    let state = random_oauth_secret();
    let code_verifier = random_oauth_secret();
    let code_challenge = claude_oauth_code_challenge(&code_verifier);
    let code = request_claude_authorization_code_at(
        client,
        base_url,
        session_key,
        &organization_uuid,
        ANTHROPIC_COOKIE_SCOPE,
        &code_challenge,
        &state,
    )
    .await?;
    let value = exchange_claude_oauth_code_at(
        client,
        token_url,
        &code,
        &code_verifier,
        ANTHROPIC_COOKIE_SCOPE,
    )
    .await?;
    claude_oauth_token_value(
        value,
        token_url,
        ANTHROPIC_COOKIE_SCOPE,
        Some(&organization_uuid),
    )
}

async fn fetch_claude_organization_uuid_at(
    client: &Client,
    base_url: &str,
    session_key: &str,
) -> Result<String, String> {
    let url = format!("{}/api/organizations", base_url.trim_end_matches('/'));
    let response = client
        .get(&url)
        .header("accept", "application/json")
        .header("cookie", format!("sessionKey={session_key}"))
        .send()
        .await
        .map_err(|error| format!("Could not query Claude organizations: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Could not read Claude organizations response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "Claude organizations returned {}: {}",
            status.as_u16(),
            truncate(&text, 240)
        ));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("Claude organizations returned invalid JSON: {error}"))?;
    claude_organization_uuid_from_value(&value)
        .ok_or_else(|| "Claude organizations response did not include an organization".to_string())
}

pub fn claude_organization_uuid_from_value(value: &Value) -> Option<String> {
    let organizations = value.as_array()?;
    organizations
        .iter()
        .filter_map(Value::as_object)
        .find(|organization| {
            organization
                .get("raven_type")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case("team"))
                && object_string(organization, "uuid").is_some()
        })
        .and_then(|organization| object_string(organization, "uuid"))
        .or_else(|| {
            organizations
                .iter()
                .filter_map(Value::as_object)
                .find_map(|organization| object_string(organization, "uuid"))
        })
}

async fn request_claude_authorization_code_at(
    client: &Client,
    base_url: &str,
    session_key: &str,
    organization_uuid: &str,
    scope: &str,
    code_challenge: &str,
    state: &str,
) -> Result<String, String> {
    let url = format!(
        "{}/v1/oauth/{}/authorize",
        base_url.trim_end_matches('/'),
        organization_uuid
    );
    let payload = json!({
        "response_type": "code",
        "client_id": ANTHROPIC_CLIENT_ID,
        "organization_uuid": organization_uuid,
        "redirect_uri": ANTHROPIC_REDIRECT_URI,
        "scope": scope,
        "state": state,
        "code_challenge": code_challenge,
        "code_challenge_method": "S256",
    });
    let response = client
        .post(&url)
        .header("accept", "application/json")
        .header("accept-language", "en-US,en;q=0.9")
        .header("cache-control", "no-cache")
        .header("origin", "https://claude.ai")
        .header("referer", "https://claude.ai/new")
        .header("content-type", "application/json")
        .header("cookie", format!("sessionKey={session_key}"))
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("Could not authorize Claude sessionKey: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Could not read Claude authorization response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "Claude authorization returned {}: {}",
            status.as_u16(),
            truncate(&text, 240)
        ));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("Claude authorization returned invalid JSON: {error}"))?;
    let redirect_uri = find_string(&value, &["redirect_uri", "redirectUri"])
        .ok_or_else(|| "Claude authorization response did not include redirect_uri".to_string())?;
    extract_claude_oauth_code(&redirect_uri, state)
}

fn claude_oauth_token_value(
    mut value: Value,
    token_url: &str,
    scope: &str,
    organization_uuid: Option<&str>,
) -> Result<Value, String> {
    let token = parse_token_value(&value)?;
    let response_organization_uuid = value
        .get("organization")
        .and_then(|organization| find_string(organization, &["uuid"]));
    let response_account_uuid = value
        .get("account")
        .and_then(|account| find_string(account, &["uuid"]));
    let response_email = value
        .get("account")
        .and_then(|account| find_string(account, &["email_address", "email"]));
    if !value.is_object() {
        value = json!({});
    }
    if let Value::Object(object) = &mut value {
        object.insert(
            "client_id".into(),
            Value::String(ANTHROPIC_CLIENT_ID.into()),
        );
        object.insert(
            "redirect_uri".into(),
            Value::String(ANTHROPIC_REDIRECT_URI.into()),
        );
        object.insert("token_url".into(), Value::String(token_url.into()));
        object.insert("scope".into(), Value::String(scope.into()));
        if let Some(organization_uuid) = organization_uuid {
            object
                .entry("org_uuid")
                .or_insert_with(|| Value::String(organization_uuid.into()));
        }
        if let Some(organization_uuid) = response_organization_uuid {
            object
                .entry("org_uuid")
                .or_insert_with(|| Value::String(organization_uuid));
        }
        if let Some(account_uuid) = response_account_uuid {
            object
                .entry("account_uuid")
                .or_insert_with(|| Value::String(account_uuid));
        }
        if let Some(email) = response_email {
            object
                .entry("email_address")
                .or_insert_with(|| Value::String(email));
        }
    }
    Ok(normalized_token_value(value, &token))
}

pub fn delete_provider_token(provider_id: &str) -> Result<(), String> {
    delete_keychain_value(&token_ref(provider_id))
}

pub fn provider_token_json(provider_id: &str) -> Result<Option<String>, String> {
    read_keychain_value(&token_ref(provider_id))
}

pub fn status_for_provider(provider: &Provider) -> KeyStatus {
    let Some(_) = account_kind(provider) else {
        return KeyStatus {
            provider_id: provider.id.clone(),
            present: false,
            available: false,
            message: Some("Unsupported official account provider".into()),
        };
    };

    match read_provider_token(&provider.id) {
        Ok(Some((_, token))) => {
            let expired = token_is_expired(&token);
            KeyStatus {
                provider_id: provider.id.clone(),
                present: true,
                available: !expired || token.refresh_token.is_some(),
                message: expired.then(|| "key.expired".into()),
            }
        }
        Ok(None) => KeyStatus {
            provider_id: provider.id.clone(),
            present: false,
            available: false,
            message: Some("key.notSignedIn".into()),
        },
        Err(message) => KeyStatus {
            provider_id: provider.id.clone(),
            present: false,
            available: false,
            message: Some(message),
        },
    }
}

pub async fn auth_for_provider(
    client: &Client,
    provider: &Provider,
) -> Result<OfficialAuth, String> {
    let kind = account_kind(provider)
        .ok_or_else(|| "Unsupported official account provider".to_string())?;
    let (mut value, mut token) = read_provider_token(&provider.id)?
        .ok_or_else(|| format!("Provider '{}' is not signed in", provider.name))?;

    if should_refresh(&token) {
        value = refresh_token(client, kind, value, &token).await?;
        token = parse_token_value(&value)?;
        write_keychain_value(&token_ref(&provider.id), &value)?;
    }

    if token_is_expired(&token) {
        return Err(format!(
            "Provider '{}' token expired. Paste a fresh token JSON.",
            provider.name
        ));
    }

    match kind {
        OfficialAccountKind::OpenAi => {
            let account_id = openai_chatgpt_account_id(&value).ok_or_else(|| {
                format!(
                    "Provider '{}' token JSON is missing chatgpt_account_id. Paste a Codex/OpenAI account token JSON.",
                    provider.name
                )
            })?;
            Ok(OfficialAuth {
                base_url: OPENAI_CODEX_BASE_URL.into(),
                headers: openai_codex_headers(&token.access_token, &account_id),
            })
        }
        OfficialAccountKind::Anthropic => {
            let auth_value = format!("{} {}", token.token_type, token.access_token);
            Ok(OfficialAuth {
                base_url: provider.base_url.clone(),
                headers: vec![
                    ("content-type".into(), "application/json".into()),
                    ("authorization".into(), auth_value),
                    ("anthropic-beta".into(), ANTHROPIC_OAUTH_BETA.into()),
                ],
            })
        }
    }
}

pub fn openai_codex_headers(access_token: &str, account_id: &str) -> Vec<(String, String)> {
    vec![
        ("content-type".into(), "application/json".into()),
        ("authorization".into(), format!("Bearer {access_token}")),
        ("chatgpt-account-id".into(), account_id.to_string()),
        ("oai-language".into(), "zh-CN".into()),
        ("originator".into(), "Codex Desktop".into()),
        (
            "accept".into(),
            "text/event-stream, application/json".into(),
        ),
        ("accept-encoding".into(), "identity".into()),
        ("openai-beta".into(), "responses=experimental".into()),
        ("sec-fetch-site".into(), "none".into()),
        ("sec-fetch-mode".into(), "no-cors".into()),
        ("sec-fetch-dest".into(), "empty".into()),
        ("priority".into(), "u=4, i".into()),
    ]
}

pub async fn openai_token_for_provider(
    client: &Client,
    provider: &Provider,
) -> Result<(Value, ParsedToken, String), String> {
    if account_kind(provider) != Some(OfficialAccountKind::OpenAi) {
        return Err("Provider is not an OpenAI official account".into());
    }
    let (mut value, mut token) = read_provider_token(&provider.id)?
        .ok_or_else(|| format!("Provider '{}' is not signed in", provider.name))?;
    if should_refresh(&token) {
        value = refresh_token(client, OfficialAccountKind::OpenAi, value, &token).await?;
        token = parse_token_value(&value)?;
        write_keychain_value(&token_ref(&provider.id), &value)?;
    }
    if token_is_expired(&token) {
        return Err(format!(
            "Provider '{}' token expired. Paste a fresh token JSON.",
            provider.name
        ));
    }
    let account_id = openai_chatgpt_account_id(&value).ok_or_else(|| {
        format!(
            "Provider '{}' token JSON is missing chatgpt_account_id. Paste a Codex/OpenAI account token JSON.",
            provider.name
        )
    })?;
    Ok((value, token, account_id))
}

pub async fn anthropic_token_for_provider(
    client: &Client,
    provider: &Provider,
) -> Result<(Value, ParsedToken), String> {
    if account_kind(provider) != Some(OfficialAccountKind::Anthropic) {
        return Err("Provider is not a Claude official account".into());
    }
    let (mut value, mut token) = read_provider_token(&provider.id)?
        .ok_or_else(|| format!("Provider '{}' is not signed in", provider.name))?;
    if should_refresh(&token) {
        value = refresh_token(client, OfficialAccountKind::Anthropic, value, &token).await?;
        token = parse_token_value(&value)?;
        write_keychain_value(&token_ref(&provider.id), &value)?;
    }
    if token_is_expired(&token) {
        return Err(format!(
            "Provider '{}' token expired. Paste a fresh token JSON.",
            provider.name
        ));
    }
    Ok((value, token))
}

pub async fn openai_token_for_codex(
    client: &Client,
) -> Result<(Value, ParsedToken, String), String> {
    let auth_path = codex_config::resolve_codex_home().join("auth.json");
    openai_token_from_codex_auth_file(client, &auth_path).await
}

pub fn codex_openai_status(provider: &Provider) -> KeyStatus {
    let auth_path = codex_config::resolve_codex_home().join("auth.json");
    let raw = match std::fs::read_to_string(&auth_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return KeyStatus {
                provider_id: provider.id.clone(),
                present: false,
                available: false,
                message: Some("key.notSignedIn".into()),
            };
        }
        Err(error) => {
            return KeyStatus {
                provider_id: provider.id.clone(),
                present: false,
                available: false,
                message: Some(format!("Could not read Codex auth.json: {error}")),
            };
        }
    };
    let value = match parse_json_value(&raw) {
        Ok(value) => value,
        Err(message) => {
            return KeyStatus {
                provider_id: provider.id.clone(),
                present: true,
                available: false,
                message: Some(message),
            };
        }
    };
    let token = match parse_token_value(&value) {
        Ok(token) => token,
        Err(message) => {
            return KeyStatus {
                provider_id: provider.id.clone(),
                present: true,
                available: false,
                message: Some(message),
            };
        }
    };
    if openai_chatgpt_account_id(&value).is_none() {
        return KeyStatus {
            provider_id: provider.id.clone(),
            present: true,
            available: false,
            message: Some("Codex auth.json is missing chatgpt_account_id".into()),
        };
    }
    let expired = token_is_expired(&token);
    KeyStatus {
        provider_id: provider.id.clone(),
        present: true,
        available: !expired || token.refresh_token.is_some(),
        message: expired.then(|| "key.expired".into()),
    }
}

pub async fn auth_for_codex_openai(client: &Client) -> Result<OfficialAuth, String> {
    let (_, token, account_id) = openai_token_for_codex(client).await?;
    Ok(OfficialAuth {
        base_url: OPENAI_CODEX_BASE_URL.into(),
        headers: openai_codex_headers(&token.access_token, &account_id),
    })
}

pub async fn openai_token_from_codex_auth_file(
    client: &Client,
    auth_path: &Path,
) -> Result<(Value, ParsedToken, String), String> {
    if !auth_path.exists() {
        return Err(format!(
            "Codex auth.json was not found at {}. Sign in to Codex first.",
            auth_path.display()
        ));
    }
    let raw = std::fs::read_to_string(auth_path)
        .map_err(|error| format!("Could not read Codex auth.json: {error}"))?;
    let mut value = parse_json_value(&raw)?;
    let mut token = parse_token_value(&value).map_err(|error| {
        format!("Codex auth.json does not contain OpenAI access token: {error}")
    })?;
    if should_refresh(&token) {
        value = refresh_token(client, OfficialAccountKind::OpenAi, value, &token).await?;
        token = parse_token_value(&value)?;
    }
    if token_is_expired(&token) {
        return Err("Codex auth.json token expired. Sign in to Codex again.".into());
    }
    let account_id = openai_chatgpt_account_id(&value)
        .ok_or_else(|| "Codex auth.json is missing chatgpt_account_id".to_string())?;
    Ok((value, token, account_id))
}

pub async fn refresh_provider_token(client: &Client, provider: &Provider) -> Result<(), String> {
    let kind = account_kind(provider)
        .ok_or_else(|| "Unsupported official account provider".to_string())?;
    let (value, token) = read_provider_token(&provider.id)?
        .ok_or_else(|| format!("Provider '{}' is not signed in", provider.name))?;
    let refreshed = refresh_token(client, kind, value, &token).await?;
    let token = parse_token_value(&refreshed)?;
    let normalized = normalized_token_value(refreshed, &token);
    write_keychain_value(&token_ref(&provider.id), &normalized)
}

fn read_provider_token(provider_id: &str) -> Result<Option<(Value, ParsedToken)>, String> {
    let Some(raw) = read_keychain_value(&token_ref(provider_id))? else {
        return Ok(None);
    };
    let value = parse_json_value(&raw)?;
    let token = parse_token_value(&value)?;
    Ok(Some((value, token)))
}

fn parse_json_value(raw_json: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(raw_json)
        .map_err(|error| format!("Token JSON is invalid: {error}"))
}

pub fn parse_token_value(value: &Value) -> Result<ParsedToken, String> {
    let access_token = find_string(
        value,
        &["access_token", "accessToken", "oauthToken", "oauth_token"],
    )
    .ok_or_else(|| "Token JSON must contain access_token/accessToken/oauthToken".to_string())?;
    let refresh_token = find_string(value, &["refresh_token", "refreshToken"]);
    let expires_at = find_expires_at(value);
    let token_type = find_string(value, &["token_type", "tokenType"])
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Bearer".into());
    Ok(ParsedToken {
        access_token,
        refresh_token,
        expires_at,
        token_type,
    })
}

fn normalized_token_value(mut value: Value, token: &ParsedToken) -> Value {
    if !value.is_object() {
        value = json!({ "raw": value });
    }
    let object = value
        .as_object_mut()
        .expect("normalized token value is object");
    object.insert(
        "access_token".into(),
        Value::String(token.access_token.clone()),
    );
    if let Some(refresh) = &token.refresh_token {
        object.insert("refresh_token".into(), Value::String(refresh.clone()));
    }
    if let Some(expires_at) = token.expires_at {
        object.insert("expires_at".into(), Value::String(expires_at.to_rfc3339()));
    }
    object.insert("token_type".into(), Value::String(token.token_type.clone()));
    value
}

async fn refresh_token(
    client: &Client,
    kind: OfficialAccountKind,
    original: Value,
    token: &ParsedToken,
) -> Result<Value, String> {
    let refresh = token
        .refresh_token
        .as_deref()
        .ok_or_else(|| "Token JSON does not contain refresh_token".to_string())?;
    let url = find_string(&original, &["token_url", "tokenUrl"]).unwrap_or_else(|| match kind {
        OfficialAccountKind::OpenAi => OPENAI_TOKEN_URL.into(),
        OfficialAccountKind::Anthropic => ANTHROPIC_TOKEN_URL.into(),
    });

    let response = if kind == OfficialAccountKind::OpenAi {
        let client_id = find_string(&original, &["client_id", "clientId"])
            .unwrap_or_else(|| OPENAI_CLIENT_ID.into());
        client
            .post(&url)
            .header("content-type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", client_id.as_str()),
                ("scope", OPENAI_REFRESH_SCOPE),
            ])
            .send()
            .await
            .map_err(|error| format!("Could not refresh token: {error}"))?
    } else {
        let mut payload = Map::new();
        payload.insert("grant_type".into(), Value::String("refresh_token".into()));
        payload.insert("refresh_token".into(), Value::String(refresh.into()));
        let client_id = find_string(&original, &["client_id", "clientId"])
            .unwrap_or_else(|| ANTHROPIC_CLIENT_ID.into());
        payload.insert("client_id".into(), Value::String(client_id));
        if let Some(client_secret) = find_string(&original, &["client_secret", "clientSecret"]) {
            payload.insert("client_secret".into(), Value::String(client_secret));
        }

        client
            .post(&url)
            .json(&Value::Object(payload))
            .send()
            .await
            .map_err(|error| format!("Could not refresh token: {error}"))?
    };
    if !response.status().is_success() {
        return Err(format!(
            "Token refresh returned {}",
            response.status().as_u16()
        ));
    }
    let mut refreshed = response
        .json::<Value>()
        .await
        .map_err(|error| format!("Token refresh returned invalid JSON: {error}"))?;
    let parsed = parse_token_value(&refreshed)?;
    if parsed.refresh_token.is_none() {
        if let Value::Object(object) = &mut refreshed {
            object.insert("refresh_token".into(), Value::String(refresh.into()));
        }
    }
    Ok(merge_token_values(original, refreshed))
}

fn merge_token_values(mut original: Value, refreshed: Value) -> Value {
    if !original.is_object() {
        original = json!({});
    }
    let original_object = original.as_object_mut().expect("token value is object");
    if let Value::Object(refreshed_object) = refreshed {
        for (key, value) in refreshed_object {
            original_object.insert(key, value);
        }
    }
    original
}

fn should_refresh(token: &ParsedToken) -> bool {
    let Some(expires_at) = token.expires_at else {
        return false;
    };
    token.refresh_token.is_some()
        && expires_at <= Utc::now() + Duration::seconds(REFRESH_SKEW_SECONDS)
}

fn token_is_expired(token: &ParsedToken) -> bool {
    token
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now())
}

fn find_expires_at(value: &Value) -> Option<DateTime<Utc>> {
    find_value(value, &["expires_at", "expiresAt", "expiry", "expires"])
        .and_then(parse_expires_at)
        .or_else(|| {
            find_value(value, &["expires_in", "expiresIn"]).and_then(|value| {
                parse_number_like(value)
                    .map(|seconds| Utc::now() + Duration::seconds(seconds.max(0)))
            })
        })
}

pub fn openai_chatgpt_account_id(value: &Value) -> Option<String> {
    find_string(
        value,
        &[
            "chatgpt_account_id",
            "chatgptAccountId",
            "account_id",
            "accountId",
        ],
    )
    .or_else(|| find_openai_auth_string(value, &["chatgpt_account_id"]))
    .or_else(|| {
        find_string(
            value,
            &[
                "organization_id",
                "organizationId",
                "poid",
                "org_id",
                "orgId",
            ],
        )
    })
    .or_else(|| find_openai_auth_string(value, &["poid", "organization_id"]))
}

pub fn openai_token_email(value: &Value) -> Option<String> {
    find_string(value, &["email"]).or_else(|| find_openai_auth_string(value, &["email"]))
}

pub fn openai_token_plan_type(value: &Value) -> Option<String> {
    find_string(value, &["plan_type", "planType", "chatgpt_plan_type"])
        .or_else(|| find_openai_auth_string(value, &["chatgpt_plan_type", "plan_type"]))
}

fn find_openai_auth_string(value: &Value, keys: &[&str]) -> Option<String> {
    for token_key in ["id_token", "idToken", "access_token", "accessToken"] {
        if let Some(token) = find_string(value, &[token_key]) {
            if let Some(payload) = jwt_payload_value(&token) {
                if let Some(found) = find_string(&payload, keys) {
                    return Some(found);
                }
                if let Some(auth) = payload.get("https://api.openai.com/auth") {
                    if let Some(found) = find_string(auth, keys) {
                        return Some(found);
                    }
                }
            }
        }
    }
    None
}

fn jwt_payload_value(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .or_else(|_| STANDARD.decode(payload))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn parse_expires_at(value: &Value) -> Option<DateTime<Utc>> {
    if let Some(text) = value.as_str() {
        if let Ok(date) = DateTime::parse_from_rfc3339(text) {
            return Some(date.with_timezone(&Utc));
        }
        if let Ok(number) = text.parse::<i64>() {
            return timestamp_to_datetime(number);
        }
    }
    parse_number_like(value).and_then(timestamp_to_datetime)
}

fn parse_number_like(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
}

fn timestamp_to_datetime(value: i64) -> Option<DateTime<Utc>> {
    let seconds = if value > 10_000_000_000 {
        value / 1000
    } else {
        value
    };
    Utc.timestamp_opt(seconds, 0).single()
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    find_value(value, keys).and_then(|value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn object_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn find_value<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(value) = object.get(*key) {
                    return Some(value);
                }
            }
            object.values().find_map(|value| find_value(value, keys))
        }
        Value::Array(items) => items.iter().find_map(|value| find_value(value, keys)),
        _ => None,
    }
}

fn read_keychain_value(account: &str) -> Result<Option<String>, String> {
    let entry = Entry::new(KEYCHAIN_SERVICE, account)
        .map_err(|error| format!("Could not open official token storage: {error}"))?;
    match entry.get_password() {
        Ok(value) => read_stored_token_value(account, &value).map(Some),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("Could not read official token storage: {error}")),
    }
}

fn write_keychain_value(account: &str, value: &Value) -> Result<(), String> {
    let serialized = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    let entry = Entry::new(KEYCHAIN_SERVICE, account)
        .map_err(|error| format!("Could not open official token storage: {error}"))?;
    if keychain_value_too_large(&serialized) {
        return write_file_backed_keychain_value(&entry, account, &serialized);
    }
    match entry.set_password(&serialized) {
        Ok(()) => {
            let _ = fs::remove_file(token_file_path(account));
            Ok(())
        }
        Err(error) if keychain_size_error(&error) => {
            write_file_backed_keychain_value(&entry, account, &serialized)
        }
        Err(error) => Err(format!("Could not write official token storage: {error}")),
    }
}

fn delete_keychain_value(account: &str) -> Result<(), String> {
    let entry = Entry::new(KEYCHAIN_SERVICE, account)
        .map_err(|error| format!("Could not open official token storage: {error}"))?;
    if let Ok(value) = entry.get_password() {
        if value.starts_with(TOKEN_FILE_REF_PREFIX) {
            let _ = fs::remove_file(token_file_path(account));
        }
    }
    let _ = fs::remove_file(token_file_path(account));
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("Could not delete official token storage: {error}")),
    }
}

fn keychain_value_too_large(value: &str) -> bool {
    value.encode_utf16().count() > DIRECT_KEYCHAIN_UTF16_LIMIT
}

fn keychain_size_error(error: &keyring::Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("platform limit") || text.contains("too long") || text.contains("longer than")
}

fn write_file_backed_keychain_value(
    entry: &Entry,
    account: &str,
    serialized: &str,
) -> Result<(), String> {
    write_token_file(account, serialized)?;
    let marker = token_file_marker(account);
    if let Err(error) = entry.set_password(&marker) {
        let _ = fs::remove_file(token_file_path(account));
        return Err(format!("Could not write official token storage: {error}"));
    }
    Ok(())
}

fn read_stored_token_value(account: &str, stored: &str) -> Result<String, String> {
    read_stored_token_value_from_dir(account, stored, &official_token_dir())
}

fn read_stored_token_value_from_dir(
    account: &str,
    stored: &str,
    dir: &Path,
) -> Result<String, String> {
    let Some(name) = stored.strip_prefix(TOKEN_FILE_REF_PREFIX) else {
        return Ok(stored.to_string());
    };
    if name != token_file_name(account) {
        return Err("Official token storage reference is invalid".into());
    }
    fs::read_to_string(token_file_path_in_dir(dir, account))
        .map_err(|error| format!("Could not read official token file: {error}"))
}

fn write_token_file(account: &str, serialized: &str) -> Result<(), String> {
    write_token_file_in_dir(account, serialized, &official_token_dir())
}

fn write_token_file_in_dir(account: &str, serialized: &str, dir: &Path) -> Result<(), String> {
    let path = token_file_path_in_dir(dir, account);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create official token directory: {error}"))?;
    }
    let mut file = open_token_file(&path)?;
    file.write_all(serialized.as_bytes())
        .map_err(|error| format!("Could not write official token file: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("Could not finish official token file: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("Could not sync official token file: {error}"))
}

#[cfg(unix)]
fn open_token_file(path: &Path) -> Result<fs::File, String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("Could not open official token file: {error}"))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("Could not protect official token file: {error}"))?;
    Ok(file)
}

#[cfg(not(unix))]
fn open_token_file(path: &Path) -> Result<fs::File, String> {
    fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("Could not open official token file: {error}"))
}

fn token_file_marker(account: &str) -> String {
    format!("{TOKEN_FILE_REF_PREFIX}{}", token_file_name(account))
}

fn token_file_path(account: &str) -> PathBuf {
    token_file_path_in_dir(&official_token_dir(), account)
}

fn token_file_path_in_dir(dir: &Path, account: &str) -> PathBuf {
    dir.join(token_file_name(account))
}

fn token_file_name(account: &str) -> String {
    format!("{:x}.json", Sha1::digest(account.as_bytes()))
}

fn official_token_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("dev.neko.route")
        .join("official-tokens")
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_string()
    } else {
        format!("{}...", &value[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::{
        claude_oauth_code_challenge, claude_organization_uuid_from_value,
        exchange_claude_cookie_session_key_at, extract_claude_oauth_code,
        extract_openai_oauth_code, keychain_value_too_large, merge_token_values,
        openai_chatgpt_account_id, openai_oauth_code_challenge, openai_token_from_codex_auth_file,
        parse_token_value, read_stored_token_value_from_dir, start_claude_oauth_session,
        start_openai_oauth_session, token_file_marker, write_token_file_in_dir,
    };
    use axum::{http::HeaderMap, routing::get, routing::post, Json, Router};
    use base64::Engine as _;
    use chrono::Utc;
    use serde_json::json;
    use std::fs;
    use tokio::net::TcpListener;

    #[test]
    fn parses_nested_token_json() {
        let token = parse_token_value(&json!({
            "oauth": {
                "accessToken": "access-1",
                "refreshToken": "refresh-1",
                "expiresAt": "2099-01-01T00:00:00Z"
            }
        }))
        .unwrap();
        assert_eq!(token.access_token, "access-1");
        assert_eq!(token.refresh_token.as_deref(), Some("refresh-1"));
        assert!(token.expires_at.unwrap() > Utc::now());
    }

    #[test]
    fn supports_expires_in() {
        let token = parse_token_value(&json!({
            "access_token": "access-1",
            "expires_in": 3600
        }))
        .unwrap();
        assert!(token.expires_at.unwrap() > Utc::now());
    }

    #[test]
    fn openai_oauth_url_contains_codex_pkce_parameters() {
        let session = start_openai_oauth_session();
        let url = url::Url::parse(&session.auth_url).unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(
            url.as_str().split('?').next().unwrap(),
            "https://auth.openai.com/oauth/authorize"
        );
        assert_eq!(
            query.get("client_id").map(|value| value.as_ref()),
            Some("app_EMoamEEZ73f0CkXaXp7hrann")
        );
        assert_eq!(
            query.get("redirect_uri").map(|value| value.as_ref()),
            Some("http://localhost:1455/auth/callback")
        );
        assert_eq!(
            query.get("scope").map(|value| value.as_ref()),
            Some("openid profile email offline_access")
        );
        assert_eq!(
            query.get("state").map(|value| value.as_ref()),
            Some(session.state.as_str())
        );
        assert_eq!(
            query
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
        assert_eq!(
            query
                .get("id_token_add_organizations")
                .map(|value| value.as_ref()),
            Some("true")
        );
        assert_eq!(
            query
                .get("codex_cli_simplified_flow")
                .map(|value| value.as_ref()),
            Some("true")
        );
        assert_eq!(
            query.get("code_challenge").map(|value| value.as_ref()),
            Some(openai_oauth_code_challenge(&session.code_verifier).as_str())
        );
    }

    #[test]
    fn extracts_openai_oauth_code_from_callback_or_raw_code() {
        let code = extract_openai_oauth_code(
            "http://localhost:1455/auth/callback?code=abc123&state=state-1",
            "state-1",
        )
        .unwrap();
        assert_eq!(code, "abc123");

        let code = extract_openai_oauth_code("raw-code", "state-1").unwrap();
        assert_eq!(code, "raw-code");
    }

    #[test]
    fn rejects_mismatched_openai_oauth_state() {
        let error = extract_openai_oauth_code(
            "http://localhost:1455/auth/callback?code=abc123&state=bad",
            "state-1",
        )
        .unwrap_err();

        assert!(error.contains("state"));
    }

    #[test]
    fn claude_oauth_url_contains_pkce_parameters() {
        let session = start_claude_oauth_session();
        let url = url::Url::parse(&session.auth_url).unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(
            url.as_str().split('?').next().unwrap(),
            "https://claude.ai/oauth/authorize"
        );
        assert_eq!(
            query.get("client_id").map(|value| value.as_ref()),
            Some("9d1c250a-e61b-44d9-88ed-5944d1962f5e")
        );
        assert_eq!(
            query.get("redirect_uri").map(|value| value.as_ref()),
            Some("https://platform.claude.com/oauth/code/callback")
        );
        assert_eq!(
            query.get("scope").map(|value| value.as_ref()),
            Some("org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload")
        );
        assert_eq!(
            query.get("state").map(|value| value.as_ref()),
            Some(session.state.as_str())
        );
        assert_eq!(
            query
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
        assert_eq!(
            query.get("code_challenge").map(|value| value.as_ref()),
            Some(claude_oauth_code_challenge(&session.code_verifier).as_str())
        );
    }

    #[test]
    fn extracts_claude_oauth_code_from_callback_raw_and_code_state() {
        let code = extract_claude_oauth_code(
            "https://platform.claude.com/oauth/code/callback?code=abc123&state=state-1",
            "state-1",
        )
        .unwrap();
        assert_eq!(code, "abc123#state-1");

        let code = extract_claude_oauth_code("raw-code", "state-1").unwrap();
        assert_eq!(code, "raw-code");

        let code = extract_claude_oauth_code("abc123#state-1", "state-1").unwrap();
        assert_eq!(code, "abc123#state-1");
    }

    #[test]
    fn rejects_mismatched_claude_oauth_state() {
        let error = extract_claude_oauth_code(
            "https://platform.claude.com/oauth/code/callback?code=abc123&state=bad",
            "state-1",
        )
        .unwrap_err();

        assert!(error.contains("state"));
    }

    #[test]
    fn selects_claude_team_organization_first() {
        let uuid = claude_organization_uuid_from_value(&json!([
            { "uuid": "personal-org", "name": "Personal" },
            { "uuid": "team-org", "name": "Team", "raven_type": "team" }
        ]))
        .unwrap();
        assert_eq!(uuid, "team-org");

        let uuid = claude_organization_uuid_from_value(&json!([
            { "uuid": "first-org", "name": "First" },
            { "uuid": "second-org", "name": "Second" }
        ]))
        .unwrap();
        assert_eq!(uuid, "first-org");
    }

    #[tokio::test]
    async fn cookie_claude_oauth_exchanges_session_key_with_mock_server() {
        let router = Router::new()
            .route(
                "/api/organizations",
                get(|headers: HeaderMap| async move {
                    assert_eq!(
                        headers.get("cookie").and_then(|value| value.to_str().ok()),
                        Some("sessionKey=session-1")
                    );
                    Json(json!([
                        { "uuid": "personal-org", "name": "Personal" },
                        { "uuid": "team-org", "name": "Team", "raven_type": "team" }
                    ]))
                }),
            )
            .route(
                "/v1/oauth/team-org/authorize",
                post(|headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                    assert_eq!(
                        headers.get("cookie").and_then(|value| value.to_str().ok()),
                        Some("sessionKey=session-1")
                    );
                    assert_eq!(
                        body["client_id"],
                        "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
                    );
                    assert_eq!(body["organization_uuid"], "team-org");
                    assert_eq!(
                        body["scope"],
                        "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"
                    );
                    let state = body["state"].as_str().unwrap();
                    Json(json!({
                        "redirect_uri": format!(
                            "https://platform.claude.com/oauth/code/callback?code=AUTH-CODE&state={state}"
                        )
                    }))
                }),
            )
            .route(
                "/token",
                post(|Json(body): Json<serde_json::Value>| async move {
                    assert_eq!(body["code"], "AUTH-CODE");
                    assert_eq!(
                        body["client_id"],
                        "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
                    );
                    assert_eq!(
                        body["redirect_uri"],
                        "https://platform.claude.com/oauth/code/callback"
                    );
                    assert!(body["state"].as_str().is_some_and(|value| !value.is_empty()));
                    Json(json!({
                        "access_token": "access-1",
                        "refresh_token": "refresh-1",
                        "token_type": "Bearer",
                        "expires_in": 3600,
                        "organization": { "uuid": "team-org" },
                        "account": { "uuid": "account-1", "email_address": "zoe@example.com" }
                    }))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let token_url = format!("{base_url}/token");
        let client = reqwest::Client::new();

        let value =
            exchange_claude_cookie_session_key_at(&client, &base_url, &token_url, "session-1")
                .await
                .unwrap();

        assert_eq!(value["access_token"], "access-1");
        assert_eq!(value["refresh_token"], "refresh-1");
        assert_eq!(value["client_id"], "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
        assert_eq!(value["token_url"], token_url);
        assert_eq!(value["org_uuid"], "team-org");
        assert_eq!(value["account_uuid"], "account-1");
        assert_eq!(value["email_address"], "zoe@example.com");
        assert!(value["expires_at"].as_str().is_some());
    }

    #[test]
    fn merge_refresh_keeps_original_metadata() {
        let merged = merge_token_values(
            json!({ "client_id": "client-1", "refresh_token": "old-refresh" }),
            json!({ "access_token": "new-access", "expires_in": 3600 }),
        );
        assert_eq!(merged["client_id"], "client-1");
        assert_eq!(merged["access_token"], "new-access");
    }

    #[test]
    fn detects_values_too_large_for_windows_keychain() {
        let long = "x".repeat(2601);
        assert!(keychain_value_too_large(&long));
        assert!(!keychain_value_too_large("short-token-json"));
    }

    #[test]
    fn reads_file_backed_official_token_reference() {
        let dir = tempfile::tempdir().unwrap();
        let account = "official-token:openai-account-main";
        let token_json = serde_json::to_string_pretty(&json!({
            "access_token": "access",
            "refresh_token": "refresh",
            "chatgpt_account_id": "acct_123"
        }))
        .unwrap();

        write_token_file_in_dir(account, &token_json, dir.path()).unwrap();
        let read =
            read_stored_token_value_from_dir(account, &token_file_marker(account), dir.path())
                .unwrap();

        assert_eq!(read.trim(), token_json);
    }

    #[test]
    fn rejects_mismatched_file_backed_official_token_reference() {
        let dir = tempfile::tempdir().unwrap();
        let error = read_stored_token_value_from_dir(
            "official-token:openai-account-main",
            &token_file_marker("official-token:other"),
            dir.path(),
        )
        .unwrap_err();

        assert_eq!(error, "Official token storage reference is invalid");
    }

    #[test]
    fn extracts_openai_account_id_from_explicit_field() {
        let value = json!({ "access_token": "access", "chatgpt_account_id": "acct_123" });
        assert_eq!(
            openai_chatgpt_account_id(&value).as_deref(),
            Some("acct_123")
        );
    }

    #[test]
    fn extracts_openai_account_id_from_id_token_claim() {
        let claims = r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_jwt"}}"#;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims);
        let value = json!({ "access_token": "access", "id_token": format!("x.{payload}.y") });
        assert_eq!(
            openai_chatgpt_account_id(&value).as_deref(),
            Some("acct_jwt")
        );
    }

    #[tokio::test]
    async fn reads_codex_auth_json_without_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(
            &path,
            serde_json::to_string(&json!({
                "access_token": "codex-access",
                "chatgpt_account_id": "acct_codex"
            }))
            .unwrap(),
        )
        .unwrap();

        let (_, token, account_id) =
            openai_token_from_codex_auth_file(&reqwest::Client::new(), &path)
                .await
                .unwrap();

        assert_eq!(token.access_token, "codex-access");
        assert_eq!(account_id, "acct_codex");
    }
}
