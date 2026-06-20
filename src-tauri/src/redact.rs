use regex::Regex;
use std::sync::OnceLock;

pub fn redact(input: &str) -> String {
    let mut output = input.to_string();
    for re in patterns() {
        output = re.replace_all(&output, "$1[REDACTED]").to_string();
    }
    output
}

fn patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r#"(?i)(authorization\s*[:=]\s*bearer\s+)[^\s"',}]+"#).unwrap(),
            Regex::new(r#"(?i)(api[_-]?key\s*["']?\s*[:=]\s*["']?)[^"',}\s]+"#).unwrap(),
            Regex::new(r#"(?i)(x-api-key\s*[:=]\s*)[^\s"',}]+"#).unwrap(),
            Regex::new(r#"(?i)(token\s*["']?\s*[:=]\s*["']?)[^"',}\s]+"#).unwrap(),
            Regex::new(r#"(?i)(key=)[^&\s]+"#).unwrap(),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redacts_common_secret_shapes() {
        let value = r#"Authorization: Bearer sk-live api_key="secret" token=abc key=123"#;
        let redacted = redact(value);
        assert!(!redacted.contains("sk-live"));
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("123"));
    }
}
