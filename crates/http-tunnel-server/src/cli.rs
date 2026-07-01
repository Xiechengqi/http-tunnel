use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "http-tunnel-server",
    version,
    about = "HTTP/WebSocket tunnel server"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Serve {
        #[arg(long, env = "HTTP_TUNNEL_CONFIG")]
        config: Option<String>,
        #[arg(long, env = "HTTP_TUNNEL_PORT")]
        port: Option<u16>,
    },
    Backup {
        #[arg(long, env = "HTTP_TUNNEL_CONFIG")]
        config: Option<String>,
        #[arg(long)]
        output: PathBuf,
    },
    Restore {
        #[arg(long, env = "HTTP_TUNNEL_CONFIG")]
        config: Option<String>,
        #[arg(long)]
        backup: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
}
