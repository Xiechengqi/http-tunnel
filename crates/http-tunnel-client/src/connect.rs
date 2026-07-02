use crate::{
    config::{clear_stored_tunnel, ensure_client_identity, save_config_file, ClientConfig},
    http_forward::forward_one_request,
    runtime::{
        acquire_instance_lock, clear_disconnect_request, disconnect_requested, write_status,
        RuntimeInstanceLock, RuntimeStatus,
    },
    ws_forward::{forward_one_websocket, frame_to_ws_close, frame_to_ws_message},
};
use anyhow::Context;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_tunnel_common::{
    api::ApiResponse,
    country::{country_code_from_name, country_from_location, normalize_country_code},
    ip::parse_public_ip,
};
use http_tunnel_protocol::{
    decode_frame, encode_frame,
    types::{
        decode_payload, encode_payload, ClientSourceReport, Hello, HelloAck, RequestStart, WsOpen,
    },
    version::VERSION as PROTOCOL_VERSION,
    Frame, FrameType,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Deserialize)]
struct CreateTunnelResponse {
    id: String,
    token: String,
    url: String,
    connect_url: String,
}

#[derive(Debug)]
struct PendingRequest {
    body_tx: Option<mpsc::Sender<Result<Bytes, io::Error>>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
struct PendingWs {
    tx: mpsc::Sender<Message>,
}

const CLIENT_SOURCE_UPDATE_CAPABILITY: &str = "client_source_update";
const DEFAULT_PUBLIC_IP_REFRESH_SECONDS: u64 = 3600;
const MIN_PUBLIC_IP_REFRESH_SECONDS: u64 = 60;
const PUBLIC_IP_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_PUBLIC_IP_LOOKUP_URLS: &[&str] = &[
    "http://3.0.3.0",
    "https://api64.ipify.org?format=json",
    "https://api.ipify.org?format=json",
];
const DEFAULT_PUBLIC_IP_COUNTRY_LOOKUP_URL_TEMPLATES: &[&str] =
    &["https://ipwho.is/{ip}", "https://api.country.is/{ip}"];
const DUPLICATE_REPLACED_REASON: &str = "duplicate_replaced";
const TUNNEL_EXPIRED_REASON: &str = "tunnel_expired";

#[derive(Debug, Clone)]
struct PublicIpLookup {
    urls: Vec<String>,
    refresh_interval: Duration,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PublicIpLookupResponse {
    public_ip: Option<String>,
    country_code: Option<String>,
    country: Option<String>,
}

impl PublicIpLookup {
    fn from_config(cfg: &ClientConfig) -> Self {
        let urls = cfg
            .public_ip_lookup_urls
            .as_ref()
            .filter(|urls| !urls.is_empty())
            .cloned()
            .unwrap_or_else(|| {
                DEFAULT_PUBLIC_IP_LOOKUP_URLS
                    .iter()
                    .map(|url| (*url).to_string())
                    .collect()
            })
            .into_iter()
            .map(|url| normalize_public_ip_lookup_url(&url))
            .filter(|url| !url.is_empty())
            .collect::<Vec<_>>();
        let refresh_seconds = cfg
            .public_ip_refresh_seconds
            .unwrap_or(DEFAULT_PUBLIC_IP_REFRESH_SECONDS)
            .max(MIN_PUBLIC_IP_REFRESH_SECONDS);
        Self {
            urls,
            refresh_interval: Duration::from_secs(refresh_seconds),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize)]
struct ConnectionStats {
    active_streams: usize,
    bytes_in: u64,
    bytes_out: u64,
    last_disconnect_reason: Option<String>,
}

#[derive(Debug)]
enum ConnectionExit {
    Interrupted {
        stats: ConnectionStats,
    },
    Disconnected {
        reconnect_token: Option<String>,
        stats: ConnectionStats,
    },
}

pub async fn connect(
    server: String,
    target: String,
    subdomain: Option<String>,
    mut cfg: ClientConfig,
    json_events: bool,
) -> anyhow::Result<()> {
    let _instance_lock = match acquire_instance_lock(&server, &target, subdomain.as_deref())? {
        RuntimeInstanceLock::Acquired(lock) => Some(lock),
        RuntimeInstanceLock::Skipped => None,
        RuntimeInstanceLock::Active(active) => {
            if json_events {
                emit_client_event(
                    json_events,
                    "duplicate_instance",
                    &serde_json::json!({
                        "pid": active.pid,
                        "server": active.server,
                        "subdomain": active.subdomain,
                        "target": active.target,
                        "message": duplicate_instance_message(&active.server, &active.subdomain, &active.target, active.pid),
                    }),
                )?;
            } else {
                eprintln!(
                    "{}",
                    duplicate_instance_message(
                        &active.server,
                        &active.subdomain,
                        &active.target,
                        active.pid
                    )
                );
            }
            return Ok(());
        }
    };
    let client = reqwest::Client::new();
    if ensure_client_identity(&mut cfg) {
        if let Err(error) = save_config_file(&cfg) {
            eprintln!("failed to save client identity: {error}");
        }
    }
    let had_stored_tunnel = cfg.tunnel_id.is_some() && cfg.token.is_some();
    let persist_token = cfg.persist_token.unwrap_or(true);
    let data = if let (Some(id), Some(token)) = (cfg.tunnel_id.clone(), cfg.token.clone()) {
        CreateTunnelResponse {
            id: id.clone(),
            token,
            url: cfg.url.clone().unwrap_or_else(|| {
                subdomain
                    .as_ref()
                    .map(|s| format!("{}/{}", server.trim_end_matches('/'), s))
                    .unwrap_or_default()
            }),
            connect_url: format!(
                "{}/api/v1/tunnels/{}/connect",
                server.trim_end_matches('/'),
                id
            ),
        }
    } else {
        create_tunnel(
            &client,
            &server,
            subdomain.clone(),
            cfg.client_id.as_deref(),
            cfg.client_secret.as_deref(),
            cfg.create_token.as_deref(),
            cfg.ttl_seconds,
        )
        .await?
    };
    if !had_stored_tunnel && persist_token {
        cfg.tunnel_id = Some(data.id.clone());
        cfg.token = Some(data.token.clone());
        cfg.url = Some(data.url.clone());
        if let Err(error) = save_config_file(&cfg) {
            eprintln!("failed to save tunnel token: {error}");
        }
    } else if !persist_token
        && (cfg.tunnel_id.is_some() || cfg.token.is_some() || cfg.url.is_some())
    {
        clear_stored_tunnel(&mut cfg);
        if let Err(error) = save_config_file(&cfg) {
            eprintln!("failed to clear persisted tunnel token: {error}");
        }
    }

    if json_events {
        emit_client_event(
            json_events,
            "startup",
            &serde_json::json!({
                "public_url": &data.url,
                "target": &target,
                "connect_url": &data.connect_url,
                "tunnel_id": &data.id,
            }),
        )?;
    } else {
        println!("public url: {}", data.url);
        println!("target: {target}");
        println!("connect url: {}", data.connect_url);
        println!("tunnel id: {}", data.id);
    }
    clear_disconnect_request();
    let mut runtime_status = RuntimeStatus::new(
        server.clone(),
        target.clone(),
        data.id.clone(),
        data.url.clone(),
    );
    let _ = write_status(&runtime_status);
    let mut ws_url = tunnel_ws_url(&server, &data.id, &data.token)?;
    let mut reconnect_delay = std::time::Duration::from_secs(1);
    let mut reconnect_token = None;
    let public_ip_lookup = PublicIpLookup::from_config(&cfg);

    loop {
        if disconnect_requested() {
            clear_disconnect_request();
            runtime_status.connected = false;
            runtime_status.last_disconnect_reason = Some("disconnect_requested".to_string());
            runtime_status.mark_updated();
            let _ = write_status(&runtime_status);
            break;
        }
        match run_tunnel_connection(
            &ws_url,
            &target,
            reconnect_token.clone(),
            json_events,
            &mut runtime_status,
            &public_ip_lookup,
        )
        .await
        {
            Ok(ConnectionExit::Interrupted { stats }) => {
                apply_runtime_stats(&mut runtime_status, false, &stats);
                let _ = write_status(&runtime_status);
                emit_client_event(json_events, "interrupted", &stats)?;
                break;
            }
            Ok(ConnectionExit::Disconnected {
                reconnect_token: token,
                stats,
            }) => {
                reconnect_token = token;
                apply_runtime_stats(&mut runtime_status, false, &stats);
                let _ = write_status(&runtime_status);
                if duplicate_replaced(&stats) {
                    if json_events {
                        emit_client_event(json_events, "disconnected", &stats)?;
                        emit_client_event(
                            json_events,
                            "duplicate_replaced",
                            &serde_json::json!({
                                "message": duplicate_replaced_message(),
                            }),
                        )?;
                    } else {
                        println!(
                            "disconnected; active_streams={} bytes_in={} bytes_out={} reason={}",
                            stats.active_streams,
                            stats.bytes_in,
                            stats.bytes_out,
                            stats.last_disconnect_reason.as_deref().unwrap_or("unknown"),
                        );
                        eprintln!("{}", duplicate_replaced_message());
                    }
                    break;
                }
                if tunnel_expired(&stats) {
                    if json_events {
                        emit_client_event(json_events, "disconnected", &stats)?;
                        emit_client_event(
                            json_events,
                            "tunnel_expired",
                            &serde_json::json!({
                                "message": tunnel_expired_message(),
                            }),
                        )?;
                    } else {
                        println!(
                            "disconnected; active_streams={} bytes_in={} bytes_out={} reason={}",
                            stats.active_streams,
                            stats.bytes_in,
                            stats.bytes_out,
                            stats.last_disconnect_reason.as_deref().unwrap_or("unknown"),
                        );
                        eprintln!("{}", tunnel_expired_message());
                    }
                    if cfg.tunnel_id.is_some() || cfg.token.is_some() || cfg.url.is_some() {
                        clear_stored_tunnel(&mut cfg);
                        if let Err(error) = save_config_file(&cfg) {
                            eprintln!("failed to clear expired tunnel token: {error}");
                        }
                    }
                    break;
                }
                if json_events {
                    emit_client_event(json_events, "disconnected", &stats)?;
                    emit_client_event(
                        json_events,
                        "reconnecting",
                        &serde_json::json!({
                            "delay_seconds": reconnect_delay.as_secs(),
                            "reason": stats.last_disconnect_reason.as_deref(),
                        }),
                    )?;
                } else {
                    println!(
                        "disconnected; active_streams={} bytes_in={} bytes_out={} reason={}; reconnecting in {}s",
                        stats.active_streams,
                        stats.bytes_in,
                        stats.bytes_out,
                        stats.last_disconnect_reason.as_deref().unwrap_or("unknown"),
                        reconnect_delay.as_secs()
                    );
                }
            }
            Err(error) => {
                runtime_status.connected = false;
                runtime_status.last_disconnect_reason = Some(error.to_string());
                runtime_status.mark_updated();
                let _ = write_status(&runtime_status);
                if cfg.tunnel_id.is_some() && stored_tunnel_error_is_terminal(&error) {
                    eprintln!("stored tunnel failed: {error}; creating a new tunnel");
                    clear_stored_tunnel(&mut cfg);
                    let fresh = create_tunnel(
                        &client,
                        &server,
                        subdomain.clone(),
                        cfg.client_id.as_deref(),
                        cfg.client_secret.as_deref(),
                        cfg.create_token.as_deref(),
                        cfg.ttl_seconds,
                    )
                    .await?;
                    if persist_token {
                        cfg.tunnel_id = Some(fresh.id.clone());
                        cfg.token = Some(fresh.token.clone());
                        cfg.url = Some(fresh.url.clone());
                        if let Err(save_error) = save_config_file(&cfg) {
                            eprintln!("failed to save tunnel token: {save_error}");
                        }
                    }
                    ws_url = tunnel_ws_url(&server, &fresh.id, &fresh.token)?;
                    runtime_status.tunnel_id = Some(fresh.id);
                    runtime_status.public_url = Some(fresh.url);
                    runtime_status.mark_updated();
                    let _ = write_status(&runtime_status);
                    reconnect_delay = std::time::Duration::from_secs(1);
                    continue;
                }
                eprintln!(
                    "connection failed: {error}; reconnecting in {}s",
                    reconnect_delay.as_secs()
                );
                if json_events {
                    emit_client_event(
                        json_events,
                        "connection_failed",
                        &serde_json::json!({"error": error.to_string()}),
                    )?;
                    emit_client_event(
                        json_events,
                        "reconnecting",
                        &serde_json::json!({
                            "delay_seconds": reconnect_delay.as_secs(),
                            "reason": error.to_string(),
                        }),
                    )?;
                }
            }
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(reconnect_delay) => {}
        }
        reconnect_delay = next_reconnect_delay(reconnect_delay);
    }

    runtime_status.connected = false;
    runtime_status.active_streams = 0;
    runtime_status
        .last_disconnect_reason
        .get_or_insert_with(|| "client_exit".to_string());
    runtime_status.mark_updated();
    let _ = write_status(&runtime_status);
    clear_disconnect_request();
    emit_client_event(json_events, "exit", &runtime_status)?;
    Ok(())
}

fn duplicate_instance_message(server: &str, subdomain: &str, target: &str, pid: u32) -> String {
    format!(
        "another http-tunnel-client is already running for server={server}, subdomain={subdomain}, target={target} (pid {pid}); exiting"
    )
}

fn duplicate_replaced(stats: &ConnectionStats) -> bool {
    stats.last_disconnect_reason.as_deref() == Some(DUPLICATE_REPLACED_REASON)
}

fn tunnel_expired(stats: &ConnectionStats) -> bool {
    stats.last_disconnect_reason.as_deref() == Some(TUNNEL_EXPIRED_REASON)
}

fn duplicate_replaced_message() -> &'static str {
    "another client connected to the same tunnel, so this client will exit instead of reconnecting"
}

fn tunnel_expired_message() -> &'static str {
    "tunnel ttl expired; the server deleted this tunnel, so the client will exit"
}

fn stored_tunnel_error_is_terminal(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("401")
        || message.contains("403")
        || message.contains("404")
        || message.contains("410")
        || message.contains("unauthorized")
        || message.contains("forbidden")
        || message.contains("not found")
        || message.contains("gone")
        || message.contains("tunnel_not_found")
        || message.contains("tunnel_expired")
}

async fn create_tunnel(
    client: &reqwest::Client,
    server: &str,
    subdomain: Option<String>,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    create_token: Option<&str>,
    ttl_seconds: Option<u64>,
) -> anyhow::Result<CreateTunnelResponse> {
    let create_url = format!("{}/api/v1/tunnels", server.trim_end_matches('/'));
    let mut request = client.post(create_url).json(&serde_json::json!({
        "subdomain": subdomain,
        "client_id": client_id,
        "client_secret": client_secret,
        "ttl_seconds": ttl_seconds,
    }));
    if let Some(token) = create_token.filter(|token| !token.trim().is_empty()) {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .context("create tunnel request failed")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("read create tunnel response")?;
    if !status.is_success() {
        if let Ok(api) = serde_json::from_str::<ApiResponse<serde_json::Value>>(&body) {
            if let Some(error) = api.error {
                anyhow::bail!("create tunnel failed: {}: {}", error.code, error.message);
            }
        }
        anyhow::bail!("create tunnel returned error status {status}: {body}");
    }
    let response = serde_json::from_str::<ApiResponse<CreateTunnelResponse>>(&body)
        .context("decode create tunnel response")?;

    let Some(data) = response.data else {
        anyhow::bail!("create tunnel failed: {:?}", response.error);
    };
    Ok(data)
}

pub async fn release_tunnel(server: &str, tunnel_id: &str, token: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/api/v1/tunnels/{}",
        server.trim_end_matches('/'),
        tunnel_id
    );
    let response = client
        .delete(url)
        .bearer_auth(token)
        .send()
        .await
        .context("release tunnel request failed")?
        .error_for_status()
        .context("release tunnel returned error status")?
        .json::<ApiResponse<()>>()
        .await
        .context("decode release tunnel response")?;
    if let Some(error) = response.error {
        anyhow::bail!("release tunnel failed: {}: {}", error.code, error.message);
    }
    Ok(())
}

async fn run_tunnel_connection(
    ws_url: &str,
    target: &str,
    reconnect_token: Option<String>,
    json_events: bool,
    runtime_status: &mut RuntimeStatus,
    public_ip_lookup: &PublicIpLookup,
) -> anyhow::Result<ConnectionExit> {
    let (ws, _) = connect_async(ws_url)
        .await
        .with_context(|| format!("connect tunnel websocket {ws_url}"))?;
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (frame_tx, mut frame_rx) = mpsc::channel::<Frame>(256);
    let bytes_out = Arc::new(AtomicU64::new(0));
    let writer_bytes_out = bytes_out.clone();

    let writer = tokio::spawn(async move {
        while let Some(frame) = frame_rx.recv().await {
            let encoded = match encode_frame(&frame) {
                Ok(encoded) => encoded,
                Err(error) => {
                    tracing::warn!(%error, "failed to encode frame");
                    continue;
                }
            };
            writer_bytes_out.fetch_add(encoded.len() as u64, Ordering::Relaxed);
            if ws_tx.send(Message::Binary(encoded)).await.is_err() {
                break;
            }
        }
    });

    let client_source = lookup_client_source(public_ip_lookup).await;
    log_client_source_result(
        json_events,
        "startup",
        client_source.as_ref(),
        public_ip_lookup,
    )?;

    frame_tx
        .send(Frame::new(
            FrameType::Hello,
            0,
            encode_payload(&Hello {
                target: target.to_string(),
                client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                protocol_version: Some(PROTOCOL_VERSION),
                capabilities: vec![
                    "http".to_string(),
                    "websocket".to_string(),
                    "heartbeat".to_string(),
                ],
                reconnect_token,
                client_source,
            })
            .context("encode hello payload")?,
        ))
        .await
        .context("send hello frame to writer")?;
    emit_client_event(
        json_events,
        "connected",
        &serde_json::json!({"target": target}),
    )?;
    runtime_status.connected = true;
    runtime_status.active_streams = 0;
    runtime_status.last_disconnect_reason = None;
    runtime_status.mark_updated();
    let _ = write_status(runtime_status);

    let http = reqwest::Client::new();
    let mut pending: HashMap<u64, PendingRequest> = HashMap::new();
    let mut pending_ws: HashMap<u64, PendingWs> = HashMap::new();
    let mut next_reconnect_token = None;
    let mut bytes_in = 0_u64;
    let mut last_disconnect_reason = None;
    let mut goaway_received = false;
    let mut status_tick = tokio::time::interval(std::time::Duration::from_secs(1));
    status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut source_update_supported = false;
    let mut source_update_tick = tokio::time::interval_at(
        tokio::time::Instant::now() + public_ip_lookup.refresh_interval,
        public_ip_lookup.refresh_interval,
    );
    source_update_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        pending.retain(|_, request| !request.task.is_finished());
        if goaway_received && pending.is_empty() && pending_ws.is_empty() {
            writer.abort();
            return Ok(ConnectionExit::Disconnected {
                reconnect_token: next_reconnect_token,
                stats: ConnectionStats {
                    active_streams: 0,
                    bytes_in,
                    bytes_out: bytes_out.load(Ordering::Relaxed),
                    last_disconnect_reason,
                },
            });
        }
        tokio::select! {
            _ = status_tick.tick() => {
                runtime_status.connected = true;
                runtime_status.active_streams = pending.len() + pending_ws.len();
                runtime_status.bytes_in = bytes_in;
                runtime_status.bytes_out = bytes_out.load(Ordering::Relaxed);
                runtime_status.last_disconnect_reason = last_disconnect_reason.clone();
                runtime_status.mark_updated();
                let _ = write_status(runtime_status);
                if disconnect_requested() {
                    clear_disconnect_request();
                    writer.abort();
                    last_disconnect_reason = Some("disconnect_requested".to_string());
                    return Ok(ConnectionExit::Interrupted {
                        stats: ConnectionStats {
                            active_streams: pending.len() + pending_ws.len(),
                            bytes_in,
                            bytes_out: bytes_out.load(Ordering::Relaxed),
                            last_disconnect_reason,
                        },
                    });
                }
            }
            _ = source_update_tick.tick(), if source_update_supported => {
                let report = lookup_client_source(public_ip_lookup).await;
                log_client_source_result(json_events, "refresh", report.as_ref(), public_ip_lookup)?;
                if let Some(report) = report {
                    let payload = encode_payload(&report).context("encode client source update")?;
                    let _ = frame_tx
                        .send(Frame::new(FrameType::ClientSourceUpdate, 0, payload))
                        .await;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                writer.abort();
                return Ok(ConnectionExit::Interrupted {
                    stats: ConnectionStats {
                        active_streams: pending.len() + pending_ws.len(),
                        bytes_in,
                        bytes_out: bytes_out.load(Ordering::Relaxed),
                        last_disconnect_reason,
                    },
                });
            }
            message = ws_rx.next() => {
                let Some(message) = message else {
                    break;
                };
                let message = message.context("read tunnel websocket message")?;
                let Message::Binary(bytes) = message else {
                    continue;
                };
                bytes_in = bytes_in.saturating_add(bytes.len() as u64);
                let frame = decode_frame(&bytes).context("decode server frame")?;
                match frame.frame_type {
                    FrameType::HelloAck => {
                        let ack = decode_payload::<HelloAck>(&frame.payload)
                            .context("decode hello ack")?;
                        if !ack.accepted {
                            anyhow::bail!(
                                "server rejected hello: {}",
                                ack.message.unwrap_or_else(|| "no reason".to_string())
                            );
                        }
                        next_reconnect_token = ack.reconnect_token;
                        source_update_supported = ack
                            .capabilities
                            .iter()
                            .any(|capability| capability == CLIENT_SOURCE_UPDATE_CAPABILITY);
                    }
                    FrameType::Ping => {
                        let _ = frame_tx
                            .send(Frame::new(FrameType::Pong, frame.stream_id, Vec::new()))
                            .await;
                    }
                    FrameType::Pong => {}
                    FrameType::RequestStart => {
                        if goaway_received {
                            let _ = frame_tx
                                .send(Frame::new(
                                    FrameType::Cancel,
                                    frame.stream_id,
                                    b"goaway_received".to_vec(),
                                ))
                                .await;
                            continue;
                        }
                        let start = decode_payload::<RequestStart>(&frame.payload)
                            .context("decode request start")?;
                        let (body_tx, body_rx) = mpsc::channel::<Result<Bytes, io::Error>>(64);
                        let tx = frame_tx.clone();
                        let http = http.clone();
                        let target = target.to_string();
                        let task = tokio::spawn(async move {
                            forward_one_request(http, tx, target, frame.stream_id, start, body_rx).await;
                        });
                        pending.insert(frame.stream_id, PendingRequest {
                            body_tx: Some(body_tx),
                            task,
                        });
                    }
                    FrameType::RequestBody => {
                        if let Some(request) = pending.get_mut(&frame.stream_id) {
                            if let Some(body_tx) = request.body_tx.as_ref() {
                                let _ = body_tx
                                    .send(Ok(Bytes::from(frame.payload)))
                                    .await;
                            }
                        }
                    }
                    FrameType::RequestEnd => {
                        if let Some(request) = pending.get_mut(&frame.stream_id) {
                            request.body_tx.take();
                        }
                    }
                    FrameType::Cancel => {
                        if let Some(request) = pending.remove(&frame.stream_id) {
                            request.task.abort();
                        }
                        pending_ws.remove(&frame.stream_id);
                    }
                    FrameType::Goaway => {
                        last_disconnect_reason = Some(String::from_utf8_lossy(&frame.payload).to_string());
                        goaway_received = true;
                        if pending.is_empty() && pending_ws.is_empty() {
                            writer.abort();
                            return Ok(ConnectionExit::Disconnected {
                                reconnect_token: next_reconnect_token,
                                stats: ConnectionStats {
                                    active_streams: 0,
                                    bytes_in,
                                    bytes_out: bytes_out.load(Ordering::Relaxed),
                                    last_disconnect_reason,
                                },
                            });
                        }
                    }
                    FrameType::WsOpen => {
                        if goaway_received {
                            let _ = frame_tx
                                .send(Frame::new(
                                    FrameType::Cancel,
                                    frame.stream_id,
                                    b"goaway_received".to_vec(),
                                ))
                                .await;
                            continue;
                        }
                        let open = decode_payload::<WsOpen>(&frame.payload)
                            .context("decode websocket open")?;
                        let (ws_tx, ws_rx) = mpsc::channel::<Message>(256);
                        pending_ws.insert(frame.stream_id, PendingWs { tx: ws_tx });
                        let tx = frame_tx.clone();
                        let target = target.to_string();
                        tokio::spawn(async move {
                            forward_one_websocket(tx, target, frame.stream_id, open, ws_rx).await;
                        });
                    }
                    FrameType::WsMessage => {
                        if let Some(ws) = pending_ws.get(&frame.stream_id) {
                            if let Ok(message) = frame_to_ws_message(&frame.payload) {
                                let _ = ws.tx.send(message).await;
                            }
                        }
                    }
                    FrameType::WsClose => {
                        if let Some(ws) = pending_ws.remove(&frame.stream_id) {
                            let _ = ws.tx.send(frame_to_ws_close(&frame.payload)).await;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    writer.abort();
    if last_disconnect_reason.is_none() {
        last_disconnect_reason = Some("websocket_closed".to_string());
    }
    Ok(ConnectionExit::Disconnected {
        reconnect_token: next_reconnect_token,
        stats: ConnectionStats {
            active_streams: pending.len() + pending_ws.len(),
            bytes_in,
            bytes_out: bytes_out.load(Ordering::Relaxed),
            last_disconnect_reason,
        },
    })
}

fn emit_client_event<T: Serialize>(json_events: bool, event: &str, data: &T) -> anyhow::Result<()> {
    if json_events {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "event": event,
                "data": data,
            }))?
        );
    } else if event == "connected" {
        println!("connected");
    }
    Ok(())
}

fn log_client_source_result(
    json_events: bool,
    phase: &str,
    report: Option<&ClientSourceReport>,
    public_ip_lookup: &PublicIpLookup,
) -> anyhow::Result<()> {
    if json_events {
        emit_client_event(
            json_events,
            "client_source",
            &serde_json::json!({
                "phase": phase,
                "available": report.is_some(),
                "public_ip": report.map(|report| report.public_ip.as_str()),
                "country_code": report.and_then(|report| report.country_code.as_deref()),
                "country": report.and_then(|report| report.country.as_deref()),
                "checked_at_unix_seconds": report.and_then(|report| report.checked_at_unix_seconds),
                "lookup_urls": &public_ip_lookup.urls,
                "country_lookup_urls": country_lookup_urls_for_report(report),
            }),
        )?;
        return Ok(());
    }

    let lookup_urls = public_ip_lookup.urls.join(",");
    if let Some(report) = report {
        println!(
            "client source ({phase}): ip={} country_code={} country={} lookup_urls={lookup_urls} country_lookup_urls={}",
            report.public_ip,
            report.country_code.as_deref().unwrap_or("unknown"),
            report.country.as_deref().unwrap_or("unknown"),
            country_lookup_urls_for_report(Some(report)).join(","),
        );
    } else {
        println!(
            "client source ({phase}): unavailable lookup_urls={lookup_urls} country_lookup_urls={}",
            country_lookup_urls_for_report(None).join(",")
        );
    }
    Ok(())
}

fn country_lookup_urls_for_report(report: Option<&ClientSourceReport>) -> Vec<String> {
    let Some(public_ip) = report.map(|report| report.public_ip.as_str()) else {
        return DEFAULT_PUBLIC_IP_COUNTRY_LOOKUP_URL_TEMPLATES
            .iter()
            .map(|template| (*template).to_string())
            .collect();
    };
    DEFAULT_PUBLIC_IP_COUNTRY_LOOKUP_URL_TEMPLATES
        .iter()
        .map(|template| public_ip_country_lookup_url(template, public_ip))
        .collect()
}

fn apply_runtime_stats(
    runtime_status: &mut RuntimeStatus,
    connected: bool,
    stats: &ConnectionStats,
) {
    runtime_status.connected = connected;
    runtime_status.active_streams = stats.active_streams;
    runtime_status.bytes_in = stats.bytes_in;
    runtime_status.bytes_out = stats.bytes_out;
    runtime_status.last_disconnect_reason = stats.last_disconnect_reason.clone();
    runtime_status.mark_updated();
}

fn next_reconnect_delay(current: std::time::Duration) -> std::time::Duration {
    let next = match current.as_secs() {
        0 | 1 => 2,
        2 => 5,
        3..=5 => 10,
        _ => 30,
    };
    std::time::Duration::from_secs(next)
}

async fn lookup_client_source(public_ip_lookup: &PublicIpLookup) -> Option<ClientSourceReport> {
    let lookup = lookup_public_ip(public_ip_lookup).await?;
    let public_ip = lookup.public_ip?;
    Some(ClientSourceReport {
        public_ip,
        country_code: lookup.country_code,
        country: lookup.country,
        checked_at_unix_seconds: Some(unix_now()),
    })
}

async fn lookup_public_ip(public_ip_lookup: &PublicIpLookup) -> Option<PublicIpLookupResponse> {
    if public_ip_lookup.urls.is_empty() {
        return None;
    }
    let client = reqwest::Client::new();
    let mut best = PublicIpLookupResponse::default();
    for url in &public_ip_lookup.urls {
        let request = client.get(url);
        let response = match tokio::time::timeout(PUBLIC_IP_LOOKUP_TIMEOUT, request.send()).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                tracing::debug!(%url, %error, "public IP lookup request failed");
                continue;
            }
            Err(_) => {
                tracing::debug!(%url, "public IP lookup request timed out");
                continue;
            }
        };
        let body = match tokio::time::timeout(PUBLIC_IP_LOOKUP_TIMEOUT, response.text()).await {
            Ok(Ok(body)) => body,
            Ok(Err(error)) => {
                tracing::debug!(%url, %error, "public IP lookup response read failed");
                continue;
            }
            Err(_) => {
                tracing::debug!(%url, "public IP lookup response read timed out");
                continue;
            }
        };
        let parsed = parse_public_ip_response(&body);
        if best.public_ip.is_none() {
            best.public_ip = parsed.public_ip;
        }
        if best.country_code.is_none() {
            best.country_code = parsed.country_code;
        }
        if best.country.is_none() {
            best.country = parsed.country;
        }
        if best.public_ip.is_some() && best.country_code.is_some() {
            return Some(best);
        }
        tracing::debug!(%url, "public IP lookup returned no public IP");
    }
    if best.country_code.is_none() {
        if let Some(public_ip) = best.public_ip.as_deref() {
            if let Some(country_lookup) = lookup_public_ip_country(&client, public_ip).await {
                if best.country_code.is_none() {
                    best.country_code = country_lookup.country_code;
                }
                if best.country.is_none() {
                    best.country = country_lookup.country;
                }
            }
        }
    }
    best.public_ip.as_ref()?;
    Some(best)
}

async fn lookup_public_ip_country(
    client: &reqwest::Client,
    public_ip: &str,
) -> Option<PublicIpLookupResponse> {
    let requested_ip = parse_public_ip(public_ip)?;
    for template in DEFAULT_PUBLIC_IP_COUNTRY_LOOKUP_URL_TEMPLATES {
        let url = public_ip_country_lookup_url(template, public_ip);
        let request = client.get(&url);
        let response = match tokio::time::timeout(PUBLIC_IP_LOOKUP_TIMEOUT, request.send()).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                tracing::debug!(%url, %error, "public IP country lookup request failed");
                continue;
            }
            Err(_) => {
                tracing::debug!(%url, "public IP country lookup request timed out");
                continue;
            }
        };
        let body = match tokio::time::timeout(PUBLIC_IP_LOOKUP_TIMEOUT, response.text()).await {
            Ok(Ok(body)) => body,
            Ok(Err(error)) => {
                tracing::debug!(%url, %error, "public IP country lookup response read failed");
                continue;
            }
            Err(_) => {
                tracing::debug!(%url, "public IP country lookup response read timed out");
                continue;
            }
        };
        let mut parsed = parse_public_ip_response(&body);
        if let Some(response_ip) = parsed.public_ip.as_deref().and_then(parse_public_ip) {
            if response_ip != requested_ip {
                tracing::debug!(
                    %url,
                    requested_ip = %requested_ip,
                    response_ip = %response_ip,
                    "public IP country lookup returned mismatched IP"
                );
                continue;
            }
        }
        if parsed.country_code.is_none() && parsed.country.is_none() {
            tracing::debug!(%url, "public IP country lookup returned no country");
            continue;
        }
        parsed.public_ip = Some(requested_ip.to_string());
        return Some(parsed);
    }
    None
}

