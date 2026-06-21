#[cfg(target_os = "macos")]
use aes::Aes128;
#[cfg(target_os = "macos")]
use base64::{engine::general_purpose::STANDARD, Engine};
#[cfg(target_os = "macos")]
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
#[cfg(target_os = "macos")]
use cbc::Decryptor;
#[cfg(target_os = "macos")]
use keyring::Entry;
#[cfg(target_os = "macos")]
use pbkdf2::pbkdf2_hmac;
use serde_json::{json, Value};
#[cfg(target_os = "macos")]
use sha1::Sha1;
use std::{env, fs, path::PathBuf};

const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
const OAUTH_BETA: &str = "oauth-2025-04-20,claude-code-20250219";

#[derive(Debug, Clone)]
pub struct ClaudeAuth {
    pub base_url: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct TokenRecord {
    access_token: String,
    source: &'static str,
    metadata: Option<Value>,
}

enum DesktopTokenLookup {
    Missing,
    Encrypted,
    Usable(TokenRecord),
}

pub fn cli_auth() -> Result<ClaudeAuth, String> {
    let token = discover_cli_token(true)?;
    Ok(ClaudeAuth {
        base_url: DEFAULT_ANTHROPIC_BASE_URL.into(),
        headers: vec![
            ("content-type".into(), "application/json".into()),
            (
                "authorization".into(),
                format!("Bearer {}", token.access_token),
            ),
            ("anthropic-beta".into(), OAUTH_BETA.into()),
        ],
    })
}

pub fn desktop_auth() -> Result<ClaudeAuth, String> {
    let token = discover_desktop_token()?;
    Ok(ClaudeAuth {
        base_url: DEFAULT_ANTHROPIC_BASE_URL.into(),
        headers: vec![
            ("content-type".into(), "application/json".into()),
            (
                "authorization".into(),
                format!("Bearer {}", token.access_token),
            ),
            ("anthropic-beta".into(), OAUTH_BETA.into()),
        ],
    })
}

pub fn cli_oauth_token_value() -> Result<(Value, String), String> {
    let token = discover_cli_token(true)?;
    Ok((token_record_value(&token), token.access_token))
}

pub fn desktop_oauth_token_value() -> Result<(Value, String), String> {
    let token = discover_desktop_token()?;
    Ok((token_record_value(&token), token.access_token))
}

pub fn cli_credential_json() -> Result<String, String> {
    credential_json(discover_cli_token(true)?)
}

pub fn desktop_credential_json() -> Result<String, String> {
    credential_json(discover_desktop_token()?)
}

pub fn cli_status() -> (bool, bool, Option<String>) {
    match discover_cli_token(false) {
        Ok(token) => (true, true, Some(format!("Using {}", token.source))),
        Err(message) => (false, true, Some(message)),
    }
}

fn credential_json(token: TokenRecord) -> Result<String, String> {
    serde_json::to_string_pretty(&token_record_value(&token)).map_err(|error| error.to_string())
}

fn token_record_value(token: &TokenRecord) -> Value {
    let mut value = json!({
        "access_token": token.access_token,
        "source": token.source,
    });
    if let (Value::Object(object), Some(metadata)) = (&mut value, &token.metadata) {
        object.insert("metadata".into(), metadata.clone());
    }
    value
}

pub fn desktop_status() -> (bool, bool, Option<String>) {
    desktop_status_from_lookup(token_from_desktop_config(false))
}

fn desktop_status_from_lookup(
    lookup: Result<DesktopTokenLookup, String>,
) -> (bool, bool, Option<String>) {
    match lookup {
        Ok(DesktopTokenLookup::Usable(token)) => {
            (true, true, Some(format!("Using {}", token.source)))
        }
        Ok(DesktopTokenLookup::Encrypted) => {
            (true, true, Some("key.claudeDesktopKeychainOnUse".into()))
        }
        Ok(DesktopTokenLookup::Missing) => (
            false,
            true,
            Some("Claude Desktop credentials were not found. Sign in with Claude Desktop.".into()),
        ),
        Err(message) => (false, true, Some(message)),
    }
}

fn discover_cli_token(allow_keychain: bool) -> Result<TokenRecord, String> {
    if let Some(token) = env_token("CLAUDE_CODE_OAUTH_TOKEN")
        .and_then(|token| token_record_from_secret(&token, "CLAUDE_CODE_OAUTH_TOKEN"))
    {
        return Ok(token);
    }

    if let Some(token) = env_token("ANTHROPIC_AUTH_TOKEN")
        .and_then(|token| token_record_from_secret(&token, "ANTHROPIC_AUTH_TOKEN"))
    {
        return Ok(token);
    }

    if let Some(token) = token_from_cli_credentials()? {
        return Ok(token);
    }

    if allow_keychain {
        if let Some(token) = token_from_cli_keychain()? {
            return Ok(token);
        }
    }

    if allow_keychain {
        Err("Claude Code CLI credentials were not found. Sign in with Claude Code CLI.".into())
    } else {
        Err("key.claudeCliKeychainOnUse".into())
    }
}

fn discover_desktop_token() -> Result<TokenRecord, String> {
    match token_from_desktop_config(true)? {
        DesktopTokenLookup::Usable(token) => Ok(token),
        DesktopTokenLookup::Encrypted => Err("Claude Desktop credentials are encrypted in Claude Safe Storage, but Neko Route could not unlock or decrypt them. Approve the system keychain prompt and try again.".into()),
        DesktopTokenLookup::Missing => {
            Err("Claude Desktop credentials were not found. Sign in with Claude Desktop.".into())
        }
    }
}

fn env_token(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn token_from_cli_credentials() -> Result<Option<TokenRecord>, String> {
    let path = claude_config_dir().join(".credentials.json");
    let Some(value) = read_json(&path)? else {
        return Ok(None);
    };
    Ok(find_access_token(&value).map(|access_token| TokenRecord {
        access_token,
        source: "Claude Code credentials",
        metadata: Some(value),
    }))
}

fn token_from_cli_keychain() -> Result<Option<TokenRecord>, String> {
    #[cfg(target_os = "macos")]
    {
        for (service, account) in [
            ("Claude Code", "oauth"),
            ("Claude Code", "credentials"),
            ("Claude Code-credentials", "Claude Code"),
            ("com.anthropic.claude-code", "oauth"),
        ] {
            let Ok(entry) = Entry::new(service, account) else {
                continue;
            };
            match entry.get_password() {
                Ok(secret) => {
                    if let Some(token) = token_record_from_secret(&secret, "Claude Code Keychain") {
                        return Ok(Some(token));
                    }
                }
                Err(keyring::Error::NoEntry) => {}
                Err(error) => {
                    return Err(format!(
                        "Could not read Claude Code Keychain credentials: {error}"
                    ));
                }
            }
        }
    }
    Ok(None)
}

fn token_from_desktop_config(allow_safe_storage: bool) -> Result<DesktopTokenLookup, String> {
    let mut found_encrypted_token_cache = false;
    for path in claude_desktop_config_paths() {
        let Some(value) = read_json(&path)? else {
            continue;
        };
        let Some(cache) = value
            .get("oauth:tokenCache")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };

        match token_from_desktop_cache(cache, allow_safe_storage)? {
            DesktopTokenLookup::Usable(mut token) => {
                token.source = if allow_safe_storage {
                    "Claude Desktop token cache"
                } else {
                    "Claude Desktop config"
                };
                return Ok(DesktopTokenLookup::Usable(token));
            }
            DesktopTokenLookup::Encrypted => {
                found_encrypted_token_cache = true;
            }
            DesktopTokenLookup::Missing => {}
        }
    }
    if found_encrypted_token_cache {
        Ok(DesktopTokenLookup::Encrypted)
    } else {
        Ok(DesktopTokenLookup::Missing)
    }
}

fn token_from_desktop_cache(
    cache: &str,
    allow_safe_storage: bool,
) -> Result<DesktopTokenLookup, String> {
    token_from_desktop_cache_with_decrypter(cache, allow_safe_storage, decrypt_desktop_token_cache)
}

fn token_from_desktop_cache_with_decrypter<F>(
    cache: &str,
    allow_safe_storage: bool,
    mut decrypt: F,
) -> Result<DesktopTokenLookup, String>
where
    F: FnMut(&str) -> Result<Option<String>, String>,
{
    if let Some(token) = token_record_from_secret(cache, "Claude Desktop config") {
        return Ok(DesktopTokenLookup::Usable(token));
    }

    if allow_safe_storage {
        if let Some(plain) = decrypt(cache)? {
            if let Some(token) = token_record_from_secret(&plain, "Claude Desktop token cache") {
                return Ok(DesktopTokenLookup::Usable(token));
            }
        }
    }

    if looks_like_chromium_encrypted_value(cache) {
        Ok(DesktopTokenLookup::Encrypted)
    } else {
        Ok(DesktopTokenLookup::Missing)
    }
}

fn token_record_from_secret(secret: &str, source: &'static str) -> Option<TokenRecord> {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            return find_access_token(&value).map(|access_token| TokenRecord {
                access_token,
                source,
                metadata: Some(value),
            });
        }
    }
    if looks_like_claude_oauth_access_token(trimmed) {
        return Some(TokenRecord {
            access_token: trimmed.to_string(),
            source,
            metadata: None,
        });
    }
    None
}

