use anyhow::Context;
use http_tunnel_common::config::default_client_config_path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ClientConfig {
    pub server: Option<String>,
    pub target: Option<String>,
    pub subdomain: Option<String>,
    pub tunnel_id: Option<String>,
    pub token: Option<String>,
    pub url: Option<String>,
    pub create_token: Option<String>,
    pub persist_token: Option<bool>,
    pub public_ip_lookup_urls: Option<Vec<String>>,
    pub public_ip_refresh_seconds: Option<u64>,
}

pub fn init_config_file() -> anyhow::Result<std::path::PathBuf> {
    let path = default_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config directory {}", parent.display()))?;
    }
    if !path.exists() {
        std::fs::write(
            &path,
            "server = \"https://example.com\"\nsubdomain = \"demo\"\ntarget = \"http://127.0.0.1:3000\"\npersist_token = true\n",
        )
        .with_context(|| format!("write config {}", path.display()))?;
    }
    Ok(path)
}

pub fn load_config_file() -> anyhow::Result<ClientConfig> {
    let path = default_config_path();
    if !path.exists() {
        return Ok(ClientConfig::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read config {}", path.display()))?;
    parse_client_config(&raw).with_context(|| format!("parse config {}", path.display()))
}

pub fn save_config_file(cfg: &ClientConfig) -> anyhow::Result<()> {
    let path = default_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create config directory {}", parent.display()))?;
    }
    let raw = toml::to_string_pretty(cfg).context("serialize client config")?;
    std::fs::write(&path, raw).with_context(|| format!("write config {}", path.display()))
}

pub fn clear_stored_tunnel(cfg: &mut ClientConfig) {
    cfg.tunnel_id = None;
    cfg.token = None;
    cfg.url = None;
}

pub fn clear_stored_tunnel_on_endpoint_override(
    cfg: &mut ClientConfig,
    explicit_server: bool,
    old_server: Option<&str>,
    server: &str,
    explicit_subdomain: bool,
    old_subdomain: Option<&str>,
    subdomain: Option<&str>,
) {
    let server_changed = explicit_server && old_server.is_some_and(|old| old != server);
    let subdomain_changed = explicit_subdomain && old_subdomain != subdomain;
    if server_changed || subdomain_changed {
        clear_stored_tunnel(cfg);
    }
}

pub fn default_config_path() -> std::path::PathBuf {
    default_client_config_path()
}

fn parse_client_config(raw: &str) -> anyhow::Result<ClientConfig> {
    toml::from_str(raw).context("parse client config")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_client_config() {
        let cfg = parse_client_config(
            r#"
server = "https://example.com"
target = "http://127.0.0.1:3000"
subdomain = "demo"
tunnel_id = "tun_123"
token = "secret"
url = "https://demo.example.com"
create_token = "create-secret"
persist_token = false
public_ip_lookup_urls = ["http://3.0.3.0", "https://api64.ipify.org?format=json"]
public_ip_refresh_seconds = 3600
"#,
        )
        .unwrap();
        assert_eq!(cfg.server.as_deref(), Some("https://example.com"));
        assert_eq!(cfg.target.as_deref(), Some("http://127.0.0.1:3000"));
        assert_eq!(cfg.subdomain.as_deref(), Some("demo"));
        assert_eq!(cfg.tunnel_id.as_deref(), Some("tun_123"));
        assert_eq!(cfg.token.as_deref(), Some("secret"));
        assert_eq!(cfg.url.as_deref(), Some("https://demo.example.com"));
        assert_eq!(cfg.create_token.as_deref(), Some("create-secret"));
        assert_eq!(cfg.persist_token, Some(false));
        assert_eq!(
            cfg.public_ip_lookup_urls.as_ref().unwrap(),
            &vec![
                "http://3.0.3.0".to_string(),
                "https://api64.ipify.org?format=json".to_string()
            ]
        );
        assert_eq!(cfg.public_ip_refresh_seconds, Some(3600));
    }

    #[test]
    fn endpoint_override_clears_stored_tunnel() {
        let mut cfg = ClientConfig {
            server: Some("https://old.example.com".to_string()),
            subdomain: Some("old".to_string()),
            tunnel_id: Some("tun_1".to_string()),
            token: Some("secret".to_string()),
            url: Some("https://old.old.example.com".to_string()),
            ..ClientConfig::default()
        };

        clear_stored_tunnel_on_endpoint_override(
            &mut cfg,
            true,
            Some("https://old.example.com"),
            "https://new.example.com",
            false,
            Some("old"),
            Some("old"),
        );

        assert!(cfg.tunnel_id.is_none());
        assert!(cfg.token.is_none());
        assert!(cfg.url.is_none());
    }
}