fn public_ip_country_lookup_url(template: &str, public_ip: &str) -> String {
    template.replace("{ip}", public_ip)
}

fn parse_public_ip_response(body: &str) -> PublicIpLookupResponse {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        return parse_public_ip_json(&value);
    }
    PublicIpLookupResponse {
        public_ip: parse_public_ip_candidate(body),
        ..PublicIpLookupResponse::default()
    }
}

fn parse_public_ip_json(value: &serde_json::Value) -> PublicIpLookupResponse {
    let public_ip = value
        .get("ip")
        .and_then(|value| value.as_str())
        .and_then(parse_public_ip_candidate)
        .or_else(|| {
            value
                .get("origin")
                .and_then(|value| value.as_str())
                .and_then(parse_public_ip_candidate)
        });
    let country_code = first_json_string(value, &["country_code", "countryCode"])
        .and_then(normalize_country_code)
        .or_else(|| first_json_string(value, &["country"]).and_then(country_code_from_json_country))
        .or_else(|| {
            first_json_string(value, &["location"])
                .and_then(country_from_location)
                .map(|(code, _)| code.to_string())
        });
    let country = first_json_string(value, &["country_name", "countryName"])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            first_json_string(value, &["country"]).and_then(|value| {
                normalize_country_code(value)
                    .is_none()
                    .then(|| value.trim().to_string())
            })
        })
        .or_else(|| {
            first_json_string(value, &["location"])
                .and_then(country_from_location)
                .map(|(_, country)| country)
        });

    PublicIpLookupResponse {
        public_ip,
        country_code,
        country,
    }
}

