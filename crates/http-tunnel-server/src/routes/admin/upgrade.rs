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
use sqlx::Row;
use std::{
    cmp::Ordering as VersionOrdering,
    fs, io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

const DEFAULT_RELEASE_REPO: &str = "Xiechengqi/http-tunnel";
const AUTO_UPGRADE_CHECK_INTERVAL: Duration = Duration::from_secs(300);
const AUTO_UPGRADE_IDLE_WINDOW: Duration = Duration::from_secs(10);

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
pub struct UpgradeStatus {
    pub auto_upgrade_enabled: bool,
    pub release_repo: String,
    pub effective_release_repo: String,
    pub release_tag: String,
    pub current_version: String,
    pub check_interval_seconds: u64,
    pub idle_window_seconds: u64,
    pub upgrade_in_progress: bool,
    pub restart_methods: Vec<&'static str>,
    pub restart_method_checks: Vec<RestartMethodCheck>,
    pub last_checked_at: Option<String>,
    pub last_result: Option<String>,
    pub last_message: Option<String>,
    pub latest_tag: Option<String>,
    pub update_available: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct UpgradeResponse {
    pub started: bool,
    pub message: String,
    pub current_version: String,
    pub resolved_tag: Option<String>,
    pub update_available: bool,
    pub asset_name: Option<String>,
    pub backup_path: Option<String>,
    pub restart_method: Option<String>,
    pub checksum_sha256: Option<String>,
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

struct UpgradeCandidate {
    asset: ReleaseAsset,
    expected_checksum: String,
    current_exe: PathBuf,
    backup_path: PathBuf,
    update_available: bool,
}

pub async fn restart(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<RestartResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let cfg = state.config.read().await.clone();
    let restart = request_restart(cfg.systemd_unit.as_deref(), None)?;
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

pub async fn upgrade_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<UpgradeStatus>>> {
    require_admin(&state, &headers).await?;
    let cfg = state.config.read().await.clone();
    let upgrade_in_progress = match state.upgrade_lock.try_lock() {
        Ok(guard) => {
            drop(guard);
            false
        }
        Err(_) => true,
    };
    Ok(Json(ApiResponse::ok(UpgradeStatus {
        auto_upgrade_enabled: cfg.auto_upgrade_enabled,
        release_repo: cfg.release_repo.clone(),
        effective_release_repo: effective_release_repo(&cfg),
        release_tag: cfg.release_tag,
        current_version: current_version(),
        check_interval_seconds: AUTO_UPGRADE_CHECK_INTERVAL.as_secs(),
        idle_window_seconds: AUTO_UPGRADE_IDLE_WINDOW.as_secs(),
        upgrade_in_progress,
        restart_methods: restart_methods(cfg.systemd_unit.as_deref()),
        restart_method_checks: restart_method_checks(cfg.systemd_unit.as_deref()),
        last_checked_at: upgrade_setting(&state, "auto_upgrade_last_checked_at").await?,
        last_result: upgrade_setting(&state, "auto_upgrade_last_result").await?,
        last_message: upgrade_setting(&state, "auto_upgrade_last_message").await?,
        latest_tag: upgrade_setting(&state, "auto_upgrade_latest_tag").await?,
        update_available: upgrade_setting(&state, "auto_upgrade_update_available")
            .await?
            .and_then(|value| match value.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }),
    })))
}

pub async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
) -> Result<Json<ApiResponse<UpgradeResponse>>> {
    let actor = require_admin_write(&state, &headers).await?;
    let cfg = state.config.read().await.clone();
    let Ok(_guard) = state.upgrade_lock.try_lock() else {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "upgrade_in_progress",
            "another upgrade is already running",
        ));
    };
    emit_upgrade_event(&state, "resolving", "resolving release asset");
    let candidate = prepare_upgrade_candidate(&cfg).await?;
    if !candidate.update_available {
        record_admin_audit(
            &state,
            &headers,
            remote_addr,
            AuditLog {
                actor_token: Some(&actor),
                action: "upgrade",
                target_type: Some("release"),
                target_id: Some(&candidate.asset.asset_name),
                result: "skipped",
                detail: Some("current server binary already matches the selected release"),
            },
        )
        .await?;
        return Ok(Json(ApiResponse::ok(UpgradeResponse {
            started: false,
            message: "current server binary already matches the selected release".to_string(),
            current_version: current_version(),
            resolved_tag: Some(candidate.asset.tag_name),
            update_available: false,
            asset_name: Some(candidate.asset.asset_name),
            backup_path: None,
            restart_method: None,
            checksum_sha256: Some(candidate.expected_checksum),
        })));
    }
    let response = install_upgrade_candidate(
        &state,
        &cfg,
        candidate,
        "admin_upgrade_installed",
        "admin_restart_requested",
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
            target_id: response.asset_name.as_deref(),
            result: "success",
            detail: response.resolved_tag.as_deref(),
        },
    )
    .await?;
    Ok(Json(ApiResponse::ok(response)))
}

