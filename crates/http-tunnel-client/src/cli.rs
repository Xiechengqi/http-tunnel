use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "http-tunnel-client",
    version,
    about = "HTTP/WebSocket tunnel client"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Connect {
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        subdomain: Option<String>,
        #[arg(long)]
        ttl_seconds: Option<u64>,
        #[arg(long, env = "HTTP_TUNNEL_CREATE_TOKEN")]
        create_token: Option<String>,
        #[arg(long)]
        no_persist_token: bool,
        #[arg(long)]
        json_events: bool,
    },
    Http {
        port: u16,
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        subdomain: Option<String>,
        #[arg(long)]
        ttl_seconds: Option<u64>,
        #[arg(long, env = "HTTP_TUNNEL_CREATE_TOKEN")]
        create_token: Option<String>,
        #[arg(long)]
        no_persist_token: bool,
        #[arg(long)]
        json_events: bool,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Doctor {
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        subdomain: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        websocket_path: Option<String>,
    },
    Release {
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        tunnel_id: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    Status {
        #[arg(long)]
        watch: bool,
        #[arg(long, default_value_t = 1)]
        interval_seconds: u64,
    },
    Disconnect {
        #[arg(long, default_value_t = 10)]
        timeout_seconds: u64,
    },
    Version,
}

#[derive(Debug, Subcommand)]
pub enum RuntimeCommand {
    Clean {
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Init,
    Show,
    ClearToken,
    Set {
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        subdomain: Option<String>,
        #[arg(long)]
        ttl_seconds: Option<u64>,
        #[arg(long)]
        tunnel_id: Option<String>,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long, env = "HTTP_TUNNEL_CREATE_TOKEN")]
        create_token: Option<String>,
        #[arg(long, value_parser = clap::value_parser!(bool))]
        persist_token: Option<bool>,
        #[arg(long = "public-ip-lookup-url", value_delimiter = ',')]
        public_ip_lookup_urls: Vec<String>,
        #[arg(long)]
        public_ip_refresh_seconds: Option<u64>,
    },
}
