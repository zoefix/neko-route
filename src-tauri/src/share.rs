//! 共享功能的纯逻辑：身份/密钥/令牌生成 + 令牌与模型限权校验。
//! 与隧道(`share_tunnel`)、服务端鉴权、UI 解耦，便于单测。

use crate::types::ShareToken;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// 共享身份（公网子域名标签）：16 位小写字母数字（取 uuid 的前 16 位 hex）。
/// 与 Go 服务端 `protocol.ValidID` 对齐。
pub fn generate_identity() -> String {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(16)
        .collect()
}

/// 向 FRP 服务端证明身份所有权的密钥（32 位 hex）。
pub fn generate_secret() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 生成一个分享令牌（朋友当 api key 用）。沿用主流 `sk-` 前缀，兼容各类 OpenAI 客户端。
pub fn generate_token() -> String {
    format!("sk-{}", uuid::Uuid::new_v4().simple())
}

/// 身份是否合法：恰好 16 位小写字母数字。
pub fn valid_identity(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// 从 `Authorization` 头（`Bearer xxx` 或裸串）里取出令牌字符串。
pub fn extract_presented_token(authorization: &str) -> &str {
    let value = authorization.trim();
    value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .unwrap_or(value)
        .trim()
}

/// 按 token 串精确匹配一个分享令牌（空串不匹配）。
pub fn find_token<'a>(tokens: &'a [ShareToken], presented: &str) -> Option<&'a ShareToken> {
    let presented = presented.trim();
    if presented.is_empty() {
        return None;
    }
    tokens.iter().find(|token| token.token == presented)
}

/// 令牌是否授权该模型 id（空列表 = 不授权任何模型）。
/// 生产路径已改用 `resolve_shared_model`（支持别名）；此简单版仅单测保留。
#[cfg(test)]
pub fn token_allows_model(token: &ShareToken, model_id: &str) -> bool {
    let model_id = model_id.trim();
    !model_id.is_empty()
        && token
            .allowed_model_ids
            .iter()
            .any(|allowed| allowed == model_id)
}

/// 把下游请求的模型 ID（可能是自定义别名，也可能是内部 ID）解析回内部模型 ID。
/// 返回 None = 该令牌未授权此模型。下游用别名或内部 ID 请求都能识别。
pub fn resolve_shared_model(token: &ShareToken, requested: &str) -> Option<String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return None;
    }
    // 直接是内部已授权 ID。
    if token.allowed_model_ids.iter().any(|id| id == requested) {
        return Some(requested.to_string());
    }
    // 是某个已授权内部 ID 的自定义别名。
    for (internal, alias) in &token.model_aliases {
        if alias.trim() == requested && token.allowed_model_ids.iter().any(|id| id == internal) {
            return Some(internal.clone());
        }
    }
    None
}

/// 下游 `/models` 看到的模型 ID：有非空别名用别名，否则用内部 ID。
pub fn display_model_id(token: &ShareToken, internal_id: &str) -> String {
    token
        .model_aliases
        .get(internal_id)
        .map(|alias| alias.trim())
        .filter(|alias| !alias.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| internal_id.to_string())
}

/// 进程级共享限额器：内存跟踪每令牌的并发数与最近一分钟请求时刻；
/// 金额累计由调用方查请求日志得到后传入。
pub struct ShareLimiter {
    inner: Mutex<HashMap<String, TokenRuntime>>,
}

#[derive(Default)]
struct TokenRuntime {
    active: u32,
    recent: VecDeque<Instant>,
}

/// 一次共享请求占用的并发位，drop 时自动释放（请求完成即归还）。
pub struct ShareGuard {
    token: String,
}

impl Drop for ShareGuard {
    fn drop(&mut self) {
        if let Some(limiter) = SHARE_LIMITER.get() {
            if let Ok(mut map) = limiter.inner.lock() {
                if let Some(rt) = map.get_mut(&self.token) {
                    rt.active = rt.active.saturating_sub(1);
                }
            }
        }
    }
}