pub(crate) fn spawn_auto_upgrade_job(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(error) = auto_upgrade_once(&state).await {
                tracing::warn!(%error, "automatic upgrade check failed");
                let _ = record_auto_upgrade_status(
                    &state,
                    "error",
                    &format!("automatic upgrade check failed: {error}"),
                    None,
                    None,
                )
                .await;
            }
            tokio::time::sleep(AUTO_UPGRADE_CHECK_INTERVAL).await;
        }
    });
}

async fn auto_upgrade_once(state: &AppState) -> Result<()> {
    let cfg = state.config.read().await.clone();
    if !cfg.auto_upgrade_enabled {
        record_auto_upgrade_status(
            state,
            "disabled",
            "automatic upgrade is disabled",
            None,
            None,
        )
        .await?;
        return Ok(());
    }
    if cfg.setup_required() {
        record_auto_upgrade_status(state, "skipped", "setup is not complete", None, None).await?;
        return Ok(());
    }
    if cfg.release_tag.trim() != "latest" {
        record_auto_upgrade_status(
            state,
            "skipped",
            "automatic upgrade only tracks the latest release tag",
            None,
            None,
        )
        .await?;
        return Ok(());
    }

    let candidate = prepare_upgrade_candidate(&cfg).await?;
    if !candidate.update_available {
        let message = format!(
            "no newer release found; current={} latest={}",
            current_version(),
            candidate.asset.tag_name
        );
        record_auto_upgrade_status(
            state,
            "no_update",
            &message,
            Some(candidate.asset.tag_name.as_str()),
            Some(false),
        )
        .await?;
        return Ok(());
    }

    let waiting_message = format!(
        "new release {} is available; waiting for {}s without tunnel proxy traffic",
        candidate.asset.tag_name,
        AUTO_UPGRADE_IDLE_WINDOW.as_secs()
    );
    record_auto_upgrade_status(
        state,
        "waiting_idle",
        &waiting_message,
        Some(candidate.asset.tag_name.as_str()),
        Some(true),
    )
    .await?;
    if !wait_for_auto_upgrade_idle(state).await {
        return Ok(());
    }

    let Ok(_guard) = state.upgrade_lock.try_lock() else {
        record_auto_upgrade_status(
            state,
            "skipped",
            "another upgrade is already running",
            Some(candidate.asset.tag_name.as_str()),
            Some(true),
        )
        .await?;
        return Ok(());
    };

    let cfg = state.config.read().await.clone();
    if !cfg.auto_upgrade_enabled || cfg.release_tag.trim() != "latest" {
        record_auto_upgrade_status(
            state,
            "skipped",
            "automatic upgrade was disabled or retargeted while waiting for idle traffic",
            None,
            None,
        )
        .await?;
        return Ok(());
    }
    let candidate = prepare_upgrade_candidate(&cfg).await?;
    if !candidate.update_available {
        record_auto_upgrade_status(
            state,
            "no_update",
            "selected release already matches current server binary after idle wait",
            Some(candidate.asset.tag_name.as_str()),
            Some(false),
        )
        .await?;
        return Ok(());
    }

    let response = install_upgrade_candidate(
        state,
        &cfg,
        candidate,
        "auto_upgrade_installed",
        "auto_restart_requested",
    )
    .await?;
    record_auto_upgrade_status(
        state,
        "installed",
        &response.message,
        response.resolved_tag.as_deref(),
        Some(true),
    )
    .await?;
    Ok(())
}