fn find_access_token(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            for key in [
                "accessToken",
                "access_token",
                "oauthToken",
                "oauth_token",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
            ] {
                if let Some(token) = object.get(key).and_then(Value::as_str) {
                    let token = token.trim();
                    if looks_like_claude_oauth_access_token(token) {
                        return Some(token.to_string());
                    }
                }
            }
            object.values().find_map(find_access_token)
        }
        Value::Array(items) => items.iter().find_map(find_access_token),
        Value::String(text) => {
            let text = text.trim();
            if looks_like_claude_oauth_access_token(text) {
                Some(text.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn looks_like_claude_oauth_access_token(value: &str) -> bool {
    value.starts_with("sk-ant-oat") && !value.contains(char::is_whitespace)
}

fn looks_like_chromium_encrypted_value(value: &str) -> bool {
    value.starts_with("v10") || value.starts_with("djEw") || value.starts_with("v11")
}

fn decrypt_desktop_token_cache(cache: &str) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        decrypt_macos_safe_storage(cache)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cache;
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
fn decrypt_macos_safe_storage(cache: &str) -> Result<Option<String>, String> {
    let ciphertext = if cache.as_bytes().starts_with(b"v10") || cache.as_bytes().starts_with(b"v11")
    {
        cache.as_bytes().to_vec()
    } else {
        let Ok(decoded) = STANDARD.decode(cache) else {
            return Ok(None);
        };
        decoded
    };
    if !(ciphertext.starts_with(b"v10") || ciphertext.starts_with(b"v11")) || ciphertext.len() <= 3
    {
        return Ok(None);
    }

    let entry = Entry::new("Claude Safe Storage", "Claude Key")
        .map_err(|error| format!("Could not open Claude Safe Storage: {error}"))?;
    let password = entry
        .get_password()
        .map_err(|error| format!("Claude Safe Storage keychain access failed: {error}"))?;

    let mut key = [0_u8; 16];
    pbkdf2_hmac::<Sha1>(password.as_bytes(), b"saltysalt", 1003, &mut key);
    let iv = [b' '; 16];
    let mut payload = ciphertext[3..].to_vec();
    let decrypted = Decryptor::<Aes128>::new(&key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut payload)
        .map_err(|_| "Failed to decrypt Claude Desktop token cache".to_string())?;
    String::from_utf8(decrypted.to_vec())
        .map(Some)
        .map_err(|error| error.to_string())
}

fn claude_config_dir() -> PathBuf {
    env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".claude")
        })
}

