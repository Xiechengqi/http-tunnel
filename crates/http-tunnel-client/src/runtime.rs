use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RuntimeStatus {
    pub pid: u32,
    pub server: Option<String>,
    pub target: Option<String>,
    pub tunnel_id: Option<String>,
    pub public_url: Option<String>,
    pub connected: bool,
    pub active_streams: usize,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub last_disconnect_reason: Option<String>,
    #[serde(default)]
    pub stale: bool,
    pub updated_at_unix: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeCleanResult {
    pub status_removed: bool,
    pub disconnect_flag_removed: bool,
}

impl RuntimeStatus {
    pub fn new(
        server: impl Into<String>,
        target: impl Into<String>,
        tunnel_id: impl Into<String>,
        public_url: impl Into<String>,
    ) -> Self {
        Self {
            pid: std::process::id(),
            server: Some(server.into()),
            target: Some(target.into()),
            tunnel_id: Some(tunnel_id.into()),
            public_url: Some(public_url.into()),
            connected: false,
            updated_at_unix: unix_now(),
            ..Self::default()
        }
    }

    pub fn mark_updated(&mut self) {
        self.updated_at_unix = unix_now();
    }
}

pub fn read_status() -> anyhow::Result<Option<RuntimeStatus>> {
    let path = runtime_status_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read runtime status {}", path.display()))?;
    let mut status: RuntimeStatus = serde_json::from_str(&raw)
        .with_context(|| format!("parse runtime status {}", path.display()))?;
    decorate_status(&mut status);
    Ok(Some(status))
}

pub fn write_status(status: &RuntimeStatus) -> anyhow::Result<()> {
    let path = runtime_status_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create runtime directory {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(status)?)
        .with_context(|| format!("write runtime status {}", path.display()))
}

pub fn request_disconnect() -> anyhow::Result<PathBuf> {
    let path = disconnect_flag_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create runtime directory {}", parent.display()))?;
    }
    std::fs::write(&path, std::process::id().to_string())
        .with_context(|| format!("write disconnect flag {}", path.display()))?;
    Ok(path)
}

pub fn disconnect_requested() -> bool {
    disconnect_flag_path().exists()
}

pub fn clear_disconnect_request() {
    let _ = std::fs::remove_file(disconnect_flag_path());
}

pub fn clean_runtime(force: bool) -> anyhow::Result<RuntimeCleanResult> {
    if let Some(status) = read_status()? {
        if status.connected && !status.stale && !force {
            anyhow::bail!(
                "runtime appears active for pid {}; use --force to remove it",
                status.pid
            );
        }
    }

    let status_path = runtime_status_path();
    let disconnect_path = disconnect_flag_path();
    let status_removed = remove_file_if_exists(&status_path)
        .with_context(|| format!("remove runtime status {}", status_path.display()))?;
    let disconnect_flag_removed = remove_file_if_exists(&disconnect_path)
        .with_context(|| format!("remove disconnect flag {}", disconnect_path.display()))?;
    Ok(RuntimeCleanResult {
        status_removed,
        disconnect_flag_removed,
    })
}

fn remove_file_if_exists(path: &PathBuf) -> anyhow::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn decorate_status(status: &mut RuntimeStatus) {
    status.stale = status.pid != 0 && !pid_alive(status.pid);
    if status.stale {
        status.connected = false;
        if status.last_disconnect_reason.is_none() {
            status.last_disconnect_reason = Some("stale_runtime".to_string());
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let proc_root = PathBuf::from("/proc");
    if proc_root.exists() {
        return proc_root.join(pid.to_string()).exists();
    }
    true
}

fn runtime_status_path() -> PathBuf {
    runtime_dir().join("runtime.json")
}

fn disconnect_flag_path() -> PathBuf {
    runtime_dir().join("disconnect")
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/http-tunnel"))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