fn first_json_string<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| value.get(*key)?.as_str())
}

fn country_code_from_json_country(value: &str) -> Option<String> {
    normalize_country_code(value).or_else(|| country_code_from_name(value).map(ToString::to_string))
}

fn parse_public_ip_candidate(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_matches('"')
        .split(',')
        .next()
        .unwrap_or_default()
        .trim();
    parse_public_ip(value).map(|ip| ip.to_string())
}

fn normalize_public_ip_lookup_url(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    if value.starts_with("http://") || value.starts_with("https://") {
        value.to_string()
    } else {
        format!("http://{value}")
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

pub(crate) fn tunnel_ws_url(server: &str, tunnel_id: &str, token: &str) -> anyhow::Result<String> {
    let server = server.trim_end_matches('/');
    let scheme = if server.starts_with("https://") {
        "wss"
    } else if server.starts_with("http://") {
        "ws"
    } else {
        anyhow::bail!("server must start with http:// or https://");
    };
    let rest = server
        .strip_prefix("https://")
        .or_else(|| server.strip_prefix("http://"))
        .expect("validated prefix");
    Ok(format!(
        "{scheme}://{rest}/api/v1/tunnels/{tunnel_id}/connect?token={token}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_stored_tunnel_errors_are_classified_narrowly() {
        assert!(stored_tunnel_error_is_terminal(&anyhow::anyhow!(
            "HTTP error: 401 Unauthorized"
        )));
        assert!(stored_tunnel_error_is_terminal(&anyhow::anyhow!(
            "HTTP error: 410 Gone tunnel_expired"
        )));
        assert!(!stored_tunnel_error_is_terminal(&anyhow::anyhow!(
            "connection refused"
        )));
    }

    #[test]
    fn duplicate_replaced_disconnect_is_terminal() {
        assert!(duplicate_replaced(&ConnectionStats {
            last_disconnect_reason: Some("duplicate_replaced".to_string()),
            ..ConnectionStats::default()
        }));
        assert!(!duplicate_replaced(&ConnectionStats {
            last_disconnect_reason: Some("websocket_closed".to_string()),
            ..ConnectionStats::default()
        }));
        assert!(!duplicate_replaced(&ConnectionStats::default()));
    }

    #[test]
    fn tunnel_expired_disconnect_is_terminal() {
        assert!(tunnel_expired(&ConnectionStats {
            last_disconnect_reason: Some("tunnel_expired".to_string()),
            ..ConnectionStats::default()
        }));
        assert!(!tunnel_expired(&ConnectionStats {
            last_disconnect_reason: Some("websocket_closed".to_string()),
            ..ConnectionStats::default()
        }));
    }

    #[test]
    fn parses_public_ip_lookup_responses() {
        assert_eq!(
            parse_public_ip_response(r#"{"ip":"183.193.183.90","location":"x"}"#),
            PublicIpLookupResponse {
                public_ip: Some("183.193.183.90".to_string()),
                country_code: None,
                country: None,
            }
        );
        assert_eq!(
            parse_public_ip_response(r#"{"ip":"183.193.183.90","location":"中国–上海–上海 移动"}"#),
            PublicIpLookupResponse {
                public_ip: Some("183.193.183.90".to_string()),
                country_code: Some("CN".to_string()),
                country: Some("中国".to_string()),
            }
        );
        assert_eq!(
            parse_public_ip_response(
                r#"{"ip":"2409:8a1e:c920:d640:b073:49ff:fee4:a804","success":true,"country":"China","country_code":"CN"}"#,
            ),
            PublicIpLookupResponse {
                public_ip: Some("2409:8a1e:c920:d640:b073:49ff:fee4:a804".to_string()),
                country_code: Some("CN".to_string()),
                country: Some("China".to_string()),
            }
        );
        assert_eq!(
            parse_public_ip_response(r#"{"origin":"8.8.8.8, 1.1.1.1"}"#),
            PublicIpLookupResponse {
                public_ip: Some("8.8.8.8".to_string()),
                country_code: None,
                country: None,
            }
        );
        assert_eq!(
            parse_public_ip_response("2606:4700:4700::1111\n"),
            PublicIpLookupResponse {
                public_ip: Some("2606:4700:4700::1111".to_string()),
                country_code: None,
                country: None,
            }
        );
        assert_eq!(
            parse_public_ip_response(r#"{"ip":"127.0.0.1"}"#),
            PublicIpLookupResponse::default()
        );
    }

    #[test]
    fn normalizes_public_ip_lookup_urls() {
        assert_eq!(normalize_public_ip_lookup_url("3.0.3.0"), "http://3.0.3.0");
        assert_eq!(
            normalize_public_ip_lookup_url("https://api.ipify.org?format=json"),
            "https://api.ipify.org?format=json"
        );
    }

    #[test]
    fn builds_public_ip_country_lookup_urls() {
        assert_eq!(
            public_ip_country_lookup_url(
                "https://ipwho.is/{ip}",
                "2409:8a1e:c920:d640:b073:49ff:fee4:a804"
            ),
            "https://ipwho.is/2409:8a1e:c920:d640:b073:49ff:fee4:a804"
        );
        assert_eq!(
            public_ip_country_lookup_url(
                "https://api.country.is/{ip}",
                "2409:8a1e:c920:d640:b073:49ff:fee4:a804"
            ),
            "https://api.country.is/2409:8a1e:c920:d640:b073:49ff:fee4:a804"
        );
        assert_eq!(
            parse_public_ip_response(
                r#"{"ip":"2409:8a1e:c920:d640:b073:49ff:fee4:a804","country":"CN"}"#,
            ),
            PublicIpLookupResponse {
                public_ip: Some("2409:8a1e:c920:d640:b073:49ff:fee4:a804".to_string()),
                country_code: Some("CN".to_string()),
                country: None,
            }
        );
    }
}
