use super::*;
use axum::{
    extract::{
        connect_info::ConnectInfo,
        ws::{Message, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use http_tunnel_common::{api::ApiResponse, ServerConfig};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RestartResponse {
    pub attempted: bool,
    pub method: Option<String>,
    pub available_methods: Vec<&'static str>,
    pub restart_method_checks: Vec<RestartMethodCheck>,
    pub pending_restart: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestartMethodCheck {
    pub method: &'static str,
    pub available: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct UpgradeValidationResponse {
    pub release_repo: String,
    pub release_tag: String,
    pub asset_name: String,
    pub resolved_tag: Option<String>,
    pub asset_url: Option<String>,
    pub current_exe: Option<String>,
    pub current_exe_writable: bool,
    pub backup_parent_writable: bool,
    pub temp_dir_writable: bool,
    pub systemd_unit_configured: bool,
    pub systemd_unit_found: bool,
    pub restart_methods: Vec<&'static str>,
    pub restart_method_checks: Vec<RestartMethodCheck>,
    pub checksum_url: Option<String>,
    pub checksum_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UpgradeResponse {
    pub started: bool,
    pub message: String,
    pub asset_name: Option<String>,
    pub backup_path: Option<String>,
    pub restart_method: Option<String>,
    pub checksum_sha256: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UpgradeRequest {
    pub dry_run: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone)]
struct ReleaseAsset {
    tag_name: String,
    asset_name: String,
    download_url: String,
    checksum_url: Option<String>,
}

pub async fn restart(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<RestartResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let cfg = state.config.read().await.clone();
    let restart = request_restart(cfg.systemd_unit.as_deref())?;
    set_pending_restart(&state, false).await?;
    add_audit_event(&state, "admin_restart_requested", restart.method).await?;
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "restart",
            target_type: Some("system"),
            target_id: restart.method,
            result: "success",
            detail: Some(restart.message.as_str()),
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(RestartResponse {
        attempted: restart.attempted,
        method: restart.method.map(ToString::to_string),
        available_methods: restart_methods(cfg.systemd_unit.as_deref()),
        restart_method_checks: restart_method_checks(cfg.systemd_unit.as_deref()),
        pending_restart: false,
        message: restart.message,
    })))
}

pub async fn validate_upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<UpgradeValidationResponse>>> {
    require_admin(&state, &headers).await?;
    let cfg = state.config.read().await.clone();
    let resolved = if cfg.release_repo.trim().is_empty() {
        None
    } else {
        resolve_release_asset(&cfg).await.ok()
    };
    let checksum_sha256 = match resolved.as_ref() {
        Some(asset) => fetch_expected_checksum(asset).await.ok(),
        None => None,
    };
    Ok(Json(ApiResponse::ok(UpgradeValidationResponse {
        release_repo: cfg.release_repo,
        release_tag: cfg.release_tag,
        asset_name: server_asset_name(),
        resolved_tag: resolved.as_ref().map(|asset| asset.tag_name.clone()),
        asset_url: resolved.as_ref().map(|asset| asset.download_url.clone()),
        current_exe: std::env::current_exe()
            .ok()
            .map(|path| path.display().to_string()),
        current_exe_writable: std::env::current_exe()
            .ok()
            .is_some_and(|path| is_writable_file(&path)),
        backup_parent_writable: std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .is_some_and(|path| is_writable_dir(&path)),
        temp_dir_writable: is_writable_dir(&std::env::temp_dir()),
        systemd_unit_configured: cfg.systemd_unit.is_some(),
        systemd_unit_found: cfg.systemd_unit.as_deref().is_some_and(systemd_unit_exists),
        restart_methods: restart_methods(cfg.systemd_unit.as_deref()),
        restart_method_checks: restart_method_checks(cfg.systemd_unit.as_deref()),
        checksum_url: resolved
            .as_ref()
            .and_then(|asset| asset.checksum_url.clone()),
        checksum_sha256,
    })))
}

pub async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    payload: Option<Json<UpgradeRequest>>,
) -> Result<Json<ApiResponse<UpgradeResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let cfg = state.config.read().await.clone();
    if cfg.release_repo.trim().is_empty() {
        record_admin_audit(
            &state,
            &headers,
            remote_addr,
            AuditLog {
                actor_token: Some(&actor),
                action: "upgrade",
                target_type: Some("release"),
                target_id: Some(&cfg.release_repo),
                result: "skipped",
                detail: Some("release repository is not configured"),
            },
        )
        .await?;
        return Ok(Json(ApiResponse::ok(UpgradeResponse {
            started: false,
            message: "release repository is not configured".to_string(),
            asset_name: None,
            backup_path: None,
            restart_method: None,
            checksum_sha256: None,
        })));
    }
    emit_upgrade_event(&state, "resolving", "resolving release asset");
    let asset = resolve_release_asset(&cfg).await?;
    let expected_checksum = fetch_expected_checksum(&asset).await?;
    let current_exe = std::env::current_exe().map_err(AppError::internal)?;
    let backup_path = backup_path(&current_exe);
    if payload
        .as_ref()
        .and_then(|Json(req)| req.dry_run)
        .unwrap_or(false)
    {
        let parent = current_exe.parent().unwrap_or_else(|| Path::new("."));
        if !is_writable_file(&current_exe)
            || !is_writable_dir(parent)
            || !is_writable_dir(&std::env::temp_dir())
        {
            return Err(AppError::new(
                StatusCode::BAD_REQUEST,
                "upgrade_environment_not_writable",
                "current executable, backup directory, or temp directory is not writable",
            ));
        }
        emit_upgrade_event(&state, "dry_run_passed", "dry run passed");
        record_admin_audit(
            &state,
            &headers,
            remote_addr,
            AuditLog {
                actor_token: Some(&actor),
                action: "upgrade_dry_run",
                target_type: Some("release"),
                target_id: Some(&asset.asset_name),
                result: "success",
                detail: Some(&asset.tag_name),
            },
        )
        .await?;
        return Ok(Json(ApiResponse::ok(UpgradeResponse {
            started: false,
            message: "dry run passed; release asset and local paths are usable".to_string(),
            asset_name: Some(asset.asset_name),
            backup_path: Some(backup_path.display().to_string()),
            restart_method: None,
            checksum_sha256: Some(expected_checksum),
        })));
    }
    let tmp_path = std::env::temp_dir().join(&asset.asset_name);
    emit_upgrade_event(&state, "downloading", "downloading release asset");
    download_asset(&asset.download_url, &tmp_path).await?;
    emit_upgrade_event(&state, "verifying", "verifying downloaded binary");
    verify_file_sha256(&tmp_path, &expected_checksum)?;
    make_executable(&tmp_path)?;
    verify_binary(&tmp_path)?;
    emit_upgrade_event(&state, "replacing", "replacing current binary");
    replace_binary(&tmp_path, &current_exe, &backup_path)?;
    add_audit_event(
        &state,
        "admin_upgrade_installed",
        Some(&format!("{} from {}", asset.asset_name, asset.tag_name)),
    )
    .await?;
    record_admin_audit(
        &state,
        &headers,
        remote_addr,
        AuditLog {
            actor_token: Some(&actor),
            action: "upgrade",
            target_type: Some("release"),
            target_id: Some(&asset.asset_name),
            result: "success",
            detail: Some(&asset.tag_name),
        },
    )
    .await?;
    emit_upgrade_event(&state, "restarting", "requesting service restart");
    let restart = request_restart(cfg.systemd_unit.as_deref())?;
    add_audit_event(&state, "admin_restart_requested", restart.method).await?;
    emit_upgrade_event(&state, "complete", &restart.message);
    Ok(Json(ApiResponse::ok(UpgradeResponse {
        started: true,
        message: format!("installed {}; {}", asset.asset_name, restart.message),
        asset_name: Some(asset.asset_name),
        backup_path: Some(backup_path.display().to_string()),
        restart_method: restart.method.map(ToString::to_string),
        checksum_sha256: Some(expected_checksum),
    })))
}

pub async fn upgrade_ws(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    if let Some(token) = query.token {
        let bearer_authorized = bearer_token_valid(&state, &token).await;
        let cfg = state.config.read().await;
        let authorized = bearer_authorized
            || cfg
                .admin_session_secret
                .as_deref()
                .is_some_and(|secret| verify_session_cookie(&token, secret).is_some());
        drop(cfg);
        if !authorized {
            return Err(AppError::unauthorized());
        }
    } else {
        require_admin(&state, &headers).await?;
    }
    let mut events = state.upgrade_events.subscribe();
    Ok(ws
        .on_upgrade(|mut socket| async move {
            let _ = socket
                .send(Message::Text(upgrade_event_json(
                    "connected",
                    "upgrade log stream connected",
                )))
                .await;
            while let Ok(message) = events.recv().await {
                if socket.send(Message::Text(message)).await.is_err() {
                    break;
                }
            }
        })
        .into_response())
}

fn emit_upgrade_event(state: &AppState, level: &str, message: &str) {
    let _ = state
        .upgrade_events
        .send(upgrade_event_json(level, message));
}

fn upgrade_event_json(level: &str, message: &str) -> String {
    serde_json::json!({
        "level": level,
        "message": message,
    })
    .to_string()
}

async fn resolve_release_asset(cfg: &ServerConfig) -> Result<ReleaseAsset> {
    let release_url = if cfg.release_tag == "latest" {
        format!(
            "https://api.github.com/repos/{}/releases/latest",
            cfg.release_repo
        )
    } else {
        format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            cfg.release_repo, cfg.release_tag
        )
    };
    let client = reqwest::Client::builder()
        .user_agent(format!("http-tunnel/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(AppError::internal)?;
    let release = client
        .get(release_url)
        .send()
        .await
        .map_err(AppError::internal)?
        .error_for_status()
        .map_err(AppError::internal)?
        .json::<GitHubRelease>()
        .await
        .map_err(AppError::internal)?;
    let asset_name = server_asset_name();
    let checksum_url = select_checksum_asset(&release.assets, &asset_name)
        .map(|asset| asset.browser_download_url.clone());
    let Some(asset) = select_release_asset(release.assets, &asset_name) else {
        return Err(AppError::new(
            StatusCode::NOT_FOUND,
            "upgrade_asset_not_found",
            "matching server release asset was not found",
        ));
    };
    Ok(ReleaseAsset {
        tag_name: release.tag_name,
        asset_name,
        download_url: asset.browser_download_url,
        checksum_url,
    })
}

fn select_release_asset(assets: Vec<GitHubAsset>, asset_name: &str) -> Option<GitHubAsset> {
    assets.into_iter().find(|asset| asset.name == asset_name)
}

fn select_checksum_asset<'a>(
    assets: &'a [GitHubAsset],
    asset_name: &str,
) -> Option<&'a GitHubAsset> {
    let sidecars = [
        format!("{asset_name}.sha256"),
        format!("{asset_name}.sha256sum"),
    ];
    assets
        .iter()
        .find(|asset| sidecars.contains(&asset.name))
        .or_else(|| {
            assets.iter().find(|asset| {
                matches!(
                    asset.name.as_str(),
                    "SHA256SUMS" | "SHA256SUMS.txt" | "checksums.txt"
                )
            })
        })
}

async fn fetch_expected_checksum(asset: &ReleaseAsset) -> Result<String> {
    let Some(checksum_url) = asset.checksum_url.as_deref() else {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "upgrade_checksum_missing",
            "matching SHA256 checksum asset was not found",
        ));
    };
    let client = reqwest::Client::builder()
        .user_agent(format!("http-tunnel/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(AppError::internal)?;
    let text = client
        .get(checksum_url)
        .send()
        .await
        .map_err(AppError::internal)?
        .error_for_status()
        .map_err(AppError::internal)?
        .text()
        .await
        .map_err(AppError::internal)?;
    parse_sha256_checksum(&text, &asset.asset_name).ok_or_else(|| {
        AppError::new(
            StatusCode::BAD_REQUEST,
            "upgrade_checksum_invalid",
            "checksum asset did not contain a SHA256 for the selected server binary",
        )
    })
}

async fn download_asset(url: &str, path: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(format!("http-tunnel/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(AppError::internal)?;
    let bytes = client
        .get(url)
        .send()
        .await
        .map_err(AppError::internal)?
        .error_for_status()
        .map_err(AppError::internal)?
        .bytes()
        .await
        .map_err(AppError::internal)?;
    tokio::fs::write(path, bytes)
        .await
        .map_err(AppError::internal)?;
    Ok(())
}

fn verify_file_sha256(path: &Path, expected: &str) -> Result<()> {
    let bytes = fs::read(path).map_err(AppError::internal)?;
    let actual = sha256_hex(&bytes);
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "upgrade_checksum_mismatch",
            "downloaded binary SHA256 did not match the release checksum",
        ))
    }
}

