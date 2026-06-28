#[derive(Debug, Clone, Copy)]
struct ModelPrice {
    input_per_million: f64,
    output_per_million: f64,
    cache_read_per_million: f64,
    cache_write_per_million: f64,
}

impl ModelPrice {
    /// `cached_in_input`：缓存 token 是否已包含在 `input_tokens` 里（OpenAI 约定 = true，
    /// 缓存是 input 的子集；Anthropic 约定 = false，缓存与 input 分离）。为 true 时把缓存部分
    /// 从全价输入里扣除，避免缓存 token 被「全价 + 缓存价」重复计费。
    fn estimate(
        self,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        cached_in_input: bool,
    ) -> f64 {
        let full_rate_input = if cached_in_input {
            input_tokens.saturating_sub(cache_read_tokens + cache_write_tokens)
        } else {
            input_tokens
        };
        ((full_rate_input as f64) * self.input_per_million
            + (output_tokens as f64) * self.output_per_million
            + (cache_read_tokens as f64) * self.cache_read_per_million
            + (cache_write_tokens as f64) * self.cache_write_per_million)
            / 1_000_000.0
    }
}

pub fn estimate_model_cost_usd(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
) -> Option<f64> {
    // Anthropic（claude 系列）的 usage 把缓存与 input 分开统计；其余（OpenAI/o/gpt 系列）
    // 缓存是 input 的子集，需在全价输入里扣除。
    let cached_in_input = !model.to_ascii_lowercase().starts_with("claude");
    price_for_model(model).map(|price| {
        price.estimate(
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            cached_in_input,
        )
    })
}

/// 画图成本估算（美元）。按 gpt-image-1 市场价的 1024² 档：low $0.011 / medium $0.042 /
/// high $0.167 每张；更大尺寸（1536/1792）约 1.5×；× 张数。官方账号订阅不按张实扣，这里同样
/// 作「等效参考价」用于共享额度计费。
pub fn estimate_image_cost_usd(quality: Option<&str>, size: Option<&str>, n: u64) -> f64 {
    let per_image = match quality.unwrap_or("medium").to_ascii_lowercase().as_str() {
        "low" => 0.011,
        "high" => 0.167,
        _ => 0.042, // medium / auto / 默认
    };
    let size_mult = match size.unwrap_or("1024x1024") {
        s if s.contains("1536") || s.contains("1792") => 1.5,
        _ => 1.0,
    };
    per_image * size_mult * (n.max(1) as f64)
}

