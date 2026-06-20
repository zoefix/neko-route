use crate::{
    claude_auth, official_auth,
    types::{OfficialAccountQuota, OfficialQuotaWindow, Provider},
};
use chrono::{DateTime, TimeZone, Utc};
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

const OPENAI_ACCOUNTS_CHECK_URL: &str =
    "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const OPENAI_SUBSCRIPTIONS_URL: &str = "https://chatgpt.com/backend-api/subscriptions";
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CLAUDE_USAGE_BETA: &str = "oauth-2025-04-20";
const CLAUDE_USAGE_USER_AGENT: &str = "claude-code/2.1.7";

#[derive(Debug, Clone, Default)]
pub struct ChatGptAccountInfo {
    pub plan_type: Option<String>,
    pub plan_label: Option<String>,
    pub email: Option<String>,
    pub subscription_expires_at: Option<DateTime<Utc>>,
}

pub async fn fetch_openai_quota(
    client: &Client,
    provider: &Provider,
) -> Result<OfficialAccountQuota, String> {
    let (token_value, token, account_id) =
        official_auth::openai_token_for_provider(client, provider).await?;
    fetch_openai_quota_with_token(client, &token_value, &token.access_token, &account_id).await
}

pub async fn fetch_codex_openai_quota(client: &Client) -> Result<OfficialAccountQuota, String> {
    let (token_value, token, account_id) = official_auth::openai_token_for_codex(client).await?;
    fetch_openai_quota_with_token(client, &token_value, &token.access_token, &account_id).await
}

pub async fn fetch_claude_quota(
    client: &Client,
    provider: &Provider,
) -> Result<OfficialAccountQuota, String> {
    let (token_value, token) =
        official_auth::anthropic_token_for_provider(client, provider).await?;
    fetch_claude_quota_with_token_at(client, CLAUDE_USAGE_URL, &token_value, &token.access_token)
        .await
}

pub async fn fetch_claude_cli_quota(client: &Client) -> Result<OfficialAccountQuota, String> {
    let (token_value, access_token) = claude_auth::cli_oauth_token_value()?;
    fetch_claude_quota_with_token_at(client, CLAUDE_USAGE_URL, &token_value, &access_token).await
}

pub async fn fetch_claude_desktop_quota(client: &Client) -> Result<OfficialAccountQuota, String> {
    let (token_value, access_token) = claude_auth::desktop_oauth_token_value()?;
    fetch_claude_quota_with_token_at(client, CLAUDE_USAGE_URL, &token_value, &access_token).await
}

async fn fetch_openai_quota_with_token(
    client: &Client,
    token_value: &Value,
    access_token: &str,
    account_id: &str,
) -> Result<OfficialAccountQuota, String> {
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
    enrich_openai_account_info(client, access_token, account_id, &mut quota).await;
    if quota.plan_label.is_none() {
        quota.plan_label = openai_plan_label(quota.plan_type.as_deref());
    }
    Ok(quota)
}

async fn fetch_claude_quota_with_token_at(
    client: &Client,
    usage_url: &str,
    token_value: &Value,
    access_token: &str,
) -> Result<OfficialAccountQuota, String> {
    let response = client
        .get(usage_url)
        .timeout(Duration::from_secs(20))
        .bearer_auth(access_token)
        .header("accept", "application/json, text/plain, */*")
        .header("content-type", "application/json")
        .header("anthropic-beta", CLAUDE_USAGE_BETA)
        .header("user-agent", CLAUDE_USAGE_USER_AGENT)
        .send()
        .await
        .map_err(|error| format!("Could not query Claude account quota: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Could not read Claude quota response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "Claude quota returned {}: {}",
            status.as_u16(),
            truncate(&text, 240)
        ));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("Claude quota returned invalid JSON: {error}"))?;
    Ok(quota_from_claude_usage(token_value, &value))
}

async fn enrich_openai_account_info(
    client: &Client,
    access_token: &str,
    account_id: &str,
    quota: &mut OfficialAccountQuota,
) {
    if let Some(info) = fetch_chatgpt_account_info(client, access_token, account_id).await {
        if quota.plan_type.is_none() {
            quota.plan_type = info.plan_type.clone();
        }
        if quota.plan_label.is_none() {
            quota.plan_label = info.plan_label;
        }
        if quota.email.is_none() {
            quota.email = info.email;
        }
        if quota.subscription_expires_at.is_none() {
            quota.subscription_expires_at = info.subscription_expires_at;
        }
    }
    if quota.subscription_expires_at.is_none() {
        quota.subscription_expires_at =
            fetch_chatgpt_subscription_expires_at(client, access_token, account_id).await;
    }
}