fn parse_sha256_checksum(text: &str, asset_name: &str) -> Option<String> {
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() == 1 && valid_sha256_hex(fields[0]) {
            return Some(fields[0].to_ascii_lowercase());
        }
        if fields.len() >= 2 && valid_sha256_hex(fields[0]) {
            let filename = fields[1].trim_start_matches('*');
            if filename.ends_with(asset_name) {
                return Some(fields[0].to_ascii_lowercase());
            }
        }
    }
    None
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(AppError::internal)?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).map_err(AppError::internal)?;
    }
    Ok(())
}

fn verify_binary(path: &Path) -> Result<()> {
    let status = Command::new(path)
        .arg("--help")
        .status()
        .map_err(AppError::internal)?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "upgrade_validation_failed",
            "downloaded binary did not pass --help validation",
        ))
    }
}

fn backup_path(current_exe: &Path) -> PathBuf {
    let mut backup = current_exe.as_os_str().to_os_string();
    backup.push(".bak");
    PathBuf::from(backup)
}

fn replace_binary(new_binary: &Path, current_exe: &Path, backup: &Path) -> Result<()> {
    replace_binary_files(new_binary, current_exe, backup).map_err(AppError::internal)?;
    make_executable(current_exe)?;
    Ok(())
}

fn replace_binary_files(
    new_binary: &Path,
    current_exe: &Path,
    backup: &Path,
) -> std::io::Result<()> {
    fs::copy(current_exe, backup)?;
    if let Err(error) = fs::copy(new_binary, current_exe) {
        let _ = fs::copy(backup, current_exe);
        return Err(error);
    }
    Ok(())
}