async fn wait_for_auto_upgrade_idle(state: &AppState) -> bool {
    loop {
        let cfg = state.config.read().await.clone();
        if !cfg.auto_upgrade_enabled || cfg.release_tag.trim() != "latest" {
            return false;
        }
        if state.proxy_idle_for(AUTO_UPGRADE_IDLE_WINDOW).await {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
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

async fn prepare_upgrade_candidate(cfg: &ServerConfig) -> Result<UpgradeCandidate> {
    let asset = resolve_release_asset(cfg).await?;
    let expected_checksum = fetch_expected_checksum(&asset).await?;
    let current_exe = std::env::current_exe().map_err(AppError::internal)?;
    let backup_path = backup_path(&current_exe);
    let current_checksum = sha256_file(&current_exe)?;
    let update_available = update_available(
        current_version().as_str(),
        &asset.tag_name,
        &current_checksum,
        &expected_checksum,
    );
    Ok(UpgradeCandidate {
        asset,
        expected_checksum,
        current_exe,
        backup_path,
        update_available,
    })
}

async fn install_upgrade_candidate(
    state: &AppState,
    cfg: &ServerConfig,
    candidate: UpgradeCandidate,
    install_event: &'static str,
    restart_event: &'static str,
) -> Result<UpgradeResponse> {
    let tmp_path = std::env::temp_dir().join(&candidate.asset.asset_name);
    emit_upgrade_event(state, "downloading", "downloading release asset");
    download_asset(&candidate.asset.download_url, &tmp_path).await?;
    emit_upgrade_event(state, "verifying", "verifying downloaded binary");
    verify_file_sha256(&tmp_path, &candidate.expected_checksum)?;
    make_executable(&tmp_path)?;
    verify_binary(&tmp_path)?;
    emit_upgrade_event(state, "replacing", "replacing current binary");
    replace_binary(&tmp_path, &candidate.current_exe, &candidate.backup_path)?;
    add_audit_event(
        state,
        install_event,
        Some(&format!(
            "{} from {}",
            candidate.asset.asset_name, candidate.asset.tag_name
        )),
    )
    .await?;
    emit_upgrade_event(state, "restarting", "requesting service restart");
    let restart = request_restart(
        cfg.systemd_unit.as_deref(),
        Some(candidate.current_exe.as_path()),
    )?;
    add_audit_event(state, restart_event, restart.method).await?;
    emit_upgrade_event(state, "complete", &restart.message);
    Ok(UpgradeResponse {
        started: true,
        message: format!(
            "installed {}; {}",
            candidate.asset.asset_name, restart.message
        ),
        current_version: current_version(),
        resolved_tag: Some(candidate.asset.tag_name),
        update_available: true,
        asset_name: Some(candidate.asset.asset_name),
        backup_path: Some(candidate.backup_path.display().to_string()),
        restart_method: restart.method.map(ToString::to_string),
        checksum_sha256: Some(candidate.expected_checksum),
    })
}

fn update_available(
    current_version: &str,
    release_tag: &str,
    current_checksum: &str,
    release_checksum: &str,
) -> bool {
    match compare_versions(release_tag, current_version) {
        Some(VersionOrdering::Greater) => true,
        Some(VersionOrdering::Equal) | Some(VersionOrdering::Less) => false,
        None => !current_checksum.eq_ignore_ascii_case(release_checksum),
    }
}

fn compare_versions(left: &str, right: &str) -> Option<VersionOrdering> {
    let left = parse_version_numbers(left)?;
    let right = parse_version_numbers(right)?;
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left_value = left.get(index).copied().unwrap_or_default();
        let right_value = right.get(index).copied().unwrap_or_default();
        match left_value.cmp(&right_value) {
            VersionOrdering::Equal => {}
            ordering => return Some(ordering),
        }
    }
    Some(VersionOrdering::Equal)
}

fn parse_version_numbers(value: &str) -> Option<Vec<u64>> {
    let core = value
        .trim()
        .trim_start_matches('v')
        .trim_start_matches('V')
        .split(['-', '+'])
        .next()
        .unwrap_or_default();
    if core.is_empty() {
        return None;
    }
    core.split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                None
            } else {
                part.parse::<u64>().ok()
            }
        })
        .collect()
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn effective_release_repo(cfg: &ServerConfig) -> String {
    let repo = cfg.release_repo.trim();
    if repo.is_empty() {
        DEFAULT_RELEASE_REPO.to_string()
    } else {
        repo.to_string()
    }
}

async fn upgrade_setting(state: &AppState, key: &str) -> Result<Option<String>> {
    let row = sqlx::query("SELECT value FROM settings WHERE key = ?1")
        .bind(key)
        .fetch_optional(&state.pool)
        .await
        .map_err(AppError::internal)?;
    Ok(row.map(|row| row.get::<String, _>("value")))
}