fn price_for_model(model: &str) -> Option<ModelPrice> {
    let model = model.to_ascii_lowercase();
    let model = model.as_str();

    if model.starts_with("gpt-5.5") {
        return Some(ModelPrice {
            input_per_million: 1.25,
            output_per_million: 10.0,
            cache_read_per_million: 0.125,
            cache_write_per_million: 1.25,
        });
    }
    if model.starts_with("gpt-5.4-mini") || model.starts_with("codex-mini") {
        return Some(ModelPrice {
            input_per_million: 0.15,
            output_per_million: 0.60,
            cache_read_per_million: 0.015,
            cache_write_per_million: 0.15,
        });
    }
    if model.starts_with("gpt-5.4")
        || model.starts_with("gpt-5.3-codex")
        || model.starts_with("gpt-5.2")
        || model.starts_with("codex-auto-review")
    {
        return Some(ModelPrice {
            input_per_million: 1.25,
            output_per_million: 10.0,
            cache_read_per_million: 0.125,
            cache_write_per_million: 1.25,
        });
    }
    if model.starts_with("gpt-4.1-mini") {
        return Some(ModelPrice {
            input_per_million: 0.40,
            output_per_million: 1.60,
            cache_read_per_million: 0.10,
            cache_write_per_million: 0.40,
        });
    }
    if model.starts_with("gpt-4.1") {
        return Some(ModelPrice {
            input_per_million: 2.0,
            output_per_million: 8.0,
            cache_read_per_million: 0.50,
            cache_write_per_million: 2.0,
        });
    }
    if model.starts_with("gpt-4o-mini") {
        return Some(ModelPrice {
            input_per_million: 0.15,
            output_per_million: 0.60,
            cache_read_per_million: 0.075,
            cache_write_per_million: 0.15,
        });
    }
    if model.starts_with("gpt-4o") || model.starts_with("chatgpt-4o") {
        return Some(ModelPrice {
            input_per_million: 2.5,
            output_per_million: 10.0,
            cache_read_per_million: 1.25,
            cache_write_per_million: 2.5,
        });
    }
    if model.starts_with("gpt-4-turbo") {
        return Some(ModelPrice {
            input_per_million: 10.0,
            output_per_million: 30.0,
            cache_read_per_million: 5.0,
            cache_write_per_million: 10.0,
        });
    }
    if model.starts_with("o3-mini") || model.starts_with("o1-mini") || model.starts_with("o4-mini")
    {
        return Some(ModelPrice {
            input_per_million: 1.1,
            output_per_million: 4.4,
            cache_read_per_million: 0.55,
            cache_write_per_million: 1.1,
        });
    }
    if model.starts_with("o3") {
        return Some(ModelPrice {
            input_per_million: 2.0,
            output_per_million: 8.0,
            cache_read_per_million: 0.5,
            cache_write_per_million: 2.0,
        });
    }
    if model.starts_with("o1") {
        return Some(ModelPrice {
            input_per_million: 15.0,
            output_per_million: 60.0,
            cache_read_per_million: 7.5,
            cache_write_per_million: 15.0,
        });
    }
    // Claude 市场定价（等效 API 价；Max 订阅不实际按 token 扣，仅作参考）。
    if model.starts_with("claude-opus-4") {
        return Some(ModelPrice {
            input_per_million: 15.0,
            output_per_million: 75.0,
            cache_read_per_million: 1.5,
            cache_write_per_million: 18.75,
        });
    }
    if model.starts_with("claude-sonnet-4") {
        return Some(ModelPrice {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cache_read_per_million: 0.3,
            cache_write_per_million: 3.75,
        });
    }
    if model.starts_with("claude-haiku-4") {
        return Some(ModelPrice {
            input_per_million: 1.0,
            output_per_million: 5.0,
            cache_read_per_million: 0.1,
            cache_write_per_million: 1.25,
        });
    }
    // Claude 3.x 系列
    if model.starts_with("claude-3-7-sonnet")
        || model.starts_with("claude-3.7-sonnet")
        || model.starts_with("claude-3-5-sonnet")
        || model.starts_with("claude-3.5-sonnet")
    {
        return Some(ModelPrice {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cache_read_per_million: 0.3,
            cache_write_per_million: 3.75,
        });
    }
    if model.starts_with("claude-3-5-haiku") || model.starts_with("claude-3.5-haiku") {
        return Some(ModelPrice {
            input_per_million: 0.8,
            output_per_million: 4.0,
            cache_read_per_million: 0.08,
            cache_write_per_million: 1.0,
        });
    }
    if model.starts_with("claude-3-opus") {
        return Some(ModelPrice {
            input_per_million: 15.0,
            output_per_million: 75.0,
            cache_read_per_million: 1.5,
            cache_write_per_million: 18.75,
        });
    }
    if model.starts_with("claude-3-haiku") {
        return Some(ModelPrice {
            input_per_million: 0.25,
            output_per_million: 1.25,
            cache_read_per_million: 0.03,
            cache_write_per_million: 0.30,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::estimate_model_cost_usd;

    #[test]
    fn estimates_known_model_cost() {
        let cost = estimate_model_cost_usd("gpt-5.5", 1_000_000, 1_000_000, 0, 0).unwrap();
        assert!((cost - 11.25).abs() < 0.0001);
    }

    #[test]
    fn unknown_model_has_no_cost() {
        assert!(estimate_model_cost_usd("custom-model", 1, 1, 0, 0).is_none());
    }

    #[test]
    fn estimates_claude_opus_cost() {
        // 1M input @ $15 + 1M output @ $75 = $90（上游真实模型按 Claude 定价）。
        let cost = estimate_model_cost_usd("claude-opus-4-8", 1_000_000, 1_000_000, 0, 0).unwrap();
        assert!((cost - 90.0).abs() < 0.0001);
    }

    #[test]
    fn openai_cached_not_double_billed() {
        // gpt-5.5: input $1.25/M, cache_read $0.125/M。10000 input 里含 8000 缓存。
        // 正确：非缓存 2000*1.25 + 缓存 8000*0.125 = 2500 + 1000 = 3500 → $0.0035。
        // （旧 bug 会算 10000*1.25 + 8000*0.125 = $0.0135，高估近 4x。）
        let cost = estimate_model_cost_usd("gpt-5.5", 10_000, 0, 8_000, 0).unwrap();
        assert!((cost - 0.0035).abs() < 1e-9, "{cost}");
    }

    #[test]
    fn image_cost_by_quality_size_count() {
        use super::estimate_image_cost_usd;
        // medium 1024² 1 张 = $0.042
        assert!((estimate_image_cost_usd(Some("medium"), Some("1024x1024"), 1) - 0.042).abs() < 1e-9);
        // high + 大尺寸(1.5×) + 2 张
        assert!(
            (estimate_image_cost_usd(Some("high"), Some("1536x1024"), 2) - 0.167 * 1.5 * 2.0).abs()
                < 1e-9
        );
        // 默认(无 quality) = medium；n=0 当 1 张算
        assert!((estimate_image_cost_usd(None, None, 0) - 0.042).abs() < 1e-9);
    }

    #[test]
    fn anthropic_cache_separate_from_input() {
        // claude: 缓存与 input 分离，不扣除。input 10000 全价 + cache_read 8000 缓存价。
        // claude-opus: input $15/M, cache_read $1.5/M → 10000*15 + 8000*1.5 = $0.162。
        let cost = estimate_model_cost_usd("claude-opus-4-8", 10_000, 0, 8_000, 0).unwrap();
        assert!((cost - 0.162).abs() < 1e-9, "{cost}");
    }
}
