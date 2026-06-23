#[derive(Debug, Clone, Copy)]
struct ModelPrice {
    input_per_million: f64,
    output_per_million: f64,
    cache_read_per_million: f64,
    cache_write_per_million: f64,
}

impl ModelPrice {
    fn estimate(
        self,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
    ) -> f64 {
        ((input_tokens as f64) * self.input_per_million
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
    price_for_model(model).map(|price| {
        price.estimate(
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        )
    })
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
}