struct RestartRequest {
    attempted: bool,
    method: Option<&'static str>,
    message: String,
}

fn request_restart(unit: Option<&str>) -> Result<RestartRequest> {
    let mut failures = Vec::new();
    if let Some(unit) = unit.filter(|unit| !unit.trim().is_empty()) {
        if command_available("systemd-run") {
            match restart_systemd_transient(unit) {
                Ok(()) => {
                    return Ok(RestartRequest {
                        attempted: true,
                        method: Some("systemd-run"),
                        message: format!("restart scheduled for {unit} through systemd-run"),
                    });
                }
                Err(error) => failures.push(format!("systemd-run: {error:?}")),
            }
        }
        if command_available("systemctl") {
            match restart_systemd_unit(unit) {
                Ok(()) => {
                    return Ok(RestartRequest {
                        attempted: true,
                        method: Some("systemctl"),
                        message: format!("restart requested for {unit} through systemctl"),
                    });
                }
                Err(error) => failures.push(format!("systemctl: {error:?}")),
            }
        }
    }
    if exec_restart_supported() {
        schedule_exec_restart()?;
        let detail = if failures.is_empty() {
            "exec restart scheduled with current process arguments".to_string()
        } else {
            format!(
                "exec restart scheduled after service-manager fallback(s) failed: {}",
                failures.join("; ")
            )
        };
        return Ok(RestartRequest {
            attempted: true,
            method: Some("exec"),
            message: detail,
        });
    }
    if failures.is_empty() {
        Ok(RestartRequest {
            attempted: false,
            method: None,
            message: "no restart method is available; restart externally".to_string(),
        })
    } else {
        Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "restart_failed",
            failures.join("; "),
        ))
    }
}

