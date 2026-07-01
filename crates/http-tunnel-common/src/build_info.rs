use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    pub commit: String,
    pub commit_message: String,
    pub build_time: String,
    pub target: String,
}

impl BuildInfo {
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            commit: option_env!("HTTP_TUNNEL_COMMIT")
                .unwrap_or("unknown")
                .to_string(),
            commit_message: option_env!("HTTP_TUNNEL_COMMIT_MESSAGE")
                .unwrap_or("unknown")
                .to_string(),
            build_time: option_env!("HTTP_TUNNEL_BUILD_TIME")
                .unwrap_or("unknown")
                .to_string(),
            target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        }
    }
}
