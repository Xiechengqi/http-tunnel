mod cli;
mod config;
mod connect;
mod doctor;
mod http_forward;
mod runtime;
mod ws_forward;

use anyhow::Context;
use clap::Parser;
use cli::{Cli, Command, ConfigCommand, RuntimeCommand};
use config::{
    clear_stored_tunnel, clear_stored_tunnel_on_endpoint_override, default_config_path,
    init_config_file, load_config_file, save_config_file,
};
use connect::{connect, release_tunnel};
use doctor::run_doctor;
use runtime::{clean_runtime, read_status, request_disconnect};
use std::time::{Duration, Instant};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "http_tunnel_client=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
        }
        Command::Config {
            command: ConfigCommand::Init,
        } => {
            let path = init_config_file()?;
            println!("created config: {}", path.display());
        }
        Command::Config {
            command: ConfigCommand::Show,
        } => {
            let cfg = load_config_file()?;
            print!(
                "{}",
                toml::to_string_pretty(&cfg).context("serialize client config")?
            );
        }
        Command::Config {
            command: ConfigCommand::ClearToken,
        } => {
            let mut cfg = load_config_file()?;
            clear_stored_tunnel(&mut cfg);
            save_config_file(&cfg)?;
            println!("cleared persisted tunnel token");
        }
        Command::Config {
            command:
                ConfigCommand::Set {
                    server,
                    target,
                    subdomain,
                    tunnel_id,
                    token,
                    url,
                    create_token,
                    persist_token,
                },
        } => {
            let mut cfg = load_config_file()?;
            if let Some(server) = server {
                if cfg.server.as_deref() != Some(server.as_str()) {
                    clear_stored_tunnel(&mut cfg);
                }
                cfg.server = Some(server);
            }
            if let Some(target) = target {
                cfg.target = Some(target);
            }
            if let Some(subdomain) = subdomain {
                if cfg.subdomain.as_deref() != Some(subdomain.as_str()) {
                    clear_stored_tunnel(&mut cfg);
                }
                cfg.subdomain = Some(subdomain);
            }
            if let Some(tunnel_id) = tunnel_id {
                cfg.tunnel_id = Some(tunnel_id);
            }
            if let Some(token) = token {
                cfg.token = Some(token);
            }
            if let Some(url) = url {
                cfg.url = Some(url);
            }
            if let Some(create_token) = create_token {
                cfg.create_token = Some(create_token);
            }
            if let Some(persist_token) = persist_token {
                cfg.persist_token = Some(persist_token);
                if !persist_token {
                    clear_stored_tunnel(&mut cfg);
                }
            }
            save_config_file(&cfg)?;
            println!("updated config: {}", default_config_path().display());
        }
        Command::Doctor {
            server,
            target,
            subdomain,
            json,
            websocket_path,
        } => {
            let cfg = load_config_file()?;
            run_doctor(server, target, subdomain, cfg, json, websocket_path).await?;
        }
        Command::Http {
            port,
            server,
            subdomain,
            create_token,
            no_persist_token,
            json_events,
        } => {
            let mut cfg = load_config_file()?;
            let old_server = cfg.server.clone();
            let old_subdomain = cfg.subdomain.clone();
            let explicit_server = server.is_some();
            let explicit_subdomain = subdomain.is_some();
            let target = format!("http://127.0.0.1:{port}");
            let server = server
                .or_else(|| cfg.server.clone())
                .context("server is required via --server or client config")?;
            let subdomain = subdomain.or_else(|| cfg.subdomain.clone());
            clear_stored_tunnel_on_endpoint_override(
                &mut cfg,
                explicit_server,
                old_server.as_deref(),
                &server,
                explicit_subdomain,
                old_subdomain.as_deref(),
                subdomain.as_deref(),
            );
            cfg.server = Some(server.clone());
            cfg.target = Some(target.clone());
            cfg.subdomain = subdomain.clone();
            if let Some(create_token) = create_token {
                cfg.create_token = Some(create_token);
            }
            if no_persist_token {
                cfg.persist_token = Some(false);
            }
            connect(server, target, subdomain, cfg, json_events).await?;
        }
        Command::Connect {
            server,
            target,
            subdomain,
            create_token,
            no_persist_token,
            json_events,
        } => {
            let mut cfg = load_config_file()?;
            let old_server = cfg.server.clone();
            let old_subdomain = cfg.subdomain.clone();
            let explicit_server = server.is_some();
            let explicit_subdomain = subdomain.is_some();
            let server = server
                .or_else(|| cfg.server.clone())
                .context("server is required via --server or client config")?;
            let target = target
                .or_else(|| cfg.target.clone())
                .context("target is required via --target or client config")?;
            let subdomain = subdomain.or_else(|| cfg.subdomain.clone());
            clear_stored_tunnel_on_endpoint_override(
                &mut cfg,
                explicit_server,
                old_server.as_deref(),
                &server,
                explicit_subdomain,
                old_subdomain.as_deref(),
                subdomain.as_deref(),
            );
            cfg.server = Some(server.clone());
            cfg.target = Some(target.clone());
            cfg.subdomain = subdomain.clone();
            if let Some(create_token) = create_token {
                cfg.create_token = Some(create_token);
            }
            if no_persist_token {
                cfg.persist_token = Some(false);
            }
            connect(server, target, subdomain, cfg, json_events).await?;
        }
        Command::Release {
            server,
            tunnel_id,
            token,
        } => {
            let mut cfg = load_config_file()?;
            let server = server
                .or_else(|| cfg.server.clone())
                .context("server is required via --server or client config")?;
            let tunnel_id = tunnel_id
                .or_else(|| cfg.tunnel_id.clone())
                .context("tunnel id is required via --tunnel-id or client config")?;
            let token = token
                .or_else(|| cfg.token.clone())
                .context("tunnel token is required via --token or client config")?;
            release_tunnel(&server, &tunnel_id, &token).await?;
            clear_stored_tunnel(&mut cfg);
            save_config_file(&cfg)?;
            println!("released tunnel: {tunnel_id}");
        }
        Command::Runtime {
            command: RuntimeCommand::Clean { force },
        } => {
            let result = clean_runtime(force)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Status {
            watch,
            interval_seconds,
        } => {
            if watch {
                loop {
                    print_runtime_status()?;
                    tokio::time::sleep(Duration::from_secs(interval_seconds.max(1))).await;
                }
            } else {
                print_runtime_status()?;
            }
        }
        Command::Disconnect { timeout_seconds } => {
            let path = request_disconnect()?;
            println!("disconnect requested: {}", path.display());
            wait_for_disconnect(timeout_seconds).await?;
        }
    }
    Ok(())
}

fn print_runtime_status() -> anyhow::Result<()> {
    match read_status()? {
        Some(status) => println!("{}", serde_json::to_string_pretty(&status)?),
        None => println!("null"),
    }
    Ok(())
}

async fn wait_for_disconnect(timeout_seconds: u64) -> anyhow::Result<()> {
    let timeout = Duration::from_secs(timeout_seconds.max(1));
    let start = Instant::now();
    loop {
        match read_status()? {
            Some(status) if status.connected && !status.stale => {}
            Some(status) => {
                println!("{}", serde_json::to_string_pretty(&status)?);
                return Ok(());
            }
            None => {
                println!("null");
                return Ok(());
            }
        }
        if start.elapsed() >= timeout {
            anyhow::bail!("disconnect timed out after {}s", timeout.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