async fn fetch_chatgpt_account_info(
    client: &Client,
    access_token: &str,
    account_id: &str,
) -> Option<ChatGptAccountInfo> {
    let response = client
        .get(OPENAI_ACCOUNTS_CHECK_URL)
        .timeout(Duration::from_secs(15))
        .bearer_auth(access_token)
        .header("origin", "https://chatgpt.com")
        .header("referer", "https://chatgpt.com/")
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value = response.json::<Value>().await.ok()?;
    chatgpt_account_info_from_accounts_check(&value, account_id)
}

async fn fetch_chatgpt_subscription_expires_at(
    client: &Client,
    access_token: &str,
    account_id: &str,
) -> Option<DateTime<Utc>> {
    let response = client
        .get(OPENAI_SUBSCRIPTIONS_URL)
        .timeout(Duration::from_secs(15))
        .bearer_auth(access_token)
        .header("origin", "https://chatgpt.com")
        .header("referer", "https://chatgpt.com/")
        .header("accept", "application/json")
        .query(&[("account_id", account_id)])
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value = response.json::<Value>().await.ok()?;
    subscription_expires_at_from_value(&value)
}

pub fn chatgpt_account_info_from_accounts_check(
    value: &Value,
    account_id: &str,
) -> Option<ChatGptAccountInfo> {
    let accounts = value.get("accounts").and_then(Value::as_object)?;
    if let Some(account) = accounts.get(account_id).and_then(Value::as_object) {
        if let Some(info) = chatgpt_account_info_from_account(account) {
            return Some(info);
        }
    }

    let mut default = None;
    let mut paid = None;
    let mut any = None;
    for account in accounts.values().filter_map(Value::as_object) {
        let Some(info) = chatgpt_account_info_from_account(account) else {
            continue;
        };
        if any.is_none() {
            any = Some(info.clone());
        }
        if default.is_none()
            && account
                .get("account")
                .and_then(Value::as_object)
                .and_then(|account| account.get("is_default"))
                .and_then(Value::as_bool)
                == Some(true)
        {
            default = Some(info.clone());
        }
        if paid.is_none()
            && info
                .plan_type
                .as_deref()
                .is_some_and(|plan| !plan.eq_ignore_ascii_case("free"))
        {
            paid = Some(info);
        }
    }
    default.or(paid).or(any)
}

fn chatgpt_account_info_from_account(
    account: &serde_json::Map<String, Value>,
) -> Option<ChatGptAccountInfo> {
    let account_object = account.get("account").and_then(Value::as_object);
    let entitlement_object = account.get("entitlement").and_then(Value::as_object);
    let plan_type = account_object
        .and_then(|account| object_string(account, "plan_type"))
        .or_else(|| {
            entitlement_object
                .and_then(|entitlement| object_string(entitlement, "subscription_plan"))
        });
    let subscription_expires_at = entitlement_object
        .and_then(|entitlement| object_string(entitlement, "expires_at"))
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc));
    let email = account_object
        .and_then(|account| object_string(account, "email"))
        .or_else(|| object_string(account, "email"));
    let plan_label = openai_plan_label(plan_type.as_deref());
    if plan_type.is_none() && subscription_expires_at.is_none() && email.is_none() {
        return None;
    }
    Some(ChatGptAccountInfo {
        plan_type,
        plan_label,
        email,
        subscription_expires_at,
    })
}

pub fn subscription_expires_at_from_value(value: &Value) -> Option<DateTime<Utc>> {
    string(value, "active_until")
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc))
}

pub fn openai_plan_label(plan_type: Option<&str>) -> Option<String> {
    let raw = plan_type?.trim();
    if raw.is_empty() {
        return None;
    }
    let normalized = raw
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace(' ', "_")
        .replace("__", "_");
    if normalized == "free" {
        return Some("Free".into());
    }
    if normalized == "plus" {
        return Some("Plus".into());
    }
    if normalized.contains("pro") && normalized.contains("100") {
        return Some("Pro 100x".into());
    }
    if normalized.contains("pro") && normalized.contains("200") {
        return Some("Pro 200x".into());
    }
    if normalized == "pro" {
        return Some("Pro".into());
    }
    Some(raw.to_string())
}

