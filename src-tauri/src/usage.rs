use crate::types::{ProviderProtocol, TokenUsage};
use serde_json::Value;

/// Parse a provider `usage` object into the normalized [`TokenUsage`].
///
/// Each upstream uses different field names and different conventions for
/// whether the input count already includes cached tokens:
///
/// - OpenAI Responses: `input_tokens`, `output_tokens`,
///   `input_tokens_details.cached_tokens` (cached is a *subset* of input).
/// - OpenAI Chat Completions: `prompt_tokens`, `completion_tokens`,
///   `prompt_tokens_details.cached_tokens` (cached is a subset of prompt).
/// - Anthropic Messages: `input_tokens`, `output_tokens`,
///   `cache_read_input_tokens`, `cache_creation_input_tokens` (these are
///   *separate* from `input_tokens`).
pub fn parse_usage(protocol: ProviderProtocol, usage: &Value) -> TokenUsage {
    match protocol {
        ProviderProtocol::AnthropicMessages => parse_anthropic(usage),
        ProviderProtocol::OpenAiChatCompletions => parse_openai(usage, true),
        ProviderProtocol::OpenAiResponses => parse_openai(usage, false),
    }
}

fn u(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn parse_openai(usage: &Value, chat: bool) -> TokenUsage {
    let (input_key, output_key, input_details) = if chat {
        (
            "prompt_tokens",
            "completion_tokens",
            "prompt_tokens_details",
        )
    } else {
        ("input_tokens", "output_tokens", "input_tokens_details")
    };
    let input = u(usage, input_key);
    let output = u(usage, output_key);
    let cache_read = usage
        .get(input_details)
        .map(|d| u(d, "cached_tokens"))
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input + output);
    TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
        total_tokens: total,
    }
}

fn parse_anthropic(usage: &Value) -> TokenUsage {
    let input = u(usage, "input_tokens");
    let output = u(usage, "output_tokens");
    let cache_read = u(usage, "cache_read_input_tokens");
    let cache_write = u(usage, "cache_creation_input_tokens");
    // Anthropic counts cache tokens separately from input_tokens.
    let total = input + output + cache_read + cache_write;
    TokenUsage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        total_tokens: total,
    }
}

/// Scan accumulated SSE / JSON text from a raw OpenAI Responses passthrough and
/// extract the final usage. The `response.completed` event carries
/// `response.usage`; non-streaming bodies carry a top-level `usage`.
pub fn usage_from_responses_text(text: &str) -> Option<TokenUsage> {
    // Streaming: find the last response.usage object.
    if let Some(usage) = last_json_object_after(text, "\"usage\"") {
        let parsed = parse_openai(&usage, false);
        if !parsed.is_empty() {
            return Some(parsed);
        }
    }
    None
}

/// Find the JSON object that follows the last occurrence of `marker` (e.g.
/// `"usage"`) and parse it, tolerating surrounding SSE framing.
fn last_json_object_after(text: &str, marker: &str) -> Option<Value> {
    let mut search_from = 0;
    let mut found: Option<Value> = None;
    while let Some(rel) = text[search_from..].find(marker) {
        let pos = search_from + rel + marker.len();
        search_from = pos;
        // Skip whitespace and the ':' separator.
        let rest = text[pos..].trim_start();
        let rest = rest.strip_prefix(':').map(str::trim_start).unwrap_or(rest);
        if !rest.starts_with('{') {
            continue;
        }
        if let Some(obj) = extract_balanced_object(rest) {
            if let Ok(value) = serde_json::from_str::<Value>(&obj) {
                if value.get("input_tokens").is_some()
                    || value.get("output_tokens").is_some()
                    || value.get("total_tokens").is_some()
                {
                    found = Some(value);
                }
            }
        }
    }
    found
}

/// Given text starting at `{`, return the balanced `{...}` substring.
fn extract_balanced_object(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_responses_usage() {
        let usage = json!({
            "input_tokens": 100,
            "output_tokens": 40,
            "input_tokens_details": { "cached_tokens": 25 },
            "total_tokens": 140
        });
        let parsed = parse_usage(ProviderProtocol::OpenAiResponses, &usage);
        assert_eq!(parsed.input_tokens, 100);
        assert_eq!(parsed.output_tokens, 40);
        assert_eq!(parsed.cache_read_tokens, 25);
        assert_eq!(parsed.total_tokens, 140);
    }

    #[test]
    fn parses_openai_chat_usage() {
        let usage = json!({
            "prompt_tokens": 80,
            "completion_tokens": 20,
            "prompt_tokens_details": { "cached_tokens": 10 },
            "total_tokens": 100
        });
        let parsed = parse_usage(ProviderProtocol::OpenAiChatCompletions, &usage);
        assert_eq!(parsed.input_tokens, 80);
        assert_eq!(parsed.output_tokens, 20);
        assert_eq!(parsed.cache_read_tokens, 10);
        assert_eq!(parsed.total_tokens, 100);
    }

    #[test]
    fn parses_anthropic_usage_with_separate_cache() {
        let usage = json!({
            "input_tokens": 50,
            "output_tokens": 30,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 100
        });
        let parsed = parse_usage(ProviderProtocol::AnthropicMessages, &usage);
        assert_eq!(parsed.input_tokens, 50);
        assert_eq!(parsed.output_tokens, 30);
        assert_eq!(parsed.cache_read_tokens, 200);
        assert_eq!(parsed.cache_write_tokens, 100);
        // total includes the separate cache buckets
        assert_eq!(parsed.total_tokens, 380);
    }

    #[test]
    fn extracts_usage_from_sse_completed_event() {
        let text = "event: response.output_text.delta\ndata: {\"delta\":\"hi\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":5,\"total_tokens\":17}}}\n\n";
        let parsed = usage_from_responses_text(text).unwrap();
        assert_eq!(parsed.input_tokens, 12);
        assert_eq!(parsed.output_tokens, 5);
        assert_eq!(parsed.total_tokens, 17);
    }

    #[test]
    fn ignores_text_without_usage() {
        let text = "event: response.output_text.delta\ndata: {\"delta\":\"hi\"}\n\n";
        assert!(usage_from_responses_text(text).is_none());
    }
}
