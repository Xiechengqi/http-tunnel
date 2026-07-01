use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

fn main() {
    println!("cargo:rerun-if-env-changed=HTTP_TUNNEL_COMMIT");
    println!("cargo:rerun-if-env-changed=HTTP_TUNNEL_COMMIT_MESSAGE");
    println!("cargo:rerun-if-env-changed=HTTP_TUNNEL_BUILD_TIME");
    emit_git_rerun_paths();

    let commit = env_or_command("HTTP_TUNNEL_COMMIT", &["rev-parse", "--short=7", "HEAD"]);
    let commit_message =
        env_or_command("HTTP_TUNNEL_COMMIT_MESSAGE", &["log", "-1", "--pretty=%s"]);
    let build_time = env::var("HTTP_TUNNEL_BUILD_TIME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(current_build_time);

    println!("cargo:rustc-env=HTTP_TUNNEL_COMMIT={}", sanitize(&commit));
    println!(
        "cargo:rustc-env=HTTP_TUNNEL_COMMIT_MESSAGE={}",
        sanitize(&commit_message)
    );
    println!(
        "cargo:rustc-env=HTTP_TUNNEL_BUILD_TIME={}",
        sanitize(&build_time)
    );
}

fn env_or_command(name: &str, args: &[&str]) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| git(args).unwrap_or_else(|| "unknown".to_string()))
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn current_build_time() -> String {
    OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

fn emit_git_rerun_paths() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let git_dir = manifest_dir.join("../../.git");
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );

    if let Some(ref_path) = head_ref_path(&git_dir) {
        println!("cargo:rerun-if-changed={}", ref_path.display());
    }
}

fn head_ref_path(git_dir: &Path) -> Option<PathBuf> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let reference = head.trim().strip_prefix("ref: ")?;
    Some(git_dir.join(reference))
}

fn sanitize(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|ch| match ch {
            '\r' | '\n' => ' ',
            _ => ch,
        })
        .collect()
}