async fn record_auto_upgrade_status(
    state: &AppState,
    result: &str,
    message: &str,
    latest_tag: Option<&str>,
    update_available: Option<bool>,
) -> Result<()> {
    set_upgrade_setting_current_timestamp(state, "auto_upgrade_last_checked_at").await?;
    set_upgrade_setting(state, "auto_upgrade_last_result", result).await?;
    set_upgrade_setting(state, "auto_upgrade_last_message", message).await?;
    if let Some(tag) = latest_tag {
        set_upgrade_setting(state, "auto_upgrade_latest_tag", tag).await?;
    }
    if let Some(available) = update_available {
        set_upgrade_setting(
            state,
            "auto_upgrade_update_available",
            if available { "true" } else { "false" },
        )
        .await?;
    }
    Ok(())
}

async fn set_upgrade_setting(state: &AppState, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value, category, requires_restart) VALUES (?1, ?2, 'upgrade', FALSE) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(key)
    .bind(value)
    .execute(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

async fn set_upgrade_setting_current_timestamp(state: &AppState, key: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value, category, requires_restart) VALUES (?1, CURRENT_TIMESTAMP, 'upgrade', FALSE) \
         ON CONFLICT(key) DO UPDATE SET value = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP",
    )
    .bind(key)
    .execute(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok(())
}

async fn resolve_release_asset(cfg: &ServerConfig) -> Result<ReleaseAsset> {
    let release_repo = effective_release_repo(cfg);
    let release_url = if cfg.release_tag == "latest" {
        format!(
            "https://api.github.com/repos/{}/releases/latest",
            release_repo
        )
    } else {
        format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            release_repo, cfg.release_tag
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
    let actual = sha256_file(path)?;
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

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(AppError::internal)?;
    Ok(sha256_hex(&bytes))
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
    make_executable_file(path).map_err(AppError::internal)
}

fn make_executable_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
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
    let staging = staging_upgrade_path(current_exe)?;
    let _ = fs::remove_file(&staging);
    fs::copy(current_exe, backup)?;
    if let Err(error) = fs::copy(new_binary, &staging)
        .and_then(|_| make_executable_file(&staging))
        .and_then(|_| fs::rename(&staging, current_exe))
    {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    Ok(())
}

fn staging_upgrade_path(current_exe: &Path) -> std::io::Result<PathBuf> {
    let parent = current_exe.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "current executable path has no parent directory",
        )
    })?;
    let file_name = current_exe.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "current executable path has no file name",
        )
    })?;
    let mut staging_name = file_name.to_os_string();
    staging_name.push(format!(".upgrade-{}", std::process::id()));
    Ok(parent.join(staging_name))
}

struct RestartRequest {
    attempted: bool,
    method: Option<&'static str>,
    message: String,
}

fn request_restart(unit: Option<&str>, exec_path: Option<&Path>) -> Result<RestartRequest> {
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
        schedule_exec_restart(exec_path)?;
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
fn schedule_exec_restart(exec_path: Option<&Path>) -> Result<()> {
    let current_exe = match exec_path {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe().map_err(AppError::internal)?,
    };
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
fn schedule_exec_restart(_exec_path: Option<&Path>) -> Result<()> {
    Err(AppError::new(
        StatusCode::BAD_GATEWAY,
        "restart_not_supported",
        "exec restart is not supported on this platform",
    ))
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

    #[test]
    fn compares_semver_like_release_tags() {
        assert_eq!(
            compare_versions("v0.2.0", "0.1.9"),
            Some(VersionOrdering::Greater)
        );
        assert_eq!(
            compare_versions("0.1.0", "v0.1.0"),
            Some(VersionOrdering::Equal)
        );
        assert_eq!(
            compare_versions("0.1.0", "0.1.1"),
            Some(VersionOrdering::Less)
        );
        assert_eq!(compare_versions("latest", "0.1.0"), None);
    }

    #[test]
    fn update_available_prefers_newer_versions_then_checksum_fallback() {
        let old_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let new_hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert!(update_available("0.1.0", "v0.2.0", old_hash, old_hash));
        assert!(!update_available("0.2.0", "v0.2.0", old_hash, new_hash));
        assert!(update_available("0.1.0", "latest", old_hash, new_hash));
        assert!(!update_available("0.1.0", "latest", old_hash, old_hash));
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