pub fn claude_plan_label(plan_type: Option<&str>) -> Option<String> {
    let raw = plan_type?.trim();
    if raw.is_empty() {
        return None;
    }
    let normalized = raw
        .to_ascii_lowercase()
        .replace('-', "")
        .replace('_', "")
        .replace(' ', "");
    if matches!(
        normalized.as_str(),
        "max" | "max5x" | "max20x" | "claudemax" | "claudemax5x" | "claudemax20x"
    ) {
        return Some("Max".into());
    }
    if matches!(normalized.as_str(), "pro" | "claudepro" | "professional") {
        return Some("Pro".into());
    }
    if matches!(normalized.as_str(), "free" | "claudefree") {
        return Some("Free".into());
    }
    Some(raw.to_string())
}

pub fn quota_from_wham_usage(value: &Value) -> OfficialAccountQuota {
    let rate_limit = value.get("rate_limit");
    let mut quota = OfficialAccountQuota {
        account_id: string(value, "account_id"),
        user_id: string(value, "user_id"),
        email: string(value, "email"),
        plan_type: string(value, "plan_type"),
        plan_label: openai_plan_label(string(value, "plan_type").as_deref()),
        subscription_expires_at: None,
        reset_credits: value
            .get("rate_limit_reset_credits")
            .and_then(|credits| credits.get("available_count"))
            .and_then(number_u64),
        ..OfficialAccountQuota::default()
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

pub fn quota_from_claude_usage(token_value: &Value, usage_value: &Value) -> OfficialAccountQuota {
    let plan_type = deep_string(
        token_value,
        &[
            "plan_label",
            "plan_type",
            "plan",
            "planName",
            "plan_name",
            "subscription",
            "subscription_tier",
            "subscriptionTier",
            "subscription_plan",
            "subscriptionPlan",
            "subscription_label",
            "subscriptionLabel",
            "tier",
            "tier_name",
            "tierName",
        ],
    )
    .or_else(|| {
        deep_string(
            usage_value,
            &[
                "plan_label",
                "plan_type",
                "plan",
                "planName",
                "plan_name",
                "subscription",
                "subscription_tier",
                "subscriptionTier",
                "subscription_plan",
                "subscriptionPlan",
                "subscription_label",
                "subscriptionLabel",
                "tier",
                "tier_name",
                "tierName",
            ],
        )
    });
    OfficialAccountQuota {
        account_id: deep_string(token_value, &["account_id", "account_uuid"]),
        user_id: deep_string(token_value, &["user_id", "account_uuid"]),
        email: deep_string(token_value, &["email_address", "email"]),
        plan_label: claude_plan_label(plan_type.as_deref()),
        plan_type,
        five_hour: claude_usage_window(usage_value.get("five_hour"), 5 * 60 * 60),
        seven_day: claude_usage_window(usage_value.get("seven_day"), 7 * 24 * 60 * 60),
        ..OfficialAccountQuota::default()
    }
}

pub fn quota_from_codex_headers(headers: &HeaderMap) -> Option<OfficialAccountQuota> {
    let primary = header_window(headers, "primary");
    let secondary = header_window(headers, "secondary");
    if primary.is_none() && secondary.is_none() {
        return None;
    }

    let mut quota = OfficialAccountQuota::default();
    if let Some(window) = primary.as_ref() {
        assign_parsed_window(&mut quota, window.clone());
    }
    if let Some(window) = secondary.as_ref() {
        assign_parsed_window(&mut quota, window.clone());
    }
    Some(quota)
}

fn claude_usage_window(
    value: Option<&Value>,
    limit_window_seconds: u64,
) -> Option<OfficialQuotaWindow> {
    let value = value?;
    let used_percent = value
        .get("utilization")
        .or_else(|| value.get("used_percent"))
        .and_then(number_f64)?;
    let reset_at = value
        .get("resets_at")
        .or_else(|| value.get("reset_at"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc);
    let reset_after_seconds = reset_at
        .map(|reset_at| (reset_at - Utc::now()).num_seconds().max(0) as u64)
        .unwrap_or(0);
    Some(OfficialQuotaWindow {
        used_percent,
        limit_window_seconds,
        reset_after_seconds,
        reset_at,
    })
}

fn assign_window(quota: &mut OfficialAccountQuota, value: Option<&Value>) {
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

fn assign_parsed_window(quota: &mut OfficialAccountQuota, window: OfficialQuotaWindow) {
    let minutes = window.limit_window_seconds / 60;
    if minutes <= 360 {
        if quota.five_hour.is_none() {
            quota.five_hour = Some(window);
        }
    } else if quota.seven_day.is_none() {
        quota.seven_day = Some(window);
    }
}

fn parse_window(value: &Value) -> Option<OfficialQuotaWindow> {
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
    Some(OfficialQuotaWindow {
        used_percent,
        limit_window_seconds,
        reset_after_seconds,
        reset_at,
    })
}

fn header_window(headers: &HeaderMap, name: &str) -> Option<OfficialQuotaWindow> {
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
    Some(OfficialQuotaWindow {
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

fn object_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn deep_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = deep_string_for_key(value, key) {
            return Some(value);
        }
    }
    None
}

fn deep_string_for_key(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(object) => object
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                object
                    .values()
                    .find_map(|value| deep_string_for_key(value, key))
            }),
        Value::Array(items) => items
            .iter()
            .find_map(|value| deep_string_for_key(value, key)),
        _ => None,
    }
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|value| value.with_timezone(&Utc))
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
    use super::{
        chatgpt_account_info_from_accounts_check, claude_plan_label,
        fetch_claude_quota_with_token_at, openai_plan_label, quota_from_claude_usage,
        quota_from_codex_headers, quota_from_wham_usage, subscription_expires_at_from_value,
    };
    use axum::{http::HeaderMap as AxumHeaderMap, routing::get, Json, Router};
    use reqwest::header::{HeaderMap, HeaderValue};
    use serde_json::json;
    use tokio::net::TcpListener;

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
    fn maps_openai_plan_labels() {
        assert_eq!(openai_plan_label(Some("free")).as_deref(), Some("Free"));
        assert_eq!(openai_plan_label(Some("plus")).as_deref(), Some("Plus"));
        assert_eq!(
            openai_plan_label(Some("pro_100x")).as_deref(),
            Some("Pro 100x")
        );
        assert_eq!(
            openai_plan_label(Some("pro-200x")).as_deref(),
            Some("Pro 200x")
        );
        assert_eq!(openai_plan_label(Some("pro")).as_deref(), Some("Pro"));
        assert_eq!(openai_plan_label(Some("team")).as_deref(), Some("team"));
    }

    #[test]
    fn maps_claude_plan_labels() {
        assert_eq!(claude_plan_label(Some("free")).as_deref(), Some("Free"));
        assert_eq!(claude_plan_label(Some("pro")).as_deref(), Some("Pro"));
        assert_eq!(claude_plan_label(Some("max")).as_deref(), Some("Max"));
        assert_eq!(claude_plan_label(Some("max_5x")).as_deref(), Some("Max"));
        assert_eq!(claude_plan_label(Some("max-20x")).as_deref(), Some("Max"));
        assert_eq!(
            claude_plan_label(Some("claude_pro")).as_deref(),
            Some("Pro")
        );
        assert_eq!(
            claude_plan_label(Some("claude_max_20x")).as_deref(),
            Some("Max")
        );
        assert_eq!(
            claude_plan_label(Some("enterprise")).as_deref(),
            Some("enterprise")
        );
    }

    #[test]
    fn parses_claude_usage_into_official_quota() {
        let quota = quota_from_claude_usage(
            &json!({
                "account_uuid": "account-1",
                "email_address": "zoe@example.com",
                "metadata": {
                    "claudeAiOauth": {
                        "account": { "subscriptionTier": "claude_max_20x" }
                    }
                }
            }),
            &json!({
                "five_hour": { "utilization": 12.5, "resets_at": "2026-06-20T10:00:00Z" },
                "seven_day": { "utilization": 34.0, "resets_at": "2026-06-27T10:00:00Z" },
                "seven_day_sonnet": { "utilization": 56.0, "resets_at": "2026-06-27T10:00:00Z" }
            }),
        );

        assert_eq!(quota.account_id.as_deref(), Some("account-1"));
        assert_eq!(quota.email.as_deref(), Some("zoe@example.com"));
        assert_eq!(quota.plan_type.as_deref(), Some("claude_max_20x"));
        assert_eq!(quota.plan_label.as_deref(), Some("Max"));
        assert_eq!(quota.five_hour.unwrap().used_percent, 12.5);
        assert_eq!(quota.seven_day.unwrap().used_percent, 34.0);
    }

    #[test]
    fn parses_claude_plan_from_usage_when_token_metadata_is_missing() {
        let quota = quota_from_claude_usage(
            &json!({ "source": "Claude Code credentials" }),
            &json!({
                "account": { "plan_name": "claude_pro" },
                "five_hour": { "utilization": 4.0, "resets_at": "2026-06-20T10:00:00Z" },
                "seven_day": { "utilization": 8.0, "resets_at": "2026-06-27T10:00:00Z" }
            }),
        );

        assert_eq!(quota.plan_type.as_deref(), Some("claude_pro"));
        assert_eq!(quota.plan_label.as_deref(), Some("Pro"));
    }

    #[tokio::test]
    async fn fetches_claude_usage_with_oauth_headers() {
        let router = Router::new().route(
            "/usage",
            get(|headers: AxumHeaderMap| async move {
                assert_eq!(
                    headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok()),
                    Some("Bearer access-1")
                );
                assert_eq!(
                    headers
                        .get("anthropic-beta")
                        .and_then(|value| value.to_str().ok()),
                    Some("oauth-2025-04-20")
                );
                Json(json!({
                    "five_hour": { "utilization": 12.5, "resets_at": "2026-06-20T10:00:00Z" },
                    "seven_day": { "utilization": 34.0, "resets_at": "2026-06-27T10:00:00Z" }
                }))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let url = format!("http://{addr}/usage");

        let quota = fetch_claude_quota_with_token_at(
            &reqwest::Client::new(),
            &url,
            &json!({ "plan_type": "pro" }),
            "access-1",
        )
        .await
        .unwrap();

        assert_eq!(quota.plan_label.as_deref(), Some("Pro"));
        assert_eq!(quota.five_hour.unwrap().used_percent, 12.5);
        assert_eq!(quota.seven_day.unwrap().used_percent, 34.0);
    }

    #[test]
    fn account_check_prefers_exact_account_and_account_plan_type() {
        let value = json!({
            "accounts": {
                "acct_free": {
                    "account": { "id": "acct_free", "plan_type": "free", "is_default": true }
                },
                "acct_pro": {
                    "account": { "id": "acct_pro", "plan_type": "pro_200x", "email": "p@example.com" },
                    "entitlement": { "expires_at": "2026-06-10T02:52:15Z" }
                }
            }
        });

        let info = chatgpt_account_info_from_accounts_check(&value, "acct_pro").unwrap();

        assert_eq!(info.plan_type.as_deref(), Some("pro_200x"));
        assert_eq!(info.plan_label.as_deref(), Some("Pro 200x"));
        assert_eq!(info.email.as_deref(), Some("p@example.com"));
        assert_eq!(
            info.subscription_expires_at.unwrap().to_rfc3339(),
            "2026-06-10T02:52:15+00:00"
        );
    }

    #[test]
    fn account_check_reads_entitlement_subscription_plan_and_fallback_order() {
        let value = json!({
            "accounts": {
                "acct_free": {
                    "account": { "id": "acct_free", "is_default": false },
                    "entitlement": { "subscription_plan": "free" }
                },
                "acct_plus": {
                    "account": { "id": "acct_plus", "is_default": true },
                    "entitlement": { "subscription_plan": "plus" }
                },
                "acct_pro": {
                    "account": { "id": "acct_pro" },
                    "entitlement": { "subscription_plan": "pro_100x" }
                }
            }
        });

        let info = chatgpt_account_info_from_accounts_check(&value, "missing").unwrap();

        assert_eq!(info.plan_type.as_deref(), Some("plus"));
        assert_eq!(info.plan_label.as_deref(), Some("Plus"));
    }

    #[test]
    fn subscription_active_until_is_parsed() {
        let expires = subscription_expires_at_from_value(&json!({
            "plan_type": "plus",
            "active_until": "2026-06-10T02:52:15Z"
        }))
        .unwrap();

        assert_eq!(expires.to_rfc3339(), "2026-06-10T02:52:15+00:00");
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
