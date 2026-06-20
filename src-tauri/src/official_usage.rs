use crate::{
    official_auth,
    types::{OpenAiAccountQuota, OpenAiQuotaWindow, Provider},
};
use chrono::{TimeZone, Utc};
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

pub async fn fetch_openai_quota(
    client: &Client,
    provider: &Provider,
) -> Result<OpenAiAccountQuota, String> {
    let (token_value, token, account_id) =
        official_auth::openai_token_for_provider(client, provider).await?;
    fetch_openai_quota_with_token(client, &token_value, &token.access_token, &account_id).await
}

pub async fn fetch_codex_openai_quota(client: &Client) -> Result<OpenAiAccountQuota, String> {
    let (token_value, token, account_id) = official_auth::openai_token_for_codex(client).await?;
    fetch_openai_quota_with_token(client, &token_value, &token.access_token, &account_id).await
}

async fn fetch_openai_quota_with_token(
    client: &Client,
    token_value: &Value,
    access_token: &str,
    account_id: &str,
) -> Result<OpenAiAccountQuota, String> {
    let mut request = client
        .get(official_auth::openai_codex_usage_url())
        .timeout(Duration::from_secs(20));
    for (name, value) in official_auth::openai_codex_headers(access_token, account_id) {
        let accept = if name.eq_ignore_ascii_case("accept") {
            "application/json".to_string()
        } else {
            value
        };
        request = request.header(name, accept);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("Could not query OpenAI account quota: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Could not read OpenAI quota response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "OpenAI quota returned {}: {}",
            status.as_u16(),
            truncate(&text, 240)
        ));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("OpenAI quota returned invalid JSON: {error}"))?;
    let mut quota = quota_from_wham_usage(&value);
    if quota.account_id.is_none() {
        quota.account_id = Some(account_id.to_string());
    }
    if quota.email.is_none() {
        quota.email = official_auth::openai_token_email(token_value);
    }
    if quota.plan_type.is_none() {
        quota.plan_type = official_auth::openai_token_plan_type(token_value);
    }
    Ok(quota)
}

pub fn quota_from_wham_usage(value: &Value) -> OpenAiAccountQuota {
    let rate_limit = value.get("rate_limit");
    let mut quota = OpenAiAccountQuota {
        account_id: string(value, "account_id"),
        user_id: string(value, "user_id"),
        email: string(value, "email"),
        plan_type: string(value, "plan_type"),
        reset_credits: value
            .get("rate_limit_reset_credits")
            .and_then(|credits| credits.get("available_count"))
            .and_then(number_u64),
        ..OpenAiAccountQuota::default()
    };
    let primary = rate_limit.and_then(|rl| rl.get("primary_window"));
    let secondary = rate_limit.and_then(|rl| rl.get("secondary_window"));
    assign_window(&mut quota, primary);
    assign_window(&mut quota, secondary);

    if let Some(items) = value
        .get("additional_rate_limits")
        .and_then(Value::as_array)
    {
        for item in items {
            let rate_limit = item.get("rate_limit");
            assign_window(
                &mut quota,
                rate_limit.and_then(|rl| rl.get("primary_window")),
            );
            assign_window(
                &mut quota,
                rate_limit.and_then(|rl| rl.get("secondary_window")),
            );
        }
    }
    quota
}

pub fn quota_from_codex_headers(headers: &HeaderMap) -> Option<OpenAiAccountQuota> {
    let primary = header_window(headers, "primary");
    let secondary = header_window(headers, "secondary");
    if primary.is_none() && secondary.is_none() {
        return None;
    }

    let mut quota = OpenAiAccountQuota::default();
    if let Some(window) = primary.as_ref() {
        assign_parsed_window(&mut quota, window.clone());
    }
    if let Some(window) = secondary.as_ref() {
        assign_parsed_window(&mut quota, window.clone());
    }
    Some(quota)
}

fn assign_window(quota: &mut OpenAiAccountQuota, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    if value.is_null() {
        return;
    }
    let Some(window) = parse_window(value) else {
        return;
    };
    assign_parsed_window(quota, window);
}

