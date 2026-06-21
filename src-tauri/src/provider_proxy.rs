use crate::{
    key_store::KeyStore,
    types::{Provider, ProviderHttpProxy},
};
use reqwest::Client;
use url::Url;

const CLIENT_USER_AGENT: &str = "NekoRoute/0.1";
const PROXY_PASSWORD_PREFIX: &str = "provider-proxy:";

pub fn proxy_password_ref(provider_id: &str) -> String {
    format!("{PROXY_PASSWORD_PREFIX}{provider_id}")
}

pub fn normalize_provider_http_proxy(
    provider_id: &str,
    mut proxy: ProviderHttpProxy,
) -> ProviderHttpProxy {
    proxy.url = normalize_proxy_url_lossy(&proxy.url);
    proxy.username = proxy.username.trim().to_string();
    proxy.password_ref = proxy
        .password_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|_| proxy_password_ref(provider_id));
    if proxy.url.is_empty() {
        proxy.enabled = false;
        proxy.username.clear();
        proxy.password_ref = None;
    }
    proxy
}

pub fn normalize_proxy_url(raw: &str) -> Result<String, String> {
    let mut input = raw.trim().to_string();
    if input.is_empty() {
        return Ok(String::new());
    }
    if !input.contains("://") {
        input = format!("http://{input}");
    }
    let mut url = Url::parse(&input).map_err(|_| "HTTP proxy address is invalid".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("HTTP proxy address must start with http:// or https://".into());
    }
    if url.host_str().is_none() {
        return Err("HTTP proxy address is missing a host".into());
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    if url.path() == "/" {
        url.set_path("");
    }
    Ok(url.to_string().trim_end_matches('/').to_string())
}

pub fn split_proxy_url_credentials(
    raw: &str,
) -> Result<(String, Option<String>, Option<String>), String> {
    let mut input = raw.trim().to_string();
    if input.is_empty() {
        return Ok((String::new(), None, None));
    }
    if !input.contains("://") {
        input = format!("http://{input}");
    }
    let mut url = Url::parse(&input).map_err(|_| "HTTP proxy address is invalid".to_string())?;
    let username = if url.username().is_empty() {
        None
    } else {
        Some(url.username().to_string())
    };
    let password = url.password().map(str::to_string);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Ok((normalize_proxy_url(url.as_str())?, username, password))
}

pub fn normalize_proxy_url_lossy(raw: &str) -> String {
    normalize_proxy_url(raw).unwrap_or_else(|_| raw.trim().to_string())
}

pub fn validate_provider_http_proxy(provider: &Provider) -> Result<(), String> {
    if !provider.http_proxy.enabled {
        return Ok(());
    }
    if provider.http_proxy.url.trim().is_empty() {
        return Err(format!(
            "Provider '{}' HTTP proxy address is required when proxy is enabled",
            provider.name
        ));
    }
    normalize_proxy_url(&provider.http_proxy.url)
        .map_err(|error| format!("Provider '{}' {error}", provider.name))?;
    Ok(())
}

pub fn client_for_provider(
    default_client: &Client,
    key_store: &KeyStore,
    provider: &Provider,
) -> Result<Client, String> {
    if !provider.http_proxy.enabled {
        return Ok(default_client.clone());
    }
    let proxy_url = normalize_proxy_url(&provider.http_proxy.url)
        .map_err(|error| format!("Provider '{}' {error}", provider.name))?;
    let mut proxy = reqwest::Proxy::all(&proxy_url).map_err(|error| {
        format!(
            "Provider '{}' HTTP proxy '{}' is invalid: {error}",
            provider.name, proxy_url
        )
    })?;
    let username = provider.http_proxy.username.trim();
    if !username.is_empty() {
        let password = provider
            .http_proxy
            .password_ref
            .as_deref()
            .map(|key_ref| key_store.get_secret(key_ref))
            .transpose()?
            .flatten()
            .unwrap_or_default();
        proxy = proxy.basic_auth(username, &password);
    }
    Client::builder()
        .user_agent(CLIENT_USER_AGENT)
        .proxy(proxy)
        .build()
        .map_err(|error| {
            format!(
                "Provider '{}' HTTP proxy '{}' could not be initialized: {error}",
                provider.name, proxy_url
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        client_for_provider, normalize_provider_http_proxy, normalize_proxy_url,
        split_proxy_url_credentials,
    };
    use crate::{key_store::KeyStore, types::default_config};

    #[test]
    fn normalizes_proxy_url_without_scheme() {
        assert_eq!(
            normalize_proxy_url("127.0.0.1:7890").unwrap(),
            "http://127.0.0.1:7890"
        );
    }

    #[test]
    fn rejects_non_http_proxy_scheme() {
        let error = normalize_proxy_url("socks5://127.0.0.1:7890").unwrap_err();
        assert!(error.contains("http:// or https://"));
    }

    #[test]
    fn splits_credentials_out_of_proxy_url() {
        let (url, username, password) =
            split_proxy_url_credentials("http://user:pass@127.0.0.1:7890").unwrap();
        assert_eq!(url, "http://127.0.0.1:7890");
        assert_eq!(username.as_deref(), Some("user"));
        assert_eq!(password.as_deref(), Some("pass"));
    }

    #[test]
    fn builds_provider_clients_for_official_and_custom_providers() {
        let temp = tempfile::tempdir().unwrap();
        let key_store = KeyStore::new(temp.path());
        let default_client = reqwest::Client::new();
        let mut config = default_config();
        for provider in &mut config.providers {
            provider.http_proxy = normalize_provider_http_proxy(
                &provider.id,
                crate::types::ProviderHttpProxy {
                    enabled: true,
                    url: "127.0.0.1:7890".into(),
                    username: String::new(),
                    password_ref: None,
                },
            );
            client_for_provider(&default_client, &key_store, provider).unwrap();
        }
    }
}
