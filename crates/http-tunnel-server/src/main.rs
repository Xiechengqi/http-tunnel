mod app;
mod backup;
mod cli;
mod db;
mod error;
mod net;
mod redaction;
mod routes;
mod state;

use anyhow::Context;
use clap::Parser;
use cli::{Cli, Command};
use http_tunnel_common::{build_info::BuildInfo, config::config_path, ServerConfig};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "http_tunnel_server=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve {
        config: None,
        port: None,
    }) {
        Command::Serve { config, port } => {
            let config_path = config_path(config);
            let mut cfg = ServerConfig::load(&config_path)
                .with_context(|| format!("load config from {config_path}"))?;
            if let Some(port) = port {
                cfg.addr.set_port(port);
            }
            app::serve(config_path, cfg).await
        }
        Command::Backup { config, output } => {
            let config_path = config_path(config);
            let cfg = ServerConfig::load(&config_path)
                .with_context(|| format!("load config from {config_path}"))?;
            let report =
                backup::create_backup_file(std::path::Path::new(&config_path), &cfg, &output)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Restore {
            config,
            backup,
            dry_run,
        } => {
            let config_path = config_path(config);
            let report =
                backup::restore_backup_file(&backup, std::path::Path::new(&config_path), dry_run)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Version => {
            println!("{}", serde_json::to_string_pretty(&BuildInfo::current())?);
            Ok(())
        }
    }
}
