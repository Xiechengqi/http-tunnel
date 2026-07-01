use crate::{CommonError, Result};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServerConfig {
    pub domain: Option<String>,
    pub public_scheme: String,
    pub addr: SocketAddr,
    pub trust_proxy_headers: bool,
    pub trusted_proxy_cidrs: Vec<String>,
    pub database_url: String,
    pub data_dir: String,
    pub tunnel_ttl_seconds: u64,
    pub reserved_ttl_seconds: u64,
    pub max_body_bytes: u64,
    pub max_header_bytes: u64,
    pub max_concurrent_streams: usize,
    pub request_timeout_seconds: u64,
    pub idle_timeout_seconds: u64,
    pub heartbeat_interval_seconds: u64,
    pub stale_session_seconds: u64,
    pub duplicate_session_policy: String,
    pub session_pool_policy: String,
    pub max_ws_message_bytes: usize,
    pub cleanup_interval_seconds: u64,
    pub request_log_retention_days: u64,
    pub event_retention_days: u64,
    pub session_retention_days: u64,
    pub inspector_enabled_default: bool,
    pub inspector_max_body_preview_bytes: usize,
    pub admin_password_hash: Option<String>,
    pub admin_session_secret: Option<String>,
    pub reconnect_token_secret: Option<String>,
    pub turnstile_secret: Option<String>,
    pub turnstile_verify_url: String,
    pub rate_limit_per_ip: u64,
    pub per_tunnel_rate_limit_per_minute: u64,
    pub admin_login_failure_limit: usize,
    pub admin_login_cooldown_seconds: u64,
    pub metrics_public: bool,
    pub metrics_bearer_token_hash: Option<String>,
    pub public_tunnel_create_enabled: bool,
    pub tunnel_create_bearer_token_hash: Option<String>,
    pub max_active_tunnels_per_ip: u64,
    pub reserved_subdomains: Vec<String>,
    pub allow_custom_subdomain: bool,
    pub require_random_subdomain: bool,
    pub release_repo: String,
    pub release_tag: String,
    pub auto_upgrade_enabled: bool,
    pub systemd_unit: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            domain: None,
            public_scheme: "https".to_string(),
            addr: "0.0.0.0:8080".parse().expect("valid default addr"),
            trust_proxy_headers: true,
            trusted_proxy_cidrs: vec!["127.0.0.1/32".to_string(), "::1/128".to_string()],
            database_url: default_database_url(),
            data_dir: default_data_dir(),
            tunnel_ttl_seconds: 86_400,
            reserved_ttl_seconds: 300,
            max_body_bytes: 25 * 1024 * 1024,
            max_header_bytes: 64 * 1024,
            max_concurrent_streams: 128,
            request_timeout_seconds: 60,
            idle_timeout_seconds: 300,
            heartbeat_interval_seconds: 15,
            stale_session_seconds: 45,
            duplicate_session_policy: "replace".to_string(),
            session_pool_policy: "single_replace".to_string(),
            max_ws_message_bytes: 8 * 1024 * 1024,
            cleanup_interval_seconds: 60,
            request_log_retention_days: 30,
            event_retention_days: 90,
            session_retention_days: 30,
            inspector_enabled_default: false,
            inspector_max_body_preview_bytes: 16 * 1024,
            admin_password_hash: None,
            admin_session_secret: None,
            reconnect_token_secret: None,
            turnstile_secret: None,
            turnstile_verify_url: "https://challenges.cloudflare.com/turnstile/v0/siteverify"
                .to_string(),
            rate_limit_per_ip: 60,
            per_tunnel_rate_limit_per_minute: 0,
            admin_login_failure_limit: 10,
            admin_login_cooldown_seconds: 60,
            metrics_public: false,
            metrics_bearer_token_hash: None,
            public_tunnel_create_enabled: true,
            tunnel_create_bearer_token_hash: None,
            max_active_tunnels_per_ip: 0,
            reserved_subdomains: crate::subdomain::RESERVED_SUBDOMAINS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            allow_custom_subdomain: true,
            require_random_subdomain: false,
            release_repo: "Xiechengqi/http-tunnel".to_string(),
            release_tag: "latest".to_string(),
            auto_upgrade_enabled: false,
            systemd_unit: None,
        }
    }
}

