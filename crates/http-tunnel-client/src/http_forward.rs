use anyhow::Context;
use bytes::Bytes;
use futures_util::StreamExt;
use http_tunnel_protocol::{
    types::{encode_payload, ErrorPayload, RequestStart, ResponseStart},
    Frame, FrameType,
};
use std::io;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

pub async fn forward_one_request(
    http: reqwest::Client,
    tx: mpsc::Sender<Frame>,
    target: String,
    stream_id: u64,
    start: RequestStart,
    body_rx: mpsc::Receiver<Result<Bytes, io::Error>>,
) {
    if let Err(error) =
        forward_one_request_inner(http, &tx, target, stream_id, start, body_rx).await
    {
        let payload = encode_payload(&ErrorPayload {
            code: "local_target_failed".to_string(),
            message: error.to_string(),
        })
        .unwrap_or_default();
        let _ = tx
            .send(Frame::new(FrameType::Error, stream_id, payload))
            .await;
    }
}

async fn forward_one_request_inner(
    http: reqwest::Client,
    tx: &mpsc::Sender<Frame>,
    target: String,
    stream_id: u64,
    start: RequestStart,
    body_rx: mpsc::Receiver<Result<Bytes, io::Error>>,
) -> anyhow::Result<()> {
    let url = format!("{}{}", target.trim_end_matches('/'), start.path);
    let method =
        reqwest::Method::from_bytes(start.method.as_bytes()).context("invalid request method")?;
    let body = reqwest::Body::wrap_stream(ReceiverStream::new(body_rx));
    let mut builder = http.request(method, url).body(body);
    for (name, value) in start.headers {
        let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        if http_tunnel_common::headers::is_hop_by_hop(&name) {
            continue;
        }
        let Ok(value) = reqwest::header::HeaderValue::from_str(&value) else {
            continue;
        };
        builder = builder.header(name, value);
    }

    let response = builder
        .send()
        .await
        .context("local target request failed")?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter(|(name, _)| !http_tunnel_common::headers::is_hop_by_hop(name))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect::<Vec<_>>();
    tx.send(Frame::new(
        FrameType::ResponseStart,
        stream_id,
        encode_payload(&ResponseStart { status, headers }).context("encode response start")?,
    ))
    .await
    .context("send response start")?;
    let mut body_stream = response.bytes_stream();
    while let Some(chunk) = body_stream.next().await {
        let chunk = chunk.context("read local target response chunk")?;
        if chunk.is_empty() {
            continue;
        }
        tx.send(Frame::new(
            FrameType::ResponseBody,
            stream_id,
            chunk.to_vec(),
        ))
        .await
        .context("send response body")?;
    }
    tx.send(Frame::new(FrameType::ResponseEnd, stream_id, Vec::new()))
        .await
        .context("send response end")?;
    Ok(())
}
