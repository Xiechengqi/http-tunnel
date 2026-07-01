use anyhow::{bail, Context};
use http_tunnel_common::{build_info::BuildInfo, ServerConfig};
use serde::Serialize;
use sqlx::Row;
use std::{
    fs::{self, File},
    io::{Cursor, Read, Seek, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const MANIFEST_NAME: &str = "manifest.json";
const CONFIG_ENTRY: &str = "config/server.toml";
const DB_ENTRY: &str = "data/http-tunnel.sqlite3";

#[derive(Debug, Clone, Serialize)]
pub struct BackupEntry {
    pub name: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupReport {
    pub archive: Option<String>,
    pub entries: Vec<BackupEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupValidationReport {
    pub path: String,
    pub valid: bool,
    pub entries: Vec<BackupEntry>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupRestoreReport {
    pub backup: String,
    pub config_path: String,
    pub database_path: String,
    pub companion_paths: Vec<String>,
    pub overwritten_paths: Vec<String>,
    pub removed_stale_paths: Vec<String>,
    pub restored: bool,
    pub backup_files: Vec<String>,
    pub entries: Vec<BackupEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BackupManifest {
    schema_version: u8,
    created_unix_seconds: u64,
    build: BuildInfo,
    entries: Vec<String>,
}

pub fn create_backup_file(
    config_path: &Path,
    cfg: &ServerConfig,
    output: &Path,
) -> anyhow::Result<BackupReport> {
    let database_path = database_path_from_url(&cfg.database_url)
        .context("database_url does not point to a filesystem SQLite database")?;
    if !database_path.exists() {
        bail!("database file does not exist: {}", database_path.display());
    }
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create backup output dir {}", parent.display()))?;
    }
    let file = File::create(output).with_context(|| format!("create {}", output.display()))?;
    let (_, mut report) = write_backup(file, config_path, &database_path)?;
    report.archive = Some(output.display().to_string());
    Ok(report)
}

pub async fn create_backup_bytes(
    pool: &sqlx::SqlitePool,
    config_path: &Path,
) -> anyhow::Result<(Vec<u8>, BackupReport)> {
    let database_path = running_database_path(pool)
        .await?
        .context("running database is not a filesystem SQLite database")?;
    sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
        .execute(pool)
        .await
        .context("checkpoint WAL before backup")?;
    let cursor = Cursor::new(Vec::new());
    let (cursor, mut report) = write_backup(cursor, config_path, &database_path)?;
    report.archive = None;
    let bytes = cursor.into_inner();
    Ok((bytes, report))
}

pub fn validate_backup_file(path: &Path) -> anyhow::Result<BackupValidationReport> {
    let mut errors = Vec::new();
    let file = File::open(path).with_context(|| format!("open backup {}", path.display()))?;
    let mut archive =
        ZipArchive::new(file).with_context(|| format!("read backup zip {}", path.display()))?;
    let mut entries = Vec::new();
    let mut has_manifest = false;
    let mut has_config = false;
    let mut has_database = false;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let name = file.name().to_string();
        let size_bytes = file.size();
        if name == MANIFEST_NAME {
            has_manifest = true;
            let mut raw = String::new();
            if let Err(error) = file.read_to_string(&mut raw) {
                errors.push(format!("manifest is unreadable: {error}"));
            } else if serde_json::from_str::<serde_json::Value>(&raw).is_err() {
                errors.push("manifest is not valid JSON".to_string());
            }
        }
        if name == CONFIG_ENTRY {
            has_config = true;
            let mut raw = String::new();
            if let Err(error) = file.read_to_string(&mut raw) {
                errors.push(format!("config/server.toml is unreadable: {error}"));
            } else {
                match toml::from_str::<ServerConfig>(&raw) {
                    Ok(config) => {
                        if database_path_from_url(&config.database_url).is_none() {
                            errors.push(
                                "config/server.toml database_url is not a filesystem SQLite database"
                                    .to_string(),
                            );
                        }
                    }
                    Err(error) => {
                        errors.push(format!(
                            "config/server.toml is not valid server config TOML: {error}"
                        ));
                    }
                }
            }
        }
        if name == DB_ENTRY {
            has_database = true;
        }
        if size_bytes == 0 && matches!(name.as_str(), MANIFEST_NAME | CONFIG_ENTRY | DB_ENTRY) {
            errors.push(format!("{name} is empty"));
        }
        entries.push(BackupEntry { name, size_bytes });
    }
    if !has_manifest {
        errors.push("manifest.json is missing".to_string());
    }
    if !has_config {
        errors.push("config/server.toml is missing".to_string());
    }
    if !has_database {
        errors.push("data/http-tunnel.sqlite3 is missing".to_string());
    }
    Ok(BackupValidationReport {
        path: path.display().to_string(),
        valid: errors.is_empty(),
        entries,
        errors,
    })
}

pub fn restore_backup_file(
    backup_path: &Path,
    config_path: &Path,
    dry_run: bool,
) -> anyhow::Result<BackupRestoreReport> {
    let validation = validate_backup_file(backup_path)?;
    if !validation.valid {
        bail!("backup validation failed: {}", validation.errors.join("; "));
    }
    let file = File::open(backup_path)
        .with_context(|| format!("open backup {}", backup_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("read backup zip {}", backup_path.display()))?;
    let config_bytes = archive_entry_bytes(&mut archive, CONFIG_ENTRY)?;
    let config_raw =
        String::from_utf8(config_bytes.clone()).context("backup config is not UTF-8")?;
    let restored_config: ServerConfig =
        toml::from_str(&config_raw).context("parse backup config")?;
    let mut database_path = database_path_from_url(&restored_config.database_url)
        .context("backup database_url does not point to a filesystem SQLite database")?;
    if database_path.is_relative() {
        database_path = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(database_path);
    }
    let database_bytes = archive_entry_bytes(&mut archive, DB_ENTRY)?;
    let wal_bytes = archive_entry_bytes_optional(&mut archive, "data/http-tunnel.sqlite3-wal")?;
    let shm_bytes = archive_entry_bytes_optional(&mut archive, "data/http-tunnel.sqlite3-shm")?;
    let wal_path = PathBuf::from(format!("{}-wal", database_path.display()));
    let shm_path = PathBuf::from(format!("{}-shm", database_path.display()));
    let companion_paths = vec![
        wal_path.display().to_string(),
        shm_path.display().to_string(),
    ];
    let mut overwritten_paths = Vec::new();
    for path in [config_path, database_path.as_path()] {
        if path.exists() {
            overwritten_paths.push(path.display().to_string());
        }
    }
    if wal_bytes.is_some() && wal_path.exists() {
        overwritten_paths.push(wal_path.display().to_string());
    }
    if shm_bytes.is_some() && shm_path.exists() {
        overwritten_paths.push(shm_path.display().to_string());
    }
    let mut removed_stale_paths = Vec::new();
    if wal_bytes.is_none() && wal_path.exists() {
        removed_stale_paths.push(wal_path.display().to_string());
    }
    if shm_bytes.is_none() && shm_path.exists() {
        removed_stale_paths.push(shm_path.display().to_string());
    }
    let mut backup_files = Vec::new();

    if !dry_run {
        backup_existing(config_path, &mut backup_files)?;
        write_restored_file(config_path, &config_bytes)?;
        backup_existing(&database_path, &mut backup_files)?;
        write_restored_file(&database_path, &database_bytes)?;
        restore_optional_companion(&wal_path, wal_bytes.as_deref(), &mut backup_files)?;
        restore_optional_companion(&shm_path, shm_bytes.as_deref(), &mut backup_files)?;
    }

    Ok(BackupRestoreReport {
        backup: backup_path.display().to_string(),
        config_path: config_path.display().to_string(),
        database_path: database_path.display().to_string(),
        companion_paths,
        overwritten_paths,
        removed_stale_paths,
        restored: !dry_run,
        backup_files,
        entries: validation.entries,
        warnings: vec![
            "Stop the server before running restore without --dry-run.".to_string(),
            "Restore writes the config path passed to the command and the database path declared inside the backup config.".to_string(),
            "Existing target files are copied to .restore-bak before replacement.".to_string(),
        ],
    })
}

pub async fn running_database_path(pool: &sqlx::SqlitePool) -> anyhow::Result<Option<PathBuf>> {
    let rows = sqlx::query("PRAGMA database_list")
        .fetch_all(pool)
        .await
        .context("read sqlite database list")?;
    Ok(rows.into_iter().find_map(|row| {
        let name = row.get::<String, _>("name");
        let file = row.get::<String, _>("file");
        (name == "main" && !file.is_empty()).then_some(PathBuf::from(file))
    }))
}

fn archive_entry_bytes<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> anyhow::Result<Vec<u8>> {
    let mut file = archive
        .by_name(name)
        .with_context(|| format!("backup entry {name} is missing"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read backup entry {name}"))?;
    Ok(bytes)
}

fn archive_entry_bytes_optional<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    match archive.by_name(name) {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .with_context(|| format!("read backup entry {name}"))?;
            Ok(Some(bytes))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read backup entry {name}")),
    }
}

fn write_restored_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("create restore directory {}", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("write restored file {}", path.display()))
}

fn restore_optional_companion(
    path: &Path,
    bytes: Option<&[u8]>,
    backup_files: &mut Vec<String>,
) -> anyhow::Result<()> {
    match bytes {
        Some(bytes) => {
            backup_existing(path, backup_files)?;
            write_restored_file(path, bytes)?;
        }
        None if path.exists() => {
            backup_existing(path, backup_files)?;
            fs::remove_file(path)
                .with_context(|| format!("remove stale companion file {}", path.display()))?;
        }
        None => {}
    }
    Ok(())
}

fn backup_existing(path: &Path, backup_files: &mut Vec<String>) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let backup = restore_backup_path(path);
    if let Some(parent) = backup.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("create restore backup dir {}", parent.display()))?;
    }
    fs::copy(path, &backup)
        .with_context(|| format!("copy existing {} to {}", path.display(), backup.display()))?;
    backup_files.push(backup.display().to_string());
    Ok(())
}

fn restore_backup_path(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".restore-bak");
    PathBuf::from(backup)
}

pub fn database_path_from_url(database_url: &str) -> Option<PathBuf> {
    let path = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))
        .unwrap_or(database_url);
    if path == ":memory:" || path.starts_with("file:") {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn write_backup<W: Write + Seek>(
    writer: W,
    config_path: &Path,
    database_path: &Path,
) -> anyhow::Result<(W, BackupReport)> {
    let mut zip = ZipWriter::new(writer);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut entries = Vec::new();

    let config_bytes = std::fs::read(config_path)
        .with_context(|| format!("read config {}", config_path.display()))?;
    add_bytes(&mut zip, options, CONFIG_ENTRY, &config_bytes, &mut entries)?;
    add_path(&mut zip, options, DB_ENTRY, database_path, &mut entries)?;
    add_optional_path(
        &mut zip,
        options,
        "data/http-tunnel.sqlite3-wal",
        &PathBuf::from(format!("{}-wal", database_path.display())),
        &mut entries,
    )?;
    add_optional_path(
        &mut zip,
        options,
        "data/http-tunnel.sqlite3-shm",
        &PathBuf::from(format!("{}-shm", database_path.display())),
        &mut entries,
    )?;
    let build_info = serde_json::to_vec_pretty(&BuildInfo::current())?;
    add_bytes(
        &mut zip,
        options,
        "build/build-info.json",
        &build_info,
        &mut entries,
    )?;
    let manifest = BackupManifest {
        schema_version: 1,
        created_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        build: BuildInfo::current(),
        entries: entries.iter().map(|entry| entry.name.clone()).collect(),
    };
    let manifest = serde_json::to_vec_pretty(&manifest)?;
    add_bytes(&mut zip, options, MANIFEST_NAME, &manifest, &mut entries)?;
    let writer = zip.finish()?;
    Ok((
        writer,
        BackupReport {
            archive: None,
            entries,
        },
    ))
}

fn add_optional_path<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    name: &str,
    path: &Path,
    entries: &mut Vec<BackupEntry>,
) -> anyhow::Result<()> {
    if path.exists() {
        add_path(zip, options, name, path, entries)?;
    }
    Ok(())
}