impl ServerConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut cfg = if path.exists() {
            let raw = fs::read_to_string(path)
                .map_err(|e| CommonError::Config(format!("read {}: {e}", path.display())))?;
            toml::from_str(&raw)
                .map_err(|e| CommonError::Config(format!("parse {}: {e}", path.display())))?
        } else {
            Self::default()
        };
        cfg.apply_env()?;
        Ok(cfg)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CommonError::Config(format!("create config dir {}: {e}", parent.display()))
            })?;
        }
        let raw = toml::to_string_pretty(self)
            .map_err(|e| CommonError::Config(format!("serialize config: {e}")))?;
        fs::write(path, raw)
            .map_err(|e| CommonError::Config(format!("write {}: {e}", path.display())))
    }

    pub fn setup_required(&self) -> bool {
        self.admin_password_hash.is_none()
            || self.domain.as_deref().unwrap_or_default().is_empty()
            || self.public_scheme.is_empty()
            || self.database_url.is_empty()
    }

    pub fn public_url(&self, subdomain: &str) -> Option<String> {
        self.domain
            .as_ref()
            .map(|domain| format!("{}://{}.{}", self.public_scheme, subdomain, domain))
    }

    fn apply_env(&mut self) -> Result<()> {
        if let Ok(v) = env::var("HTTP_TUNNEL_DOMAIN") {
            self.domain = Some(v);
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_PUBLIC_SCHEME") {
            self.public_scheme = v;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_ADDR") {
            self.addr = v
                .parse()
                .map_err(|e| CommonError::Config(format!("invalid HTTP_TUNNEL_ADDR: {e}")))?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_TRUST_PROXY_HEADERS") {
            self.trust_proxy_headers = parse_bool("HTTP_TUNNEL_TRUST_PROXY_HEADERS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_TRUSTED_PROXY_CIDRS") {
            self.trusted_proxy_cidrs = v
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToString::to_string)
                .collect();
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_DATABASE_URL") {
            self.database_url = v;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_DATA_DIR") {
            self.data_dir = v;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_TUNNEL_TTL_SECONDS") {
            self.tunnel_ttl_seconds = parse_env("HTTP_TUNNEL_TUNNEL_TTL_SECONDS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_RESERVED_TTL_SECONDS") {
            self.reserved_ttl_seconds = parse_env("HTTP_TUNNEL_RESERVED_TTL_SECONDS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_MAX_BODY_BYTES") {
            self.max_body_bytes = parse_env("HTTP_TUNNEL_MAX_BODY_BYTES", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_MAX_HEADER_BYTES") {
            self.max_header_bytes = parse_env("HTTP_TUNNEL_MAX_HEADER_BYTES", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_MAX_CONCURRENT_STREAMS") {
            self.max_concurrent_streams = parse_env("HTTP_TUNNEL_MAX_CONCURRENT_STREAMS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_REQUEST_TIMEOUT_SECONDS") {
            self.request_timeout_seconds = parse_env("HTTP_TUNNEL_REQUEST_TIMEOUT_SECONDS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_IDLE_TIMEOUT_SECONDS") {
            self.idle_timeout_seconds = parse_env("HTTP_TUNNEL_IDLE_TIMEOUT_SECONDS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_HEARTBEAT_INTERVAL_SECONDS") {
            self.heartbeat_interval_seconds =
                parse_env("HTTP_TUNNEL_HEARTBEAT_INTERVAL_SECONDS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_STALE_SESSION_SECONDS") {
            self.stale_session_seconds = parse_env("HTTP_TUNNEL_STALE_SESSION_SECONDS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_DUPLICATE_SESSION_POLICY") {
            self.duplicate_session_policy = v;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_SESSION_POOL_POLICY") {
            self.session_pool_policy = v;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_MAX_WS_MESSAGE_BYTES") {
            self.max_ws_message_bytes = parse_env("HTTP_TUNNEL_MAX_WS_MESSAGE_BYTES", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_CLEANUP_INTERVAL_SECONDS") {
            self.cleanup_interval_seconds = parse_env("HTTP_TUNNEL_CLEANUP_INTERVAL_SECONDS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_REQUEST_LOG_RETENTION_DAYS") {
            self.request_log_retention_days =
                parse_env("HTTP_TUNNEL_REQUEST_LOG_RETENTION_DAYS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_EVENT_RETENTION_DAYS") {
            self.event_retention_days = parse_env("HTTP_TUNNEL_EVENT_RETENTION_DAYS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_SESSION_RETENTION_DAYS") {
            self.session_retention_days = parse_env("HTTP_TUNNEL_SESSION_RETENTION_DAYS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_INSPECTOR_ENABLED_DEFAULT") {
            self.inspector_enabled_default =
                parse_bool("HTTP_TUNNEL_INSPECTOR_ENABLED_DEFAULT", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_INSPECTOR_MAX_BODY_PREVIEW_BYTES") {
            self.inspector_max_body_preview_bytes =
                parse_env("HTTP_TUNNEL_INSPECTOR_MAX_BODY_PREVIEW_BYTES", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_ADMIN_PASSWORD_HASH") {
            self.admin_password_hash = Some(v);
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_ADMIN_SESSION_SECRET") {
            self.admin_session_secret = Some(v);
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_RECONNECT_TOKEN_SECRET") {
            self.reconnect_token_secret = Some(v);
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_TURNSTILE_SECRET") {
            self.turnstile_secret = Some(v);
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_TURNSTILE_VERIFY_URL") {
            self.turnstile_verify_url = v;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_RATE_LIMIT_PER_IP") {
            self.rate_limit_per_ip = parse_env("HTTP_TUNNEL_RATE_LIMIT_PER_IP", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_PER_TUNNEL_RATE_LIMIT_PER_MINUTE") {
            self.per_tunnel_rate_limit_per_minute =
                parse_env("HTTP_TUNNEL_PER_TUNNEL_RATE_LIMIT_PER_MINUTE", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_ADMIN_LOGIN_FAILURE_LIMIT") {
            self.admin_login_failure_limit =
                parse_env("HTTP_TUNNEL_ADMIN_LOGIN_FAILURE_LIMIT", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_ADMIN_LOGIN_COOLDOWN_SECONDS") {
            self.admin_login_cooldown_seconds =
                parse_env("HTTP_TUNNEL_ADMIN_LOGIN_COOLDOWN_SECONDS", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_METRICS_PUBLIC") {
            self.metrics_public = parse_bool("HTTP_TUNNEL_METRICS_PUBLIC", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_METRICS_BEARER_TOKEN_HASH") {
            self.metrics_bearer_token_hash = Some(v);
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_PUBLIC_TUNNEL_CREATE_ENABLED") {
            self.public_tunnel_create_enabled =
                parse_bool("HTTP_TUNNEL_PUBLIC_TUNNEL_CREATE_ENABLED", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_CREATE_BEARER_TOKEN_HASH") {
            self.tunnel_create_bearer_token_hash = Some(v);
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_MAX_ACTIVE_TUNNELS_PER_IP") {
            self.max_active_tunnels_per_ip =
                parse_env("HTTP_TUNNEL_MAX_ACTIVE_TUNNELS_PER_IP", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_RESERVED_SUBDOMAINS") {
            self.reserved_subdomains = v
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToString::to_string)
                .collect();
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_ALLOW_CUSTOM_SUBDOMAIN") {
            self.allow_custom_subdomain = parse_bool("HTTP_TUNNEL_ALLOW_CUSTOM_SUBDOMAIN", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_REQUIRE_RANDOM_SUBDOMAIN") {
            self.require_random_subdomain = parse_bool("HTTP_TUNNEL_REQUIRE_RANDOM_SUBDOMAIN", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_RELEASE_REPO") {
            self.release_repo = v;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_RELEASE_TAG") {
            self.release_tag = v;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_AUTO_UPGRADE_ENABLED") {
            self.auto_upgrade_enabled = parse_bool("HTTP_TUNNEL_AUTO_UPGRADE_ENABLED", &v)?;
        }
        if let Ok(v) = env::var("HTTP_TUNNEL_SYSTEMD_UNIT") {
            self.systemd_unit = Some(v);
        }
        Ok(())
    }
}

fn parse_env<T>(name: &str, value: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|e| CommonError::Config(format!("invalid {name}: {e}")))
}

fn parse_bool(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(CommonError::Config(format!("invalid {name}: {value}"))),
    }
}

pub fn config_path(cli_path: Option<String>) -> String {
    cli_path
        .or_else(|| env::var("HTTP_TUNNEL_CONFIG").ok())
        .unwrap_or_else(|| default_server_config_path().display().to_string())
}

pub fn default_home_dir() -> PathBuf {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".http-tunnel"))
        .unwrap_or_else(|| PathBuf::from(".http-tunnel"))
}

pub fn default_server_config_path() -> PathBuf {
    default_home_dir().join("server.toml")
}

pub fn default_client_config_path() -> PathBuf {
    default_home_dir().join("client.toml")
}

pub fn default_database_url() -> String {
    format!(
        "sqlite://{}",
        default_home_dir().join("http-tunnel.sqlite3").display()
    )
}

pub fn default_data_dir() -> String {
    default_home_dir().display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::{Mutex, OnceLock},
    };

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn default_paths_live_under_home_http_tunnel() {
        let _guard = env_lock().lock().unwrap();
        let original_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", "/tmp/http-tunnel-home-test");
        }

        assert_eq!(
            default_server_config_path(),
            PathBuf::from("/tmp/http-tunnel-home-test/.http-tunnel/server.toml")
        );
        assert_eq!(
            default_client_config_path(),
            PathBuf::from("/tmp/http-tunnel-home-test/.http-tunnel/client.toml")
        );
        assert_eq!(
            default_database_url(),
            "sqlite:///tmp/http-tunnel-home-test/.http-tunnel/http-tunnel.sqlite3"
        );
        assert_eq!(
            ServerConfig::default().data_dir,
            "/tmp/http-tunnel-home-test/.http-tunnel"
        );

        unsafe {
            match original_home {
                Some(home) => env::set_var("HOME", home),
                None => env::remove_var("HOME"),
            }
        }
    }
}