static SHARE_LIMITER: OnceLock<ShareLimiter> = OnceLock::new();

/// 取进程级限额器单例。
pub fn share_limiter() -> &'static ShareLimiter {
    SHARE_LIMITER.get_or_init(|| ShareLimiter {
        inner: Mutex::new(HashMap::new()),
    })
}

impl ShareLimiter {
    /// 各令牌当前正在处理的请求数（仅返回 >0 的），供 UI 显示「正在使用 / 总并发」(如 2/10)。
    pub fn active_counts(&self) -> HashMap<String, u32> {
        self.inner
            .lock()
            .map(|map| {
                map.iter()
                    .filter(|(_, rt)| rt.active > 0)
                    .map(|(token, rt)| (token.clone(), rt.active))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 校验金额/并发/RPM 限额：通过则占用一个并发位 + 记一次请求时刻，返回守卫；超限返回中文原因。
    /// `spend_usd` = 该令牌历史累计消费（调用方查日志得到）。0 表示对应限制不启用。
    pub fn acquire(&self, token: &ShareToken, spend_usd: f64) -> Result<ShareGuard, String> {
        let mut map = self.inner.lock().map_err(|_| "限额器状态异常".to_string())?;
        let rt = map.entry(token.token.clone()).or_default();
        let now = Instant::now();
        while let Some(front) = rt.recent.front().copied() {
            if now.duration_since(front).as_secs() >= 60 {
                rt.recent.pop_front();
            } else {
                break;
            }
        }
        if let Some(limit) = token.amount_limit_usd {
            if spend_usd >= limit {
                return Err(format!("额度已用尽（已消费 ${spend_usd:.2} / 上限 ${limit:.2}）"));
            }
        }
        if token.concurrency_limit > 0 && rt.active >= token.concurrency_limit {
            return Err(format!("并发超限（{} / {}）", rt.active, token.concurrency_limit));
        }
        if token.rpm_limit > 0 && rt.recent.len() as u32 >= token.rpm_limit {
            return Err(format!(
                "请求频率超限（{} / {} RPM）",
                rt.recent.len(),
                token.rpm_limit
            ));
        }
        rt.active += 1;
        rt.recent.push_back(now);
        Ok(ShareGuard {
            token: token.token.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(tok: &str, models: &[&str]) -> ShareToken {
        ShareToken {
            token: tok.into(),
            label: String::new(),
            allowed_model_ids: models.iter().map(|m| m.to_string()).collect(),
            amount_limit_usd: Some(1000.0),
            concurrency_limit: 10,
            rpm_limit: 0,
            model_aliases: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn generated_identity_is_valid_16_char() {
        let id = generate_identity();
        assert_eq!(id.len(), 16);
        assert!(valid_identity(&id), "{id}");
    }

    #[test]
    fn valid_identity_rejects_bad_shapes() {
        assert!(!valid_identity("short"));
        assert!(!valid_identity("ABCD1234EFGH5678")); // uppercase
        assert!(!valid_identity("abcd-234efgh5678")); // symbol
    }

    #[test]
    fn extract_token_handles_bearer_and_bare() {
        assert_eq!(extract_presented_token("Bearer nrs-abc"), "nrs-abc");
        assert_eq!(extract_presented_token("  bearer nrs-abc "), "nrs-abc");
        assert_eq!(extract_presented_token("nrs-bare"), "nrs-bare");
    }

    #[test]
    fn find_token_matches_exact_only() {
        let tokens = vec![token("nrs-aaa", &["m1"]), token("nrs-bbb", &["m2"])];
        assert_eq!(find_token(&tokens, "nrs-bbb").unwrap().token, "nrs-bbb");
        assert!(find_token(&tokens, "nrs-ccc").is_none());
        assert!(find_token(&tokens, "").is_none());
    }

    #[test]
    fn model_scope_enforced() {
        let t = token("nrs-aaa", &["neko-model-1", "neko-model-2"]);
        assert!(token_allows_model(&t, "neko-model-1"));
        assert!(!token_allows_model(&t, "neko-model-9"));
        assert!(!token_allows_model(&t, ""));
        // 空允许列表 = 不授权任何模型
        assert!(!token_allows_model(&token("nrs-empty", &[]), "neko-model-1"));
    }

    #[test]
    fn resolve_model_handles_alias_and_internal() {
        let mut t = token("nrs-x", &["neko-model-1", "neko-model-2"]);
        t.model_aliases.insert("neko-model-1".into(), "gpt-5.5".into());
        // 内部 ID 直接命中
        assert_eq!(
            resolve_shared_model(&t, "neko-model-2").as_deref(),
            Some("neko-model-2")
        );
        // 别名解析回内部 ID
        assert_eq!(
            resolve_shared_model(&t, "gpt-5.5").as_deref(),
            Some("neko-model-1")
        );
        // 未授权 / 空
        assert!(resolve_shared_model(&t, "gpt-9").is_none());
        assert!(resolve_shared_model(&t, "").is_none());
    }

    #[test]
    fn display_id_prefers_alias() {
        let mut t = token("nrs-x", &["neko-model-1", "neko-model-2"]);
        t.model_aliases.insert("neko-model-1".into(), "gpt-5.5".into());
        assert_eq!(display_model_id(&t, "neko-model-1"), "gpt-5.5");
        // 无别名 → 内部 ID
        assert_eq!(display_model_id(&t, "neko-model-2"), "neko-model-2");
    }

    #[test]
    fn limiter_amount_blocks_when_spend_reaches_limit() {
        let t = token("nrs-limit-amount", &["m1"]); // amount 上限 $1000
        assert!(share_limiter().acquire(&t, 1000.0).is_err());
        assert!(share_limiter().acquire(&t, 1500.0).is_err());
        assert!(share_limiter().acquire(&t, 999.0).is_ok());
    }

    #[test]
    fn limiter_concurrency_blocks_and_guard_releases() {
        let mut t = token("nrs-limit-conc", &["m1"]);
        t.concurrency_limit = 2;
        t.amount_limit_usd = None;
        let g1 = share_limiter().acquire(&t, 0.0).unwrap();
        let _g2 = share_limiter().acquire(&t, 0.0).unwrap();
        assert!(share_limiter().acquire(&t, 0.0).is_err()); // 第三个超并发
        drop(g1);
        assert!(share_limiter().acquire(&t, 0.0).is_ok()); // 释放后又能拿
    }

    #[test]
    fn active_counts_reflects_live_guards() {
        let mut t = token("nrs-active-cnt", &["m1"]);
        t.concurrency_limit = 0; // 不限并发，便于同时持有多个守卫
        t.amount_limit_usd = None;
        let g1 = share_limiter().acquire(&t, 0.0).unwrap();
        let g2 = share_limiter().acquire(&t, 0.0).unwrap();
        assert_eq!(
            share_limiter().active_counts().get("nrs-active-cnt").copied(),
            Some(2)
        );
        drop(g1);
        assert_eq!(
            share_limiter().active_counts().get("nrs-active-cnt").copied(),
            Some(1)
        );
        drop(g2);
        // 归零后不再出现在表里（active_counts 只返回 >0 的）。
        assert_eq!(
            share_limiter().active_counts().get("nrs-active-cnt").copied(),
            None
        );
    }

    #[test]
    fn limiter_rpm_blocks_over_threshold() {
        let mut t = token("nrs-limit-rpm", &["m1"]);
        t.rpm_limit = 2;
        t.concurrency_limit = 0; // 并发不限
        t.amount_limit_usd = None;
        let _g1 = share_limiter().acquire(&t, 0.0).unwrap();
        let _g2 = share_limiter().acquire(&t, 0.0).unwrap();
        assert!(share_limiter().acquire(&t, 0.0).is_err()); // 同一分钟第3次超 RPM
    }
}