fn restart_methods(unit: Option<&str>) -> Vec<&'static str> {
    restart_method_checks(unit)
        .into_iter()
        .filter_map(|check| check.available.then_some(check.method))
        .collect()
}

fn restart_method_checks(unit: Option<&str>) -> Vec<RestartMethodCheck> {
    let mut methods = Vec::new();
    if unit.is_some_and(|unit| !unit.trim().is_empty()) {
        let systemd_run = command_availability("systemd-run");
        methods.push(RestartMethodCheck {
            method: "systemd-run",
            available: systemd_run.available,
            detail: systemd_run.detail,
        });
        let systemctl = command_availability("systemctl");
        methods.push(RestartMethodCheck {
            method: "systemctl",
            available: systemctl.available,
            detail: systemctl.detail,
        });
    }
    methods.push(RestartMethodCheck {
        method: "exec",
        available: exec_restart_supported(),
        detail: if exec_restart_supported() {
            "exec restart is supported on this platform".to_string()
        } else {
            "exec restart is not supported on this platform".to_string()
        },
    });
    methods
}

fn restart_systemd_transient(unit: &str) -> Result<()> {
    let transient_unit = format!("http-tunnel-restart-{}", std::process::id());
    let status = Command::new("systemd-run")
        .arg("--unit")
        .arg(transient_unit)
        .arg("--on-active=1s")
        .arg("systemctl")
        .arg("restart")
        .arg("--no-block")
        .arg(unit)
        .status()
        .map_err(AppError::internal)?;
    if status.success() {
        Ok(())
    } else {
        Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "restart_failed",
            "systemd-run restart scheduling failed",
        ))
    }
}