fn claude_desktop_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(config_dir) = dirs::config_dir() {
        #[cfg(target_os = "macos")]
        {
            paths.push(config_dir.join("Claude").join("config.json"));
            paths.push(config_dir.join("Claude").join("claude_desktop_config.json"));
            paths.push(config_dir.join("Claude-3p").join("config.json"));
            paths.push(
                config_dir
                    .join("Claude-3p")
                    .join("claude_desktop_config.json"),
            );
        }
        #[cfg(target_os = "windows")]
        {
            paths.push(config_dir.join("Claude").join("config.json"));
            paths.push(config_dir.join("Claude").join("claude_desktop_config.json"));
        }
        #[cfg(target_os = "linux")]
        {
            paths.push(config_dir.join("Claude").join("config.json"));
            paths.push(config_dir.join("Claude").join("claude_desktop_config.json"));
            paths.push(config_dir.join("claude").join("config.json"));
            paths.push(config_dir.join("claude").join("claude_desktop_config.json"));
        }
    }
    paths
}

fn read_json(path: &PathBuf) -> Result<Option<Value>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str::<Value>(&raw)
        .map(Some)
        .map_err(|error| format!("Failed to parse {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{
        desktop_status_from_lookup, find_access_token, looks_like_chromium_encrypted_value,
        token_from_desktop_cache_with_decrypter, token_record_from_secret, token_record_value,
        DesktopTokenLookup,
    };
    use serde_json::json;

    #[test]
    fn finds_nested_access_token() {
        let value = json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz"
            }
        });

        assert_eq!(
            find_access_token(&value).unwrap(),
            "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz"
        );
    }

    #[test]
    fn ignores_api_keys_for_official_auth() {
        assert!(token_record_from_secret("sk-ant-api03-test", "secret").is_none());
    }

    #[test]
    fn ignores_desktop_jwt_tokens_for_official_api_auth() {
        assert!(token_record_from_secret("eyJabcdefghijklmnopqrstuvwxyz", "secret").is_none());
    }

    #[test]
    fn preserves_token_metadata_for_quota_plan_parsing() {
        let token = token_record_from_secret(
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-token","account":{"subscriptionTier":"claude_max_20x"}}}"#,
            "Claude Desktop token cache",
        )
        .unwrap();
        let value = token_record_value(&token);

        assert_eq!(
            value
                .get("metadata")
                .and_then(|metadata| metadata.get("claudeAiOauth"))
                .and_then(|oauth| oauth.get("account"))
                .and_then(|account| account.get("subscriptionTier"))
                .and_then(serde_json::Value::as_str),
            Some("claude_max_20x")
        );
    }

    #[test]
    fn detects_chromium_encrypted_values_without_keychain_access() {
        assert!(looks_like_chromium_encrypted_value("v10encrypted"));
        assert!(looks_like_chromium_encrypted_value("djEwencoded"));
        assert!(!looks_like_chromium_encrypted_value(
            "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz"
        ));
    }

    #[test]
    fn desktop_status_lookup_does_not_decrypt_encrypted_cache() {
        let mut decrypt_calls = 0;

        let lookup = token_from_desktop_cache_with_decrypter("djEwencoded", false, |_| {
            decrypt_calls += 1;
            Ok(None)
        })
        .unwrap();

        assert!(matches!(lookup, DesktopTokenLookup::Encrypted));
        assert_eq!(decrypt_calls, 0);
    }

    #[test]
    fn encrypted_desktop_cache_counts_as_present_status() {
        let (present, available, message) =
            desktop_status_from_lookup(Ok(DesktopTokenLookup::Encrypted));

        assert!(present);
        assert!(available);
        assert_eq!(message.as_deref(), Some("key.claudeDesktopKeychainOnUse"));
    }

    #[test]
    fn desktop_auth_lookup_decrypts_encrypted_cache_on_use() {
        let mut decrypt_calls = 0;

        let lookup = token_from_desktop_cache_with_decrypter("djEwencoded", true, |_| {
            decrypt_calls += 1;
            Ok(Some(
                r#"{"accessToken":"sk-ant-oat01-decrypted-token"}"#.into(),
            ))
        })
        .unwrap();

        match lookup {
            DesktopTokenLookup::Usable(token) => {
                assert_eq!(token.access_token, "sk-ant-oat01-decrypted-token");
                assert_eq!(token.source, "Claude Desktop token cache");
                assert!(token.metadata.is_some());
            }
            _ => panic!("expected decrypted Claude Desktop token"),
        }
        assert_eq!(decrypt_calls, 1);
    }
}
