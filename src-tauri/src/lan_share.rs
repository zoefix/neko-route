use crate::{
    catalog::CatalogModel,
    types::{default_lan_api_key, ProviderProtocol, Settings},
};
use reqwest::Client;
use serde_json::Value;
use std::{collections::HashSet, net::IpAddr, time::Duration};

const DEFAULT_REMOTE_CONTEXT_WINDOW: u64 = 128_000;
pub const LAN_REMOTE_MODELS_TIMEOUT_SECS: u64 = 5;

pub fn generate_api_key() -> String {
    default_lan_api_key()
}

pub fn remote_base_url(settings: &Settings) -> Result<String, String> {
    let host = settings.lan_remote_host.trim();
    if host.is_empty() {
        return Err("LAN host is required".into());
    }
    if host.contains("://") {
        return Err("LAN host must be an IP address without http:// or https://".into());
    }
    host.parse::<IpAddr>()
        .map_err(|_| "LAN host must be an IP address".to_string())?;
    if settings.lan_remote_port == 0 {
        return Err("LAN port is required".into());
    }
    Ok(format!("http://{}:{}/v1", host, settings.lan_remote_port))
}

pub fn bearer_value(api_key: &str) -> Result<String, String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("LAN API key is required".into());
    }
    Ok(format!("Bearer {api_key}"))
}

pub fn remote_models_timeout() -> Duration {
    Duration::from_secs(LAN_REMOTE_MODELS_TIMEOUT_SECS)
}

pub async fn fetch_remote_catalog_models(
    client: &Client,
    settings: &Settings,
) -> Result<Vec<CatalogModel>, String> {
    let url = format!("{}/models", remote_base_url(settings)?);
    let response = client
        .get(&url)
        .header("authorization", bearer_value(&settings.lan_remote_api_key)?)
        .timeout(remote_models_timeout())
        .send()
        .await
        .map_err(|error| format!("Could not reach LAN host: {error}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("Could not read LAN models response: {error}"))?;
    if !status.is_success() {
        let message = String::from_utf8_lossy(&body);
        return Err(format!(
            "LAN host returned {} while loading models: {}",
            status.as_u16(),
            message
        ));
    }
    let value = serde_json::from_slice::<Value>(&body)
        .map_err(|error| format!("Invalid LAN models response: {error}"))?;
    parse_models_response(&value)
}

pub fn parse_models_response(value: &Value) -> Result<Vec<CatalogModel>, String> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "LAN models response is missing data[]".to_string())?;
    let mut seen = HashSet::new();
    let mut models = Vec::new();

    for item in data {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "LAN model entry is missing id".to_string())?;
        if !seen.insert(id.to_string()) {
            return Err(format!("LAN host returned duplicate model id '{id}'"));
        }
        models.push(CatalogModel {
            slug: id.to_string(),
            display_name: string_field(item, "display_name").unwrap_or_else(|| id.to_string()),
            description: string_field(item, "description")
                .or_else(|| string_field(item, "owned_by"))
                .unwrap_or_else(|| "LAN shared model".into()),
            context_window: u64_field(item, "context_window")
                .or_else(|| u64_field(item, "max_context_window"))
                .unwrap_or(DEFAULT_REMOTE_CONTEXT_WINDOW),
            reasoning_enabled: item
                .get("supports_reasoning_summaries")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            default_reasoning_level: string_field(item, "default_reasoning_level")
                .unwrap_or_else(|| "medium".into()),
            supported_reasoning_levels: reasoning_levels(item),
            provider_protocol: string_field(item, "provider_protocol")
                .as_deref()
                .and_then(provider_protocol_from_str),
        });
    }

    if models.is_empty() {
        return Err("LAN host did not return any available models".into());
    }
    Ok(models)
}

fn provider_protocol_from_str(value: &str) -> Option<ProviderProtocol> {
    match value {
        "open_ai_responses" => Some(ProviderProtocol::OpenAiResponses),
        "open_ai_chat_completions" => Some(ProviderProtocol::OpenAiChatCompletions),
        "anthropic_messages" => Some(ProviderProtocol::AnthropicMessages),
        _ => None,
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
}

fn reasoning_levels(value: &Value) -> Vec<String> {
    value
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| {
                    level
                        .as_str()
                        .or_else(|| level.get("effort").and_then(Value::as_str))
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{parse_models_response, remote_models_timeout, LAN_REMOTE_MODELS_TIMEOUT_SECS};
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn parses_neko_route_model_metadata() {
        let models = parse_models_response(&json!({
            "object": "list",
            "data": [{
                "id": "gpt-lan",
                "display_name": "GPT LAN",
                "description": "Remote",
                "context_window": 1_000_000,
                "supports_reasoning_summaries": true,
                "default_reasoning_level": "high",
                "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}]
            }]
        }))
        .unwrap();

        assert_eq!(models[0].slug, "gpt-lan");
        assert_eq!(models[0].display_name, "GPT LAN");
        assert_eq!(models[0].context_window, 1_000_000);
        assert!(models[0].reasoning_enabled);
        assert_eq!(models[0].supported_reasoning_levels, ["low", "high"]);
    }

    #[test]
    fn rejects_duplicate_lan_model_ids() {
        let error = parse_models_response(&json!({
            "data": [{"id": "same"}, {"id": "same"}]
        }))
        .unwrap_err();

        assert!(error.contains("duplicate"));
    }

    #[test]
    fn lan_model_fetch_uses_short_timeout() {
        assert_eq!(
            remote_models_timeout(),
            Duration::from_secs(LAN_REMOTE_MODELS_TIMEOUT_SECS)
        );
        assert!(remote_models_timeout() <= Duration::from_secs(5));
    }
}
