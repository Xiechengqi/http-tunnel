use serde_json::Value;

const REDACTED: &str = "[redacted]";

pub fn redact_text(input: &str) -> String {
    if let Ok(mut value) = serde_json::from_str::<Value>(input) {
        redact_json_value(&mut value);
        return serde_json::to_string(&value).unwrap_or_else(|_| REDACTED.to_string());
    }
    if looks_like_sensitive_assignment(input) {
        REDACTED.to_string()
    } else {
        input.to_string()
    }
}

pub fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object.iter_mut() {
                if sensitive_key(key) {
                    *value = Value::String(REDACTED.to_string());
                } else {
                    redact_json_value(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json_value(item);
            }
        }
        Value::String(text) if looks_like_sensitive_assignment(text) => {
            *text = REDACTED.to_string();
        }
        _ => {}
    }
}

pub fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("password")
        || key.contains("secret")
        || key.contains("token")
        || key.ends_with("_hash")
        || matches!(
            key.as_str(),
            "authorization" | "cookie" | "set-cookie" | "x-api-key" | "x-http-tunnel-access-token"
        )
}

fn looks_like_sensitive_assignment(input: &str) -> bool {
    let lower = input.to_ascii_lowercase();
    [
        "password=",
        "secret=",
        "token=",
        "authorization:",
        "cookie:",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_sensitive_json_keys() {
        let redacted =
            redact_text(r#"{"token":"abc","nested":{"password":"pw"},"safe":"visible"}"#);
        assert!(redacted.contains("[redacted]"));
        assert!(redacted.contains("visible"));
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("pw"));
    }

    #[test]
    fn redacts_sensitive_text_assignments_without_hiding_safe_events() {
        assert_eq!(redact_text("token=abc123"), "[redacted]");
        assert_eq!(
            redact_text("missing or invalid CSRF token"),
            "missing or invalid CSRF token"
        );
    }
}
