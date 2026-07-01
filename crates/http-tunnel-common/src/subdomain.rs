use crate::{CommonError, Result};

pub const RESERVED_SUBDOMAINS: &[&str] = &[
    "api",
    "admin",
    "www",
    "static",
    "assets",
    "dashboard",
    "health",
    "status",
    "setup",
    "login",
    "localhost",
    "root",
];

pub fn normalize_subdomain(input: &str) -> String {
    input.trim().to_ascii_lowercase()
}

pub fn is_reserved_subdomain(input: &str) -> bool {
    let normalized = normalize_subdomain(input);
    RESERVED_SUBDOMAINS
        .iter()
        .any(|candidate| *candidate == normalized)
}

pub fn validate_subdomain(input: &str) -> Result<String> {
    let s = normalize_subdomain(input);
    if s.is_empty() || s.len() > 63 {
        return Err(CommonError::InvalidSubdomain(input.to_string()));
    }
    if s.starts_with('-') || s.ends_with('-') {
        return Err(CommonError::InvalidSubdomain(input.to_string()));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(CommonError::InvalidSubdomain(input.to_string()));
    }
    if is_reserved_subdomain(&s) {
        return Err(CommonError::ReservedSubdomain(s));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_subdomain_accepts_lowercase_dns_label() {
        assert_eq!(validate_subdomain("demo-1").unwrap(), "demo-1");
    }

    #[test]
    fn valid_subdomain_normalizes_uppercase() {
        assert_eq!(validate_subdomain("Demo").unwrap(), "demo");
    }

    #[test]
    fn valid_subdomain_rejects_empty() {
        assert!(validate_subdomain("").is_err());
    }

    #[test]
    fn valid_subdomain_rejects_underscore() {
        assert!(validate_subdomain("bad_name").is_err());
    }

    #[test]
    fn valid_subdomain_rejects_leading_or_trailing_dash() {
        assert!(validate_subdomain("-demo").is_err());
        assert!(validate_subdomain("demo-").is_err());
    }

    #[test]
    fn reserved_subdomain_rejects_admin_api_www() {
        assert!(validate_subdomain("admin").is_err());
        assert!(validate_subdomain("api").is_err());
        assert!(validate_subdomain("www").is_err());
    }
}