fn restart_systemd_unit(unit: &str) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("restart")
        .arg("--no-block")
        .arg(unit)
        .status()
        .map_err(AppError::internal)?;
    if !status.success() {
        return Err(AppError::new(
            StatusCode::BAD_GATEWAY,
            "restart_failed",
            "systemctl restart failed",
        ));
    }
    Ok(())
}

fn command_available(command: &str) -> bool {
    command_availability(command).available
}

struct CommandAvailability {
    available: bool,
    detail: String,
}

fn command_availability(command: &str) -> CommandAvailability {
    command_availability_from_result(command, Command::new(command).arg("--version").output())
}

fn command_availability_from_result(
    command: &str,
    result: io::Result<Output>,
) -> CommandAvailability {
    match result {
        Ok(output) if output.status.success() => CommandAvailability {
            available: true,
            detail: format!("{command} --version succeeded"),
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = stderr.trim();
            let stdout = stdout.trim();
            let detail = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                "no output"
            };
            CommandAvailability {
                available: false,
                detail: format!(
                    "{command} --version exited with {}: {detail}",
                    output.status
                ),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => CommandAvailability {
            available: false,
            detail: format!("{command} was not found in PATH"),
        },
        Err(error) => CommandAvailability {
            available: false,
            detail: format!("{command} --version failed to start: {error}"),
        },
    }
}

#[cfg(unix)]
fn exec_restart_supported() -> bool {
    true
}

#[cfg(not(unix))]
fn exec_restart_supported() -> bool {
    false
}

#[cfg(unix)]
fn schedule_exec_restart() -> Result<()> {
    let current_exe = std::env::current_exe().map_err(AppError::internal)?;
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(350));
        use std::os::unix::process::CommandExt;
        let error = Command::new(current_exe).args(args).exec();
        eprintln!("http-tunnel exec restart failed: {error}");
    });
    Ok(())
}

#[cfg(not(unix))]
fn schedule_exec_restart() -> Result<()> {
    Err(AppError::new(
        StatusCode::BAD_GATEWAY,
        "restart_not_supported",
        "exec restart is not supported on this platform",
    ))
}

fn is_writable_file(path: &Path) -> bool {
    fs::OpenOptions::new().write(true).open(path).is_ok()
}

