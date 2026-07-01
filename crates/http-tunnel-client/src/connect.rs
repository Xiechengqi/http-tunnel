use crate::{
    config::{clear_stored_tunnel, save_config_file, ClientConfig},
    http_forward::forward_one_request,
    runtime::{clear_disconnect_request, disconnect_requested, write_status, RuntimeStatus},
    ws_forward::{forward_one_websocket, frame_to_ws_close, frame_to_ws_message},
};
use anyhow::Context;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_tunnel_common::api::ApiResponse;
use http_tunnel_protocol::{
    decode_frame, encode_frame,
    types::{decode_payload, encode_payload, Hello, HelloAck, RequestStart, WsOpen},
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
    let client = reqwest::Client::new();
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
            cfg.create_token.as_deref(),
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
                        cfg.create_token.as_deref(),
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
    create_token: Option<&str>,
) -> anyhow::Result<CreateTunnelResponse> {
    let create_url = format!("{}/api/v1/tunnels", server.trim_end_matches('/'));
    let mut request = client.post(create_url).json(&serde_json::json!({
        "subdomain": subdomain,
    }));
    if let Some(token) = create_token.filter(|token| !token.trim().is_empty()) {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .context("create tunnel request failed")?
        .error_for_status()
        .context("create tunnel returned error status")?
        .json::<ApiResponse<CreateTunnelResponse>>()
        .await
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
}
