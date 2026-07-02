use anyhow::Context;
use http_tunnel_common::{config::default_home_dir, token::hash_token};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeInstance {
    pub pid: u32,
    pub server: String,
    pub target: String,
    pub subdomain: String,
    pub created_at_unix: u64,
}

#[derive(Debug)]
pub enum RuntimeInstanceLock {
    Acquired(InstanceLockGuard),
    Active(RuntimeInstance),
    Skipped,
}

#[derive(Debug)]
pub struct InstanceLockGuard {
    path: PathBuf,
}

impl Drop for InstanceLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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

pub fn acquire_instance_lock(
    server: &str,
    target: &str,
    subdomain: Option<&str>,
) -> anyhow::Result<RuntimeInstanceLock> {
    let Some(subdomain) = subdomain
        .map(str::trim)
        .filter(|subdomain| !subdomain.is_empty())
    else {
        return Ok(RuntimeInstanceLock::Skipped);
    };

    let instance = RuntimeInstance {
        pid: std::process::id(),
        server: normalize_endpoint_part(server),
        target: normalize_endpoint_part(target),
        subdomain: subdomain.to_ascii_lowercase(),
        created_at_unix: unix_now(),
    };
    let path = instance_lock_path(&instance.server, &instance.target, &instance.subdomain);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create runtime lock directory {}", parent.display()))?;
    }

    for _ in 0..2 {
        match create_instance_lock_file(&path, &instance) {
            Ok(()) => return Ok(RuntimeInstanceLock::Acquired(InstanceLockGuard { path })),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                let Some(active) = read_instance_lock(&path)? else {
                    remove_stale_lock(&path)?;
                    continue;
                };
                if !instance_pid_alive(active.pid) {
                    remove_stale_lock(&path)?;
                    continue;
                }
                return Ok(RuntimeInstanceLock::Active(active));
            }
            Err(error) => return Err(error.into()),
        }
    }

    match read_instance_lock(&path)? {
        Some(active) if instance_pid_alive(active.pid) => Ok(RuntimeInstanceLock::Active(active)),
        _ => {
            create_instance_lock_file(&path, &instance)?;
            Ok(RuntimeInstanceLock::Acquired(InstanceLockGuard { path }))
        }
    }
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

fn create_instance_lock_file(path: &Path, instance: &RuntimeInstance) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let raw = serde_json::to_vec_pretty(instance).map_err(std::io::Error::other)?;
    file.write_all(&raw)
}

fn read_instance_lock(path: &Path) -> anyhow::Result<Option<RuntimeInstance>> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(serde_json::from_str(&raw).ok()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn remove_stale_lock(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn normalize_endpoint_part(value: &str) -> String {
    value.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn instance_lock_path(server: &str, target: &str, subdomain: &str) -> PathBuf {
    let key = format!("{server}\0{subdomain}\0{target}");
    let hash = hash_token(&key);
    runtime_dir()
        .join("locks")
        .join(format!("{}.lock", &hash[..32]))
}

fn instance_pid_alive(pid: u32) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    let cmdline_path = PathBuf::from("/proc").join(pid.to_string()).join("cmdline");
    if !cmdline_path.exists() {
        return true;
    }
    let Ok(raw) = fs::read(&cmdline_path) else {
        return true;
    };
    if raw.is_empty() {
        return true;
    }
    let cmdline = String::from_utf8_lossy(&raw).to_ascii_lowercase();
    cmdline.contains("http-tunnel-client") || cmdline.contains("http_tunnel_client")
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
    default_home_dir()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn instance_lock_blocks_same_endpoint_until_guard_drops() {
        let _guard = env_lock().lock().unwrap();
        let original_home = std::env::var_os("HOME");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!("http-tunnel-runtime-lock-test-{now}"));
        unsafe {
            std::env::set_var("HOME", &home);
        }

        let lock = acquire_instance_lock(
            "https://Example.com/",
            "HTTP://127.0.0.1:9999/",
            Some("Demo"),
        )
        .unwrap();
        let RuntimeInstanceLock::Acquired(lock) = lock else {
            panic!("expected first lock acquisition to succeed");
        };

        let duplicate =
            acquire_instance_lock("https://example.com", "http://127.0.0.1:9999", Some("demo"))
                .unwrap();
        let RuntimeInstanceLock::Active(active) = duplicate else {
            panic!("expected duplicate lock to report active instance");
        };
        assert_eq!(active.pid, std::process::id());
        assert_eq!(active.server, "https://example.com");
        assert_eq!(active.target, "http://127.0.0.1:9999");
        assert_eq!(active.subdomain, "demo");

        drop(lock);
        let reacquired =
            acquire_instance_lock("https://example.com", "http://127.0.0.1:9999", Some("demo"))
                .unwrap();
        assert!(matches!(reacquired, RuntimeInstanceLock::Acquired(_)));

        unsafe {
            match original_home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(home);
    }
}