fn is_writable_dir(path: &Path) -> bool {
    let probe = path.join(format!(".http-tunnel-write-test-{}", std::process::id()));
    match fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn systemd_unit_exists(unit: &str) -> bool {
    Command::new("systemctl")
        .arg("status")
        .arg(unit)
        .arg("--no-pager")
        .status()
        .is_ok_and(|status| status.success())
}

fn server_asset_name() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("http-tunnel-server-linux-{arch}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn selects_release_asset_by_exact_name() {
        let assets = vec![
            GitHubAsset {
                name: "http-tunnel-client-linux-amd64".to_string(),
                browser_download_url: "client".to_string(),
            },
            GitHubAsset {
                name: "http-tunnel-server-linux-amd64".to_string(),
                browser_download_url: "server".to_string(),
            },
        ];
        let asset = select_release_asset(assets, "http-tunnel-server-linux-amd64").unwrap();
        assert_eq!(asset.browser_download_url, "server");
    }

    #[test]
    fn selects_and_parses_sha256_checksum() {
        let assets = vec![
            GitHubAsset {
                name: "SHA256SUMS".to_string(),
                browser_download_url: "checksums".to_string(),
            },
            GitHubAsset {
                name: "http-tunnel-server-linux-amd64".to_string(),
                browser_download_url: "server".to_string(),
            },
        ];
        let checksum = select_checksum_asset(&assets, "http-tunnel-server-linux-amd64").unwrap();
        assert_eq!(checksum.browser_download_url, "checksums");

        let hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            parse_sha256_checksum(
                &format!("{hash}  http-tunnel-server-linux-amd64\n"),
                "http-tunnel-server-linux-amd64",
            )
            .as_deref(),
            Some(hash)
        );
        assert!(parse_sha256_checksum(
            &format!("{hash}  http-tunnel-client-linux-amd64\n"),
            "http-tunnel-server-linux-amd64",
        )
        .is_none());
    }

    #[test]
    fn backup_path_appends_bak() {
        assert_eq!(
            backup_path(Path::new("/tmp/http-tunnel-server")),
            PathBuf::from("/tmp/http-tunnel-server.bak")
        );
    }

    #[test]
    fn replace_binary_files_creates_backup_and_replaces_current() {
        let dir = temp_test_dir("replace-ok");
        let current = dir.join("current");
        let new_binary = dir.join("new");
        let backup = dir.join("current.bak");
        fs::write(&current, "old").unwrap();
        fs::write(&new_binary, "new").unwrap();

        replace_binary_files(&new_binary, &current, &backup).unwrap();

        assert_eq!(fs::read_to_string(&current).unwrap(), "new");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "old");
    }

    #[test]
    fn replace_binary_files_rolls_back_when_new_binary_is_missing() {
        let dir = temp_test_dir("replace-rollback");
        let current = dir.join("current");
        let new_binary = dir.join("missing");
        let backup = dir.join("current.bak");
        fs::write(&current, "old").unwrap();

        assert!(replace_binary_files(&new_binary, &current, &backup).is_err());

        assert_eq!(fs::read_to_string(&current).unwrap(), "old");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "old");
    }

    #[test]
    fn writable_dir_probe_reports_existing_temp_dir() {
        let dir = temp_test_dir("writable-dir");
        assert!(is_writable_dir(&dir));
        assert!(!is_writable_dir(&dir.join("missing")));
    }

    #[cfg(unix)]
    #[test]
    fn command_availability_requires_successful_version_probe() {
        let ok = command_availability_from_result(
            "tool",
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: b"tool 1.0".to_vec(),
                stderr: Vec::new(),
            }),
        );
        assert!(ok.available);

        let failed = command_availability_from_result(
            "tool",
            Ok(Output {
                status: std::process::ExitStatus::from_raw(256),
                stdout: Vec::new(),
                stderr: b"failed".to_vec(),
            }),
        );
        assert!(!failed.available);
        assert!(failed.detail.contains("failed"));
    }

    #[test]
    fn restart_method_checks_include_exec_without_systemd_unit() {
        let checks = restart_method_checks(None);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].method, "exec");
    }

    fn temp_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("http-tunnel-{name}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
