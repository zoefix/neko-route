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
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const KEYCHAIN_SERVICE: &str = "Neko Route Official Tokens";
const TOKEN_FILE_REF_PREFIX: &str = "file:v1:";
const DIRECT_KEYCHAIN_UTF16_LIMIT: usize = 2400;
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_REFRESH_SCOPE: &str = "openid profile email";
const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const OPENAI_CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REFRESH_SKEW_SECONDS: i64 = 300;
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
    let token = parse_token_value(&value)?;
    let normalized = normalized_token_value(value, &token);
    write_keychain_value(&token_ref(provider_id), &normalized)
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

pub async fn openai_token_for_codex(
    client: &Client,
) -> Result<(Value, ParsedToken, String), String> {
    let auth_path = codex_config::resolve_codex_home().join("auth.json");
    openai_token_from_codex_auth_file(client, &auth_path).await
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
        if let Some(client_id) = find_string(&original, &["client_id", "clientId"]) {
            payload.insert("client_id".into(), Value::String(client_id));
        }
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

#[cfg(test)]
mod tests {
    use super::{
        keychain_value_too_large, merge_token_values, openai_chatgpt_account_id,
        openai_token_from_codex_auth_file, parse_token_value, read_stored_token_value_from_dir,
        token_file_marker, write_token_file_in_dir,
    };
    use base64::Engine as _;
    use chrono::Utc;
    use serde_json::json;
    use std::fs;

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