fn add_path<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    name: &str,
    path: &Path,
    entries: &mut Vec<BackupEntry>,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    add_bytes(zip, options, name, &bytes, entries)
}

fn add_bytes<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    name: &str,
    bytes: &[u8],
    entries: &mut Vec<BackupEntry>,
) -> anyhow::Result<()> {
    zip.start_file(name, options)?;
    zip.write_all(bytes)?;
    entries.push(BackupEntry {
        name: name.to_string(),
        size_bytes: bytes.len() as u64,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_backup_file_dry_run_reports_destinations_and_restore_writes_files() {
        let root = temp_test_dir("restore");
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        let source_config = source.join("server.toml");
        let target_config = target.join("server.toml");
        let target_db = target.join("http-tunnel.sqlite3");
        let cfg = ServerConfig {
            domain: Some("example.com".to_string()),
            database_url: format!("sqlite://{}", target_db.display()),
            ..ServerConfig::default()
        };
        cfg.save(&source_config).unwrap();
        fs::write(&target_db, b"restored-db").unwrap();
        let backup_path = root.join("backup.zip");

        create_backup_file(&source_config, &cfg, &backup_path).unwrap();
        fs::write(&target_config, b"old-config").unwrap();
        fs::write(&target_db, b"old-db").unwrap();
        fs::write(format!("{}-wal", target_db.display()), b"stale-wal").unwrap();
        let dry_run = restore_backup_file(&backup_path, &target_config, true).unwrap();
        assert!(!dry_run.restored);
        assert_eq!(dry_run.config_path, target_config.display().to_string());
        assert_eq!(dry_run.database_path, target_db.display().to_string());
        assert!(dry_run
            .overwritten_paths
            .iter()
            .any(|path| path.ends_with("server.toml")));
        assert!(dry_run
            .removed_stale_paths
            .iter()
            .any(|path| path.ends_with("http-tunnel.sqlite3-wal")));
        assert_eq!(fs::read(&target_db).unwrap(), b"old-db");

        let restored = restore_backup_file(&backup_path, &target_config, false).unwrap();
        assert!(restored.restored);
        assert_eq!(fs::read(&target_db).unwrap(), b"restored-db");
        assert!(!PathBuf::from(format!("{}-wal", target_db.display())).exists());
        assert!(restored
            .backup_files
            .iter()
            .any(|path| path.ends_with("server.toml.restore-bak")));
    }

    fn temp_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("http-tunnel-backup-{name}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