fn assign_parsed_window(quota: &mut OpenAiAccountQuota, window: OpenAiQuotaWindow) {
    let minutes = window.limit_window_seconds / 60;
    if minutes <= 360 {
        if quota.five_hour.is_none() {
            quota.five_hour = Some(window);
        }
    } else if quota.seven_day.is_none() {
        quota.seven_day = Some(window);
    }
}

fn parse_window(value: &Value) -> Option<OpenAiQuotaWindow> {
    let used_percent = value.get("used_percent").and_then(number_f64)?;
    let limit_window_seconds = value
        .get("limit_window_seconds")
        .and_then(number_u64)
        .unwrap_or(0);
    let reset_after_seconds = value
        .get("reset_after_seconds")
        .and_then(number_u64)
        .unwrap_or(0);
    let reset_at = value
        .get("reset_at")
        .and_then(number_i64)
        .and_then(|value| Utc.timestamp_opt(value, 0).single());
    Some(OpenAiQuotaWindow {
        used_percent,
        limit_window_seconds,
        reset_after_seconds,
        reset_at,
    })
}

fn header_window(headers: &HeaderMap, name: &str) -> Option<OpenAiQuotaWindow> {
    let used_percent = header_f64(headers, &format!("x-codex-{name}-used-percent"))?;
    let reset_after_seconds =
        header_u64(headers, &format!("x-codex-{name}-reset-after-seconds")).unwrap_or(0);
    let window_minutes =
        header_u64(headers, &format!("x-codex-{name}-window-minutes")).unwrap_or(0);
    let reset_at = if reset_after_seconds > 0 {
        Some(Utc::now() + chrono::Duration::seconds(reset_after_seconds as i64))
    } else {
        None
    };
    Some(OpenAiQuotaWindow {
        used_percent,
        limit_window_seconds: window_minutes.saturating_mul(60),
        reset_after_seconds,
        reset_at,
    })
}

fn header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<f64>().ok())
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn number_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
        .or_else(|| value.as_f64().map(|v| v.max(0.0) as u64))
}

fn number_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
        .or_else(|| value.as_f64().map(|v| v as i64))
}

fn number_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|v| v as f64))
        .or_else(|| value.as_u64().map(|v| v as f64))
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
    use super::{quota_from_codex_headers, quota_from_wham_usage};
    use reqwest::header::{HeaderMap, HeaderValue};
    use serde_json::json;

    #[test]
    fn parses_wham_usage_into_5h_and_7d() {
        let quota = quota_from_wham_usage(&json!({
            "account_id": "acct",
            "email": "a@example.com",
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 18.5,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 5000,
                    "reset_at": 1893456000
                },
                "secondary_window": {
                    "used_percent": 42.0,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 600,
                    "reset_at": 1893450000
                }
            },
            "rate_limit_reset_credits": { "available_count": 2 }
        }));

        assert_eq!(quota.account_id.as_deref(), Some("acct"));
        assert_eq!(quota.seven_day.unwrap().used_percent, 18.5);
        assert_eq!(quota.five_hour.unwrap().used_percent, 42.0);
        assert_eq!(quota.reset_credits, Some(2));
    }

    #[test]
    fn parses_codex_response_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-codex-primary-used-percent",
            HeaderValue::from_static("12"),
        );
        headers.insert(
            "x-codex-primary-reset-after-seconds",
            HeaderValue::from_static("120"),
        );
        headers.insert(
            "x-codex-primary-window-minutes",
            HeaderValue::from_static("300"),
        );
        headers.insert(
            "x-codex-secondary-used-percent",
            HeaderValue::from_static("34"),
        );
        headers.insert(
            "x-codex-secondary-window-minutes",
            HeaderValue::from_static("10080"),
        );

        let quota = quota_from_codex_headers(&headers).unwrap();
        assert_eq!(quota.five_hour.unwrap().used_percent, 12.0);
        assert_eq!(quota.seven_day.unwrap().used_percent, 34.0);
    }
}
