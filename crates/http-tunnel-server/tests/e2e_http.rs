use axum::{
    body::{to_bytes, Body},
    extract::{
        ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade},
        Request,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_tunnel_common::token::hash_token;
use http_tunnel_protocol::{
    decode_frame, encode_frame,
    types::{decode_payload, encode_payload, Hello, HelloAck, RequestStart, ResponseStart},
    version::VERSION as PROTOCOL_VERSION,
    Frame, FrameType,
};
use reqwest::header::HOST;
use serde_json::Value;
use sqlx::Row;
use std::{
    collections::HashSet,
    convert::Infallible,
    io::Cursor,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, http::HeaderValue, Message as TungsteniteMessage,
};

struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TargetGuard {
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TargetGuard {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

#[test]
fn dashboard_admin_static_has_policy_form_and_no_prompt_editors() {
    let admin_source =
        std::fs::read_to_string(workspace_root().join("dashboard/app/admin/admin-console.tsx"))
            .expect("read dashboard admin source");
    assert!(!admin_source.contains("prompt("));
    assert!(admin_source.contains("Toggle inspector"));
    assert!(admin_source.contains("Rotate tunnel token"));
    assert!(admin_source.contains("Download backup"));
    assert!(admin_source.contains("Diagnostics"));
    assert!(admin_source.contains("ConfigFieldSchema"));
    assert!(admin_source.contains("allowed_values"));
    assert!(admin_source.contains("/api/admin/requests/"));
    assert!(admin_source.contains("/api/admin/diagnostics"));
    assert!(admin_source.contains("/api/admin/config/schema"));
    assert!(admin_source.contains("/api/admin/alerts"));
    assert!(admin_source.contains("/replay"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_dashboard_lists_tunnels_without_admin_fields() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("public-dashboard");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;
    let http = reqwest::Client::new();
    let _ = create_tunnel(&http, server_port, "publicdash").await;

    let response: Value = http
        .get(format!("http://127.0.0.1:{server_port}/api/v1/dashboard"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["ok"], true);
    let data = &response["data"];
    assert_eq!(data["server_url"], "http://127.0.0.1");
    let tunnel = data["tunnels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tunnel| tunnel["subdomain"] == "publicdash")
        .expect("public dashboard should list created tunnel");
    assert_eq!(tunnel["url"], "http://publicdash.127.0.0.1");
    assert!(tunnel["source"]["label"].as_str().is_some());

    let raw = serde_json::to_string(data).unwrap();
    for forbidden in [
        "access_policy",
        "access_token",
        "access_username",
        "allowed_methods",
        "blocked_path_prefixes",
        "inspector",
        "rate_limit",
        "client_ip",
        "remote_ip",
        "token_hash",
        "database",
        "protocol_version",
    ] {
        assert!(
            !raw.contains(forbidden),
            "leaked public dashboard field {forbidden}"
        );
    }
}

#[test]
fn initial_schema_declares_request_log_and_session_indexes() {
    let schema = std::fs::read_to_string(workspace_root().join("schema/initial.sql"))
        .expect("read initial schema");
    for index in [
        "idx_request_logs_tunnel_started",
        "idx_request_logs_error_started",
        "idx_request_logs_status_started",
        "idx_sessions_tunnel_connected",
    ] {
        assert!(schema.contains(index), "missing {index}");
    }
    assert!(schema.contains("schema_versions"));
    let db_rs =
        std::fs::read_to_string(workspace_root().join("crates/http-tunnel-server/src/db.rs"))
            .expect("read db module");
    assert!(db_rs.contains("schema/initial.sql"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_forwards_get_and_post() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("forward");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "demo");
    let http = reqwest::Client::new();

    let get = wait_for_tunnel_get(&http, server_port, "demo", "/hello?x=1").await;
    assert_eq!(get.status(), StatusCode::OK);
    assert_eq!(get.headers().get("x-target").unwrap(), "ok");
    let body = get.text().await.unwrap();
    assert!(body.contains("method=GET"));
    assert!(body.contains("path=/hello?x=1"));
    assert!(body.contains("subdomain=demo"));

    let post = http
        .post(format!("http://127.0.0.1:{server_port}/submit"))
        .header(HOST, "demo.127.0.0.1")
        .body("abc123")
        .send()
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::CREATED);
    assert_eq!(post.headers().get("x-target").unwrap(), "ok");
    let body = post.text().await.unwrap();
    assert!(body.contains("method=POST"));
    assert!(body.contains("path=/submit"));
    assert!(body.contains("body=abc123"));
    assert!(body.contains("subdomain=demo"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_sets_forwarded_headers_and_logs_request_metadata() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("forwarded-metadata");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "forwarded");
    let http = reqwest::Client::new();
    let response = wait_for_tunnel_get(&http, server_port, "forwarded", "/headers").await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = http
        .get(format!("http://127.0.0.1:{server_port}/headers"))
        .header(HOST, "forwarded.127.0.0.1")
        .header("x-forwarded-for", "198.51.100.10")
        .header("connection", "x-dynamic-hop")
        .header("x-dynamic-hop", "remove-me")
        .header("user-agent", "metadata-test")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("forwarded_for=198.51.100.10, 127.0.0.1"));
    assert!(body.contains("forwarded_host=forwarded.127.0.0.1"));
    assert!(body.contains("forwarded_proto=http"));
    assert!(body.contains("dynamic_hop="));

    let token = server.login().await;
    let requests = wait_for_admin_requests(
        &http,
        server_port,
        &token,
        "/api/admin/requests?q=/headers&limit=5",
    )
    .await;
    let row = requests["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["user_agent"] == "metadata-test")
        .unwrap();
    assert_eq!(row["type"], "http");
    assert_eq!(row["remote_ip"], "198.51.100.10");
    assert_eq!(row["host"], "forwarded.127.0.0.1");
    assert!(row["request_id"].as_str().unwrap().starts_with("req_"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_streams_sse_response() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("sse");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "events");
    let http = reqwest::Client::new();
    let response = wait_for_tunnel_get(&http, server_port, "events", "/sse").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );

    let mut body = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), body.next())
        .await
        .expect("first SSE chunk timed out")
        .expect("missing first SSE chunk")
        .expect("failed to read first SSE chunk");
    let first = String::from_utf8_lossy(&first);
    assert!(first.contains("data: one"));

    let mut rest = String::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.unwrap();
        rest.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(rest.contains("data: two"));
    assert!(rest.contains("data: three"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_streams_request_body_chunks() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("request-stream");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "upload");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "upload", "/hello").await;

    let body = streaming_body(["chunk-", "body-", "ok"]);
    let response = http
        .post(format!("http://127.0.0.1:{server_port}/submit-stream"))
        .header(HOST, "upload.127.0.0.1")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response.text().await.unwrap();
    assert!(body.contains("body=chunk-body-ok"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_forwards_large_request_body() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("large-request");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "large");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "large", "/hello").await;

    let body = vec![b'a'; 1024 * 1024];
    let response = http
        .post(format!("http://127.0.0.1:{server_port}/len"))
        .header(HOST, "large.127.0.0.1")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.text().await.unwrap(), "len=1048576\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_rejects_request_body_over_limit() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("request-limit");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start_with_max_body(&workspace, &test_dir, server_port, 8).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "limit");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "limit", "/hello").await;

    let response = http
        .post(format!("http://127.0.0.1:{server_port}/submit"))
        .header(HOST, "limit.127.0.0.1")
        .body("this body is too large")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response.headers().get("x-http-tunnel-reason").unwrap(),
        "request_too_large"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_forwards_websocket_echo() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("websocket");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "ws");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "ws", "/hello").await;

    let mut request = format!("ws://127.0.0.1:{server_port}/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("host", HeaderValue::from_static("ws.127.0.0.1"));
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();

    ws.send(TungsteniteMessage::Text("hello".to_string()))
        .await
        .unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply, TungsteniteMessage::Text("hello".to_string()));

    ws.send(TungsteniteMessage::Binary(vec![1, 2, 3]))
        .await
        .unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply, TungsteniteMessage::Binary(vec![1, 2, 3]));

    ws.close(None).await.unwrap();

    let token = server.login().await;
    let mut last_ws_logs = None;
    let mut completed_row = None;
    for _ in 0..80 {
        let ws_logs = admin_get_json(
            &http,
            server_port,
            &token,
            "/api/admin/requests?type=ws&q=/ws&limit=5",
        )
        .await;
        if let Some(row) = ws_logs["data"].as_array().and_then(|rows| rows.first()) {
            let ws_message_count = row["ws_message_count"].as_i64().unwrap_or_default();
            let bytes_in = row["bytes_in"].as_i64().unwrap_or_default();
            if ws_message_count >= 2 && bytes_in >= 5 {
                completed_row = Some(row.clone());
                break;
            }
        }
        last_ws_logs = Some(ws_logs);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let row = completed_row
        .unwrap_or_else(|| panic!("websocket request metrics did not update: {last_ws_logs:?}"));
    assert_eq!(row["type"], "ws");
    assert_eq!(row["status"], 101);
    assert!(row["ws_message_count"].as_i64().unwrap() >= 2);
    assert!(row["bytes_in"].as_i64().unwrap() >= 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "operator smoke-soak harness; set HTTP_TUNNEL_SOAK_ITERATIONS to tune length"]
async fn smoke_soak_harness_exercises_http_sse_websocket_and_reconnect() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("smoke-soak");
    std::fs::create_dir_all(&test_dir).unwrap();
    let iterations = std::env::var("HTTP_TUNNEL_SOAK_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20)
        .max(1);

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;
    let _client = start_client(&workspace, server_port, target_port, "soak");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "soak", "/hello").await;
    let admin_token = server.login().await;

    for index in 0..iterations {
        let response = http
            .get(format!("http://127.0.0.1:{server_port}/hello"))
            .header(HOST, "soak.127.0.0.1")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = http
            .get(format!("http://127.0.0.1:{server_port}/sse"))
            .header(HOST, "soak.127.0.0.1")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.text().await.unwrap().contains("data: three"));

        let mut request = format!("ws://127.0.0.1:{server_port}/ws")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("host", HeaderValue::from_static("soak.127.0.0.1"));
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        ws.send(TungsteniteMessage::Text(format!("hello-{index}")))
            .await
            .unwrap();
        assert_eq!(
            ws.next().await.unwrap().unwrap(),
            TungsteniteMessage::Text(format!("hello-{index}"))
        );
        ws.close(None).await.unwrap();

        if index == iterations / 2 {
            let tunnels = admin_get_json(
                &http,
                server_port,
                &admin_token,
                "/api/admin/tunnels?q=soak",
            )
            .await;
            let tunnel_id = tunnels["data"][0]["id"].as_str().unwrap();
            let disconnected = http
                .post(format!(
                    "http://127.0.0.1:{server_port}/api/admin/tunnels/{tunnel_id}/disconnect"
                ))
                .bearer_auth(&admin_token)
                .send()
                .await
                .unwrap();
            assert_eq!(disconnected.status(), StatusCode::OK);
            let _ = wait_for_tunnel_get(&http, server_port, "soak", "/hello").await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_upgrade_waits_for_local_accept() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("websocket-reject");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "wsreject");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "wsreject", "/hello").await;

    let mut request = format!("ws://127.0.0.1:{server_port}/not-a-websocket")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("host", HeaderValue::from_static("wsreject.127.0.0.1"));
    let error = tokio_tungstenite::connect_async(request)
        .await
        .expect_err("websocket upgrade should fail before 101");
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        }
        other => panic!("expected HTTP 502 websocket rejection, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_lifecycle_errors_are_reported() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("errors");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;
    let pool = server_db(&server).await;
    let schema_versions = sqlx::query("SELECT version FROM schema_versions ORDER BY version")
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("version"))
        .collect::<Vec<_>>();
    assert_eq!(schema_versions, vec!["initial".to_string()]);

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "demo").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap().to_string();

    let duplicate = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .json(&serde_json::json!({"subdomain": "demo"}))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);

    let unknown = http
        .get(format!("http://127.0.0.1:{server_port}/"))
        .header(HOST, "missing.127.0.0.1")
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        unknown.headers().get("x-http-tunnel-reason").unwrap(),
        "tunnel_not_found"
    );

    let offline = http
        .get(format!("http://127.0.0.1:{server_port}/"))
        .header(HOST, "demo.127.0.0.1")
        .send()
        .await
        .unwrap();
    assert_eq!(offline.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        offline.headers().get("x-http-tunnel-reason").unwrap(),
        "tunnel_offline"
    );

    let token = server.login().await;
    let disabled = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels/{tunnel_id}/disable"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);

    let disabled_host = http
        .get(format!("http://127.0.0.1:{server_port}/"))
        .header(HOST, "demo.127.0.0.1")
        .send()
        .await
        .unwrap();
    assert_eq!(disabled_host.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        disabled_host.headers().get("x-http-tunnel-reason").unwrap(),
        "tunnel_disabled"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn public_tunnel_delete_requires_tunnel_token() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("delete-token");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "delete-token").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();

    let blocked = http
        .delete(format!(
            "http://127.0.0.1:{server_port}/api/v1/tunnels/{tunnel_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::UNAUTHORIZED);

    let allowed = http
        .delete(format!(
            "http://127.0.0.1:{server_port}/api/v1/tunnels/{tunnel_id}"
        ))
        .bearer_auth(tunnel_token)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);

    let deleted = http
        .get(format!(
            "http://127.0.0.1:{server_port}/api/v1/tunnels/{tunnel_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_release_deletes_stored_tunnel_and_clears_local_token() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("client-release");
    let client_home = unique_test_dir("client-release-home");
    std::fs::create_dir_all(&test_dir).unwrap();
    std::fs::create_dir_all(client_home.join(".http-tunnel")).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "release").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();
    let config_path = client_home.join(".http-tunnel/client.toml");
    std::fs::write(
        &config_path,
        format!(
            "server = \"http://127.0.0.1:{server_port}\"\ntunnel_id = \"{tunnel_id}\"\ntoken = \"{tunnel_token}\"\npersist_token = true\n"
        ),
    )
    .unwrap();

    let output = Command::new("cargo")
        .current_dir(&workspace)
        .args(["run", "-q", "-p", "http-tunnel-client", "--", "release"])
        .env("HOME", &client_home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "release failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let deleted = http
        .get(format!(
            "http://127.0.0.1:{server_port}/api/v1/tunnels/{tunnel_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);
    let saved_config = std::fs::read_to_string(config_path).unwrap();
    assert!(!saved_config.contains(tunnel_id));
    assert!(!saved_config.contains(tunnel_token));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_status_and_disconnect_control_runtime() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("client-runtime-control");
    let client_home = unique_test_dir("client-runtime-control-home");
    std::fs::create_dir_all(&test_dir).unwrap();
    std::fs::create_dir_all(client_home.join(".http-tunnel")).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;
    let child = Command::new("cargo")
        .current_dir(&workspace)
        .args([
            "run",
            "-q",
            "-p",
            "http-tunnel-client",
            "--",
            "connect",
            "--server",
            &format!("http://127.0.0.1:{server_port}"),
            "--subdomain",
            "runtime",
            "--target",
            &format!("http://127.0.0.1:{target_port}"),
        ])
        .env("HOME", &client_home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut guard = ChildGuard { child };

    let http = reqwest::Client::new();
    let response = wait_for_tunnel_get(&http, server_port, "runtime", "/hello").await;
    assert_eq!(response.status(), StatusCode::OK);

    let status_output = Command::new("cargo")
        .current_dir(&workspace)
        .args(["run", "-q", "-p", "http-tunnel-client", "--", "status"])
        .env("HOME", &client_home)
        .output()
        .unwrap();
    assert!(
        status_output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status_output.stderr)
    );
    let status: Value = serde_json::from_slice(&status_output.stdout).unwrap();
    assert_eq!(status["connected"], true);
    assert!(status["tunnel_id"].as_str().is_some());

    let disconnect_output = Command::new("cargo")
        .current_dir(&workspace)
        .args(["run", "-q", "-p", "http-tunnel-client", "--", "disconnect"])
        .env("HOME", &client_home)
        .output()
        .unwrap();
    assert!(
        disconnect_output.status.success(),
        "disconnect failed: {}",
        String::from_utf8_lossy(&disconnect_output.stderr)
    );

    for _ in 0..60 {
        if guard.child.try_wait().unwrap().is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("client did not exit after disconnect request");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_json_events_stdout_is_ndjson_only() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("client-json-events");
    let client_home = unique_test_dir("client-json-events-home");
    std::fs::create_dir_all(&test_dir).unwrap();
    std::fs::create_dir_all(client_home.join(".http-tunnel")).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;
    let mut child = Command::new("cargo")
        .current_dir(&workspace)
        .args([
            "run",
            "-q",
            "-p",
            "http-tunnel-client",
            "--",
            "connect",
            "--server",
            &format!("http://127.0.0.1:{server_port}"),
            "--subdomain",
            "jsonevents",
            "--target",
            &format!("http://127.0.0.1:{target_port}"),
            "--json-events",
        ])
        .env("HOME", &client_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let http = reqwest::Client::new();
    let response = wait_for_tunnel_get(&http, server_port, "jsonevents", "/hello").await;
    assert_eq!(response.status(), StatusCode::OK);

    let disconnect_output = Command::new("cargo")
        .current_dir(&workspace)
        .args(["run", "-q", "-p", "http-tunnel-client", "--", "disconnect"])
        .env("HOME", &client_home)
        .output()
        .unwrap();
    assert!(
        disconnect_output.status.success(),
        "disconnect failed: {}",
        String::from_utf8_lossy(&disconnect_output.stderr)
    );

    for _ in 0..60 {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(!stdout.contains("public url:"));
            assert!(!stdout.contains("target:"));
            let events = stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert!(events.iter().any(|event| event["event"] == "startup"));
            assert!(events.iter().any(|event| event["event"] == "connected"));
            assert!(events.iter().any(|event| event["event"] == "exit"));
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let _ = child.kill();
    panic!("json-events client did not exit after disconnect request");
}

#[test]
fn client_status_marks_missing_runtime_pid_as_stale() {
    let workspace = workspace_root();
    let client_home = unique_test_dir("client-runtime-stale-home");
    let runtime_dir = client_home.join(".http-tunnel");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    std::fs::write(
        runtime_dir.join("runtime.json"),
        serde_json::to_vec(&serde_json::json!({
            "pid": 99999999u32,
            "server": "http://127.0.0.1:1",
            "target": "http://127.0.0.1:2",
            "tunnel_id": "tun_fake",
            "public_url": "http://fake.127.0.0.1",
            "connected": true,
            "active_streams": 1,
            "bytes_in": 0,
            "bytes_out": 0,
            "last_disconnect_reason": null,
            "updated_at_unix": 1
        }))
        .unwrap(),
    )
    .unwrap();

    let output = Command::new("cargo")
        .current_dir(&workspace)
        .args(["run", "-q", "-p", "http-tunnel-client", "--", "status"])
        .env("HOME", &client_home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["stale"], true);
    assert_eq!(status["connected"], false);
    assert_eq!(status["last_disconnect_reason"], "stale_runtime");

    std::fs::write(runtime_dir.join("disconnect"), "test").unwrap();
    let clean_output = Command::new("cargo")
        .current_dir(&workspace)
        .args([
            "run",
            "-q",
            "-p",
            "http-tunnel-client",
            "--",
            "runtime",
            "clean",
        ])
        .env("HOME", &client_home)
        .output()
        .unwrap();
    assert!(
        clean_output.status.success(),
        "runtime clean failed: {}",
        String::from_utf8_lossy(&clean_output.stderr)
    );
    let clean: Value = serde_json::from_slice(&clean_output.stdout).unwrap();
    assert_eq!(clean["status_removed"], true);
    assert_eq!(clean["disconnect_flag_removed"], true);
    assert!(!runtime_dir.join("runtime.json").exists());
    assert!(!runtime_dir.join("disconnect").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_token_rotate_invalidates_old_token_and_returns_new_one() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("rotate-token");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "rotate").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let old_token = created["data"]["token"].as_str().unwrap();

    let unauthenticated = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels/{tunnel_id}/token/rotate"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let admin_token = server.login().await;
    let rotated: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels/{tunnel_id}/token/rotate"
        ))
        .bearer_auth(admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let new_token = rotated["data"]["token"].as_str().unwrap();
    assert_ne!(old_token, new_token);

    let old_token_delete = http
        .delete(format!(
            "http://127.0.0.1:{server_port}/api/v1/tunnels/{tunnel_id}"
        ))
        .bearer_auth(old_token)
        .send()
        .await
        .unwrap();
    assert_eq!(old_token_delete.status(), StatusCode::UNAUTHORIZED);

    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{server_port}/api/v1/tunnels/{tunnel_id}/connect?token={new_token}"
    ))
    .await
    .unwrap();
    ws.close(None).await.unwrap();

    let new_token_delete = http
        .delete(format!(
            "http://127.0.0.1:{server_port}/api/v1/tunnels/{tunnel_id}"
        ))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap();
    assert_eq!(new_token_delete.status(), StatusCode::OK);

    let audit = admin_get_json(
        &http,
        server_port,
        &server.login().await,
        "/api/admin/audit?action=tunnel_token_rotate",
    )
    .await;
    assert!(audit["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| { row["target_id"] == tunnel_id && row["result"] == "success" }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_patch_updates_tunnel_ttl_and_can_expire_now() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("patch-tunnel");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "patchable").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let token = server.login().await;

    let patched: Value = http
        .patch(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels/{tunnel_id}"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"ttl_seconds": 600}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patched["data"]["subdomain"], "patchable");

    let expired: Value = http
        .patch(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels/{tunnel_id}"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"expire_now": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(expired["data"]["status"], "expired");

    let proxy = http
        .get(format!("http://127.0.0.1:{server_port}/"))
        .header(HOST, "patchable.127.0.0.1")
        .send()
        .await
        .unwrap();
    assert_eq!(proxy.status(), StatusCode::GONE);

    let missing = http
        .patch(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels/tun_missing"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"ttl_seconds": 600}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let audit = admin_get_json(
        &http,
        server_port,
        &token,
        "/api/admin/audit?action=tunnel_patch&result=failure",
    )
    .await;
    assert!(audit["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["target_id"] == "tun_missing"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn access_control_inspector_and_replay_work() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("access-inspector-replay");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;
    let _client = start_client(&workspace, server_port, target_port, "secure");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "secure", "/hello").await;

    let token = server.login().await;
    let listed = admin_get_json(&http, server_port, &token, "/api/admin/tunnels?q=secure").await;
    let tunnel_id = listed["data"][0]["id"].as_str().unwrap();
    let patched = http
        .patch(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels/{tunnel_id}"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "inspector_enabled": true,
            "access_policy": "bearer",
            "access_token": "front-door",
            "allowed_methods": ["GET"],
            "blocked_path_prefixes": ["/blocked"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::OK);

    let unauthorized = http
        .get(format!("http://127.0.0.1:{server_port}/hello"))
        .header(HOST, "secure.127.0.0.1")
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let method_blocked = http
        .post(format!("http://127.0.0.1:{server_port}/submit"))
        .header(HOST, "secure.127.0.0.1")
        .bearer_auth("front-door")
        .body("blocked")
        .send()
        .await
        .unwrap();
    assert_eq!(method_blocked.status(), StatusCode::METHOD_NOT_ALLOWED);

    let path_blocked = http
        .get(format!("http://127.0.0.1:{server_port}/blocked/path"))
        .header(HOST, "secure.127.0.0.1")
        .bearer_auth("front-door")
        .send()
        .await
        .unwrap();
    assert_eq!(path_blocked.status(), StatusCode::FORBIDDEN);

    let blocked_logs = wait_for_admin_requests(
        &http,
        server_port,
        &token,
        "/api/admin/requests?type=blocked&limit=10&q=secure.127.0.0.1",
    )
    .await;
    let blocked_rows = blocked_logs["data"].as_array().unwrap();
    assert!(blocked_rows
        .iter()
        .any(|row| row["error"] == "access_token_required"));
    assert!(blocked_rows
        .iter()
        .any(|row| row["error"] == "method_not_allowed"));
    assert!(blocked_rows
        .iter()
        .any(|row| row["error"] == "path_blocked"));

    let allowed = http
        .get(format!("http://127.0.0.1:{server_port}/hello?inspect=1"))
        .header(HOST, "secure.127.0.0.1")
        .bearer_auth("front-door")
        .header("user-agent", "inspector-test")
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);

    let requests = wait_for_admin_requests(
        &http,
        server_port,
        &token,
        "/api/admin/requests?q=/hello?inspect=1&limit=5",
    )
    .await;
    let request_id = requests["data"][0]["id"].as_str().unwrap();
    let detail = admin_get_json(
        &http,
        server_port,
        &token,
        &format!("/api/admin/requests/{request_id}"),
    )
    .await;
    assert!(!detail["data"]["inspection"].is_null());
    assert!(detail["data"]["inspection"]["request_headers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|header| header["name"] == "authorization" && header["value"] == "[redacted]"));
    assert_eq!(
        detail["data"]["inspection"]["response_body_preview_encoding"],
        "utf8"
    );

    let replay: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/requests/{request_id}/replay"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["data"]["status"], 200);
    assert!(replay["data"]["body_preview"]
        .as_str()
        .unwrap()
        .contains("path=/hello?inspect=1"));

    let replay_override: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/requests/{request_id}/replay"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "method": "POST",
            "path": "/submit?replay=1",
            "headers": [{"name": "content-type", "value": "text/plain"}],
            "body": "override-body"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay_override["data"]["status"], 201);
    assert_eq!(replay_override["data"]["replay_of"], request_id);
    let override_body = replay_override["data"]["body_preview"].as_str().unwrap();
    assert!(override_body.contains("path=/submit?replay=1"));
    assert!(override_body.contains("body=override-body"));
    let replay_log_id = replay_override["data"]["request_id"].as_str().unwrap();
    let replay_detail = admin_get_json(
        &http,
        server_port,
        &token,
        &format!("/api/admin/requests/{replay_log_id}"),
    )
    .await;
    assert_eq!(replay_detail["data"]["type"], "http_replay");
    assert_eq!(replay_detail["data"]["replay_of"], request_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_tunnels_return_gone_for_proxy_and_connect() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("expired-gone");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "expired").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();
    mark_tunnel_expired(&server, tunnel_id).await;

    let proxy = http
        .get(format!("http://127.0.0.1:{server_port}/"))
        .header(HOST, "expired.127.0.0.1")
        .send()
        .await
        .unwrap();
    assert_eq!(proxy.status(), StatusCode::GONE);
    assert_eq!(
        proxy.headers().get("x-http-tunnel-reason").unwrap(),
        "tunnel_expired"
    );

    let connect = tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{server_port}/api/v1/tunnels/{tunnel_id}/connect?token={tunnel_token}"
    ))
    .await
    .expect_err("expired tunnel websocket should be rejected");
    match connect {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), StatusCode::GONE);
        }
        other => panic!("expected HTTP 410 websocket rejection, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creating_tunnel_expires_disconnected_records() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("expire-disconnected");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let stale = create_tunnel(&http, server_port, "stale").await;
    let stale_id = stale["data"]["id"].as_str().unwrap();
    let pool = server_db(&server).await;
    sqlx::query(
        "UPDATE tunnels SET status = 'disconnected', expires_at = datetime('now', '-1 second') WHERE id = ?1",
    )
    .bind(stale_id)
    .execute(&pool)
    .await
    .unwrap();

    let _ = create_tunnel(&http, server_port, "fresh").await;
    let row = sqlx::query("SELECT status FROM tunnels WHERE id = ?1")
        .bind(stale_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("status"), "expired");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeat_marks_nonresponsive_session_stale() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("heartbeat-stale");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[
            ("HTTP_TUNNEL_HEARTBEAT_INTERVAL_SECONDS", "1".to_string()),
            ("HTTP_TUNNEL_STALE_SESSION_SECONDS", "2".to_string()),
        ],
    )
    .await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "stale").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();
    let mut ws = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    let ack = send_tunnel_hello(&mut ws, None).await;
    assert!(ack.accepted);

    let token = server.login().await;
    let mut last_status = None;
    for _ in 0..30 {
        let detail = admin_get_json(
            &http,
            server_port,
            &token,
            &format!("/api/admin/tunnels/{tunnel_id}/detail"),
        )
        .await;
        last_status = detail["data"]["active_session"]["disconnect_reason"]
            .as_str()
            .map(ToString::to_string);
        if last_status.as_deref() == Some("stale_session") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("session was not marked stale, last status: {last_status:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_ws_requires_hello_before_stream_frames() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("hello-required");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "hello-required").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();
    let mut ws = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    let payload = encode_payload(&RequestStart {
        method: "GET".to_string(),
        path: "/before-hello".to_string(),
        headers: Vec::new(),
    })
    .unwrap();
    ws.send(TungsteniteMessage::Binary(
        encode_frame(&Frame::new(FrameType::RequestStart, 1, payload)).unwrap(),
    ))
    .await
    .unwrap();
    let frame = next_tunnel_frame(&mut ws).await;
    assert_eq!(frame.frame_type, FrameType::Goaway);
    assert_eq!(String::from_utf8_lossy(&frame.payload), "hello_required");

    let detail = admin_get_json(
        &http,
        server_port,
        &server.login().await,
        &format!("/api/admin/tunnels/{tunnel_id}/detail"),
    )
    .await;
    assert!(detail["data"]["active_session"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_tunnel_connection_replaces_old_session_by_default() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("duplicate-replace");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "dupreplace").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();
    let mut first = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    assert!(send_tunnel_hello(&mut first, None).await.accepted);
    let mut second = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    assert!(send_tunnel_hello(&mut second, None).await.accepted);

    let frame = next_tunnel_frame_of_type(&mut first, FrameType::Goaway).await;
    assert_eq!(frame.frame_type, FrameType::Goaway);
    assert_eq!(
        String::from_utf8_lossy(&frame.payload),
        "duplicate_replaced"
    );

    let events = admin_get_json(
        &http,
        server_port,
        &server.login().await,
        "/api/admin/events?kind=client_duplicate_replaced&limit=5",
    )
    .await;
    assert!(!events["data"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_tunnel_connection_can_be_rejected_by_config() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("duplicate-reject");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[("HTTP_TUNNEL_DUPLICATE_SESSION_POLICY", "reject".to_string())],
    )
    .await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "dupreject").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();
    let mut first = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    assert!(send_tunnel_hello(&mut first, None).await.accepted);
    let mut second = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    let ack = send_tunnel_hello(&mut second, None).await;
    assert!(!ack.accepted);
    assert_eq!(ack.message.as_deref(), Some("duplicate session"));
    let frame = next_tunnel_frame(&mut second).await;
    assert_eq!(frame.frame_type, FrameType::Goaway);
    assert_eq!(String::from_utf8_lossy(&frame.payload), "duplicate_session");

    let detail = admin_get_json(
        &http,
        server_port,
        &server.login().await,
        &format!("/api/admin/tunnels/{tunnel_id}/detail"),
    )
    .await;
    assert_eq!(
        detail["data"]["active_session"]["runtime_active"],
        Value::Bool(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn round_robin_session_pool_dispatches_across_sessions() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("round-robin-pool");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[("HTTP_TUNNEL_SESSION_POOL_POLICY", "round_robin".to_string())],
    )
    .await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "pool").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();
    let mut first = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    assert!(send_tunnel_hello(&mut first, None).await.accepted);
    let mut second = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    assert!(send_tunnel_hello(&mut second, None).await.accepted);

    let first_request = tokio::spawn({
        let http = http.clone();
        async move {
            http.get(format!("http://127.0.0.1:{server_port}/one"))
                .header(HOST, "pool.127.0.0.1")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }
    });
    let first_frame = next_tunnel_frame_of_type(&mut first, FrameType::RequestStart).await;
    send_tunnel_http_response(&mut first, first_frame.stream_id, "session=first").await;
    assert_eq!(first_request.await.unwrap(), "session=first");

    let second_request = tokio::spawn({
        let http = http.clone();
        async move {
            http.get(format!("http://127.0.0.1:{server_port}/two"))
                .header(HOST, "pool.127.0.0.1")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }
    });
    let second_frame = next_tunnel_frame_of_type(&mut second, FrameType::RequestStart).await;
    send_tunnel_http_response(&mut second, second_frame.stream_id, "session=second").await;
    assert_eq!(second_request.await.unwrap(), "session=second");

    let detail = admin_get_json(
        &http,
        server_port,
        &server.login().await,
        &format!("/api/admin/tunnels/{tunnel_id}/detail"),
    )
    .await;
    assert_eq!(
        detail["data"]["active_sessions"].as_array().unwrap().len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn least_loaded_session_pool_prefers_idle_session_and_cleans_failed_session() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("least-loaded-pool");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[(
            "HTTP_TUNNEL_SESSION_POOL_POLICY",
            "least_loaded".to_string(),
        )],
    )
    .await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "least").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();
    let mut first = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    assert!(send_tunnel_hello(&mut first, None).await.accepted);
    let mut second = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    assert!(send_tunnel_hello(&mut second, None).await.accepted);

    let held_request = tokio::spawn({
        let http = http.clone();
        async move {
            http.get(format!("http://127.0.0.1:{server_port}/held"))
                .header(HOST, "least.127.0.0.1")
                .send()
                .await
                .unwrap()
        }
    });
    let held_frame = next_tunnel_frame_of_type(&mut first, FrameType::RequestStart).await;
    assert!(held_frame.stream_id > 0);

    let fast_request = tokio::spawn({
        let http = http.clone();
        async move {
            http.get(format!("http://127.0.0.1:{server_port}/fast"))
                .header(HOST, "least.127.0.0.1")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }
    });
    let fast_frame = next_tunnel_frame_of_type(&mut second, FrameType::RequestStart).await;
    send_tunnel_http_response(&mut second, fast_frame.stream_id, "session=second").await;
    assert_eq!(fast_request.await.unwrap(), "session=second");

    let detail = admin_get_json(
        &http,
        server_port,
        &server.login().await,
        &format!("/api/admin/tunnels/{tunnel_id}/detail"),
    )
    .await;
    assert!(detail["data"]["active_sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|session| session["runtime_active_streams"].as_u64().unwrap_or(0) >= 1));

    drop(first);
    let _ = tokio::time::timeout(Duration::from_secs(5), held_request).await;
    wait_for_active_session_count(&http, server_port, &server.login().await, tunnel_id, 1).await;

    let recovered_request = tokio::spawn({
        let http = http.clone();
        async move {
            http.get(format!("http://127.0.0.1:{server_port}/recovered"))
                .header(HOST, "least.127.0.0.1")
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap()
        }
    });
    let recovered_frame = next_tunnel_frame_of_type(&mut second, FrameType::RequestStart).await;
    send_tunnel_http_response(&mut second, recovered_frame.stream_id, "session=recovered").await;
    assert_eq!(recovered_request.await.unwrap(), "session=recovered");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tampered_reconnect_token_is_reported_but_does_not_replace_tunnel_auth() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("reconnect-token-rejected");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "badreconnect").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let tunnel_token = created["data"]["token"].as_str().unwrap();
    let mut first = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    let ack = send_tunnel_hello(&mut first, None).await;
    assert!(ack.accepted);
    let token = ack.reconnect_token.unwrap();
    first.close(None).await.unwrap();

    let mut second = connect_tunnel_ws(server_port, tunnel_id, tunnel_token).await;
    let ack = send_tunnel_hello(&mut second, Some(format!("{token}x"))).await;
    assert!(ack.accepted);

    let events = admin_get_json(
        &http,
        server_port,
        &server.login().await,
        "/api/admin/events?kind=client_reconnect_token_rejected&limit=5",
    )
    .await;
    assert!(!events["data"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_login_sets_cookie_that_authorizes_api() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("admin-cookie");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let login = http
        .post(format!("http://127.0.0.1:{server_port}/api/admin/login"))
        .json(&serde_json::json!({"password": "password123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let status = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/status"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(status.headers().get("x-frame-options").unwrap(), "DENY");
    assert_eq!(
        status.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(
        status
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .unwrap(),
        "no-store, max-age=0"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_cookie_survives_server_restart() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("admin-cookie-restart");
    std::fs::create_dir_all(&test_dir).unwrap();

    let cookie = {
        let server = TestServer::start(&workspace, &test_dir, server_port).await;
        server.setup().await;
        let http = reqwest::Client::new();
        let login = http
            .post(format!("http://127.0.0.1:{server_port}/api/admin/login"))
            .json(&serde_json::json!({"password": "password123"}))
            .send()
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        login
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    };

    tokio::time::sleep(Duration::from_millis(500)).await;
    let _server = TestServer::start(&workspace, &test_dir, server_port).await;
    let http = reqwest::Client::new();
    let status = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/status"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_cookie_write_requires_csrf() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("admin-csrf");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let login = http
        .post(format!("http://127.0.0.1:{server_port}/api/admin/login"))
        .json(&serde_json::json!({"password": "password123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookies = login
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .map(|value| {
            value
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        })
        .collect::<Vec<_>>();
    let login_body: Value = login.json().await.unwrap();
    let token = login_body["data"]["token"].as_str().unwrap().to_string();
    let cookie_header = cookies.join("; ");
    let csrf = cookies
        .iter()
        .find_map(|cookie| cookie.strip_prefix("http_tunnel_csrf="))
        .unwrap()
        .to_string();

    let blocked = http
        .post(format!("http://127.0.0.1:{server_port}/api/admin/logout"))
        .header(reqwest::header::COOKIE, &cookie_header)
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    let csrf_audit = admin_get_json(
        &http,
        server_port,
        &token,
        "/api/admin/audit?action=csrf_check&result=failure",
    )
    .await;
    assert!(csrf_audit["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["detail"] == "missing or invalid CSRF token"));

    let allowed = http
        .post(format!("http://127.0.0.1:{server_port}/api/admin/logout"))
        .header(reqwest::header::COOKIE, cookie_header)
        .header("x-csrf-token", csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_sessions_can_be_listed_and_revoked() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("admin-sessions");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let first_token = server.login().await;
    let second_token = server.login().await;
    let sessions = admin_get_json(&http, server_port, &second_token, "/api/admin/sessions").await;
    let revoke_id = sessions["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["active"] == true && row["current"] == false)
        .and_then(|row| row["id"].as_str())
        .unwrap()
        .to_string();

    let revoked = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/sessions/{revoke_id}/revoke"
        ))
        .bearer_auth(&second_token)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);

    let old_status = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/status"))
        .bearer_auth(&first_token)
        .send()
        .await
        .unwrap();
    assert_eq!(old_status.status(), StatusCode::UNAUTHORIZED);

    let third_token = server.login().await;
    let revoke_all = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/sessions/revoke-all"
        ))
        .bearer_auth(&second_token)
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_all.status(), StatusCode::OK);

    let third_status = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/status"))
        .bearer_auth(&third_token)
        .send()
        .await
        .unwrap();
    assert_eq!(third_status.status(), StatusCode::UNAUTHORIZED);

    let current_status = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/status"))
        .bearer_auth(&second_token)
        .send()
        .await
        .unwrap();
    assert_eq!(current_status.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metrics_and_maintenance_endpoints_report_runtime_state() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("metrics-maintenance");
    std::fs::create_dir_all(&test_dir).unwrap();

    let metrics_token = "metrics-secret";
    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[
            (
                "HTTP_TUNNEL_TRUSTED_PROXY_CIDRS",
                "192.0.2.0/24".to_string(),
            ),
            (
                "HTTP_TUNNEL_METRICS_BEARER_TOKEN_HASH",
                hash_token(metrics_token),
            ),
        ],
    )
    .await;
    server.setup().await;

    let http = reqwest::Client::new();
    let ready = http
        .get(format!("http://127.0.0.1:{server_port}/api/v1/ready"))
        .send()
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);

    let metrics = http
        .get(format!("http://127.0.0.1:{server_port}/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(metrics.status(), StatusCode::UNAUTHORIZED);

    let token = server.login().await;
    let metrics = http
        .get(format!("http://127.0.0.1:{server_port}/metrics"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(metrics.status(), StatusCode::OK);
    assert!(metrics
        .text()
        .await
        .unwrap()
        .contains("http_tunnel_active_sessions"));

    let metrics = http
        .get(format!("http://127.0.0.1:{server_port}/metrics"))
        .bearer_auth(metrics_token)
        .send()
        .await
        .unwrap();
    assert_eq!(metrics.status(), StatusCode::OK);

    let maintenance = admin_get_json(&http, server_port, &token, "/api/admin/maintenance").await;
    assert!(maintenance["data"]["database_path"]
        .as_str()
        .unwrap()
        .ends_with("http-tunnel.sqlite3"));

    let checkpoint: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/maintenance/wal-checkpoint"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(checkpoint["data"]["ok"], true);

    let analyze: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/maintenance/analyze"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(analyze["data"]["operation"], "analyze");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_secret_lifecycle_is_redacted_and_audited() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("secret-lifecycle");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let token = server.login().await;
    let secret = "super-secret-turnstile-token";
    let set_turnstile: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/turnstile-secret"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"secret": secret}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(set_turnstile["data"]["configured"], true);
    let config = admin_get_json(&http, server_port, &token, "/api/admin/config").await;
    assert_eq!(config["data"]["turnstile_configured"], true);
    assert!(config["data"]["turnstile_secret"].is_null());

    let rotated_metrics: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/metrics-token/rotate"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rotated_metrics["data"]["token"].as_str().unwrap().len() > 20);
    let config = admin_get_json(&http, server_port, &token, "/api/admin/config").await;
    assert_eq!(config["data"]["metrics_bearer_token_configured"], true);
    assert!(config["data"]["metrics_bearer_token_hash"].is_null());

    let rotated_create: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnel-create-token/rotate"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(rotated_create["data"]["token"].as_str().unwrap().len() > 20);
    let clear_create = http
        .delete(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnel-create-token"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(clear_create.status(), StatusCode::OK);
    let clear_metrics = http
        .delete(format!(
            "http://127.0.0.1:{server_port}/api/admin/metrics-token"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(clear_metrics.status(), StatusCode::OK);
    let clear_turnstile = http
        .delete(format!(
            "http://127.0.0.1:{server_port}/api/admin/turnstile-secret"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(clear_turnstile.status(), StatusCode::OK);

    let audit = http
        .get(format!(
            "http://127.0.0.1:{server_port}/api/admin/audit/export?all=true"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(audit.contains("turnstile_secret_set"));
    assert!(!audit.contains(secret));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backup_api_returns_zip_and_restore_validate_checks_it() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("backup-api");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let token = server.login().await;
    let backup = http
        .post(format!("http://127.0.0.1:{server_port}/api/admin/backup"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(backup.status(), StatusCode::OK);
    assert_eq!(
        backup.headers().get("content-type").unwrap(),
        "application/zip"
    );
    let bytes = backup.bytes().await.unwrap();
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
    assert!(archive.by_name("manifest.json").is_ok());
    assert!(archive.by_name("config/server.toml").is_ok());
    assert!(archive.by_name("data/http-tunnel.sqlite3").is_ok());

    let backup_path = test_dir.join("backup.zip");
    std::fs::write(&backup_path, &bytes).unwrap();
    let validation: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/restore/validate"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"path": backup_path.display().to_string()}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(validation["data"]["validation"]["valid"], true);
    assert!(validation["data"]["restore_plan"]["config_path"]
        .as_str()
        .unwrap()
        .ends_with("server.toml"));
    assert!(validation["data"]["restore_plan"]["database_path"]
        .as_str()
        .unwrap()
        .ends_with("db.sqlite3"));
    assert!(!validation["data"]["restore_plan"]["warnings"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_lists_support_pagination_and_filters() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("admin-list-filters");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "adminlist");
    let http = reqwest::Client::new();
    let forwarded = wait_for_tunnel_get(&http, server_port, "adminlist", "/hello?filter=one").await;
    assert_eq!(forwarded.status(), StatusCode::OK);
    assert!(forwarded
        .text()
        .await
        .unwrap()
        .contains("path=/hello?filter=one"));

    let _ = create_tunnel(&http, server_port, "alpha").await;
    let _ = create_tunnel(&http, server_port, "beta").await;

    let token = server.login().await;
    let page_response = http
        .get(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels?limit=1&offset=1"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(page_response.status(), StatusCode::OK);
    assert_eq!(
        page_response.headers().get("x-http-tunnel-limit").unwrap(),
        "1"
    );
    assert_eq!(
        page_response.headers().get("x-http-tunnel-offset").unwrap(),
        "1"
    );
    assert!(
        page_response
            .headers()
            .get("x-http-tunnel-total-count")
            .unwrap()
            .to_str()
            .unwrap()
            .parse::<i64>()
            .unwrap()
            >= 3
    );
    let page: Value = page_response.json().await.unwrap();
    assert_eq!(page["data"].as_array().unwrap().len(), 1);

    let filtered = admin_get_json(
        &http,
        server_port,
        &token,
        "/api/admin/tunnels?status=reserved&q=beta",
    )
    .await;
    let tunnels = filtered["data"].as_array().unwrap();
    assert_eq!(tunnels.len(), 1);
    assert_eq!(tunnels[0]["subdomain"], "beta");

    let requests = wait_for_admin_requests(
        &http,
        server_port,
        &token,
        "/api/admin/requests?status=200&q=/hello&limit=1",
    )
    .await;
    let request = &requests["data"].as_array().unwrap()[0];
    assert_eq!(request["status"], 200);
    assert!(request["path"].as_str().unwrap().contains("/hello"));
    let request_export = http
        .get(format!(
            "http://127.0.0.1:{server_port}/api/admin/requests/export?status=200&q=/hello&limit=1"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(request_export.status(), StatusCode::OK);
    assert!(request_export
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/csv"));
    assert!(request_export
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("http-tunnel-requests.csv"));
    let request_csv = request_export.text().await.unwrap();
    assert!(request_csv.starts_with("id,type,tunnel_id"));
    assert!(request_csv.contains("/hello?filter=one"));
    let request_export_all = http
        .get(format!(
            "http://127.0.0.1:{server_port}/api/admin/requests/export?status=200&q=/hello&all=true"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(request_export_all.status(), StatusCode::OK);
    assert_eq!(
        request_export_all
            .headers()
            .get("x-http-tunnel-export-truncated")
            .unwrap(),
        "false"
    );
    assert!(
        request_export_all
            .headers()
            .get("x-http-tunnel-export-row-count")
            .unwrap()
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap()
            >= 1
    );

    let adminlist =
        admin_get_json(&http, server_port, &token, "/api/admin/tunnels?q=adminlist").await;
    let tunnel_id = adminlist["data"][0]["id"].as_str().unwrap();
    let detail = admin_get_json(
        &http,
        server_port,
        &token,
        &format!("/api/admin/tunnels/{tunnel_id}/detail"),
    )
    .await;
    assert_eq!(detail["data"]["tunnel"]["subdomain"], "adminlist");
    assert!(detail["data"]["request_count"].as_i64().unwrap() >= 1);
    assert!(detail["data"]["active_session"]["client_version"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(detail["data"]["active_session"]["client_capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "heartbeat"));
    assert!(!detail["data"]["recent_requests"]
        .as_array()
        .unwrap()
        .is_empty());

    let events = admin_get_json(
        &http,
        server_port,
        &token,
        "/api/admin/events?kind=tunnel_created&q=beta&limit=5",
    )
    .await;
    assert!(events["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["kind"] == "tunnel_created" && event["message"] == "beta" }));

    let logs = admin_get_json(
        &http,
        server_port,
        &token,
        "/api/admin/logs?q=tunnel_created&limit=2",
    )
    .await;
    let log_rows = logs["data"].as_array().unwrap();
    assert!(!log_rows.is_empty());
    assert!(log_rows.len() <= 2);
    assert!(log_rows
        .iter()
        .any(|row| row["source"] == "event" && row["kind"] == "tunnel_created"));

    let audit = admin_get_json(&http, server_port, &token, "/api/admin/audit?action=login").await;
    assert!(audit["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["action"] == "login" && row["result"] == "success"));
    let audit_export = http
        .get(format!(
            "http://127.0.0.1:{server_port}/api/admin/audit/export?action=login"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(audit_export.status(), StatusCode::OK);
    let audit_csv = audit_export.text().await.unwrap();
    assert!(audit_csv.starts_with("actor,remote_ip,action"));
    assert!(audit_csv.contains(",login,"));
    let schema = admin_get_json(&http, server_port, &token, "/api/admin/config/schema").await;
    assert!(schema["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["key"] == "domain" && row["restart_required"] == true));
    assert!(schema["data"].as_array().unwrap().iter().any(|row| {
        row["key"] == "public_scheme"
            && row["value_type"] == "enum"
            && row["allowed_values"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "https")
    }));
    assert!(schema["data"].as_array().unwrap().iter().any(|row| {
        row["key"] == "tunnel_ttl_seconds" && row["min"] == 60 && row["hot_reloadable"] == true
    }));
    let diagnostics = admin_get_json(&http, server_port, &token, "/api/admin/diagnostics").await;
    assert!(diagnostics["data"]["config"]["admin_password_hash"].is_null());
    assert!(diagnostics["data"]["config"]["admin_session_secret"].is_null());
    assert!(diagnostics["data"]["config"]["reconnect_token_secret"].is_null());
    assert!(diagnostics["data"]["config"]["tunnel_create_bearer_token_hash"].is_null());
    assert!(diagnostics["data"]["config_schema"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["env"] == "HTTP_TUNNEL_DOMAIN"));
    assert!(
        diagnostics["data"]["metrics"]["request_count"]
            .as_i64()
            .unwrap()
            >= 1
    );
    let diagnostics_export = http
        .get(format!(
            "http://127.0.0.1:{server_port}/api/admin/diagnostics/export"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(diagnostics_export.status(), StatusCode::OK);
    assert_eq!(
        diagnostics_export.headers().get("content-type").unwrap(),
        "application/json"
    );
    let diagnostics_export: Value = diagnostics_export.json().await.unwrap();
    assert!(diagnostics_export["config"]["admin_password_hash"].is_null());
    assert!(diagnostics_export["config"]["admin_session_secret"].is_null());
    assert!(diagnostics_export["config"]["reconnect_token_secret"].is_null());
    assert!(diagnostics_export["config"]["tunnel_create_bearer_token_hash"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_request_and_log_filters_can_select_errors() {
    let workspace = workspace_root();
    let server_port = free_port();
    let unused_target_port = free_port();
    let test_dir = unique_test_dir("admin-error-filters");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;
    let _client = start_client(&workspace, server_port, unused_target_port, "badtarget");

    let http = reqwest::Client::new();
    let mut last_status = None;
    for _ in 0..120 {
        let response = http
            .get(format!("http://127.0.0.1:{server_port}/fail"))
            .header(HOST, "badtarget.127.0.0.1")
            .send()
            .await
            .unwrap();
        last_status = Some(response.status());
        if response.status() == StatusCode::BAD_GATEWAY {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_eq!(last_status, Some(StatusCode::BAD_GATEWAY));

    let token = server.login().await;
    let requests = wait_for_admin_requests(
        &http,
        server_port,
        &token,
        "/api/admin/requests?error_only=true&q=/fail&limit=5",
    )
    .await;
    assert!(requests["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| { row["path"] == "/fail" && row["error"] == "local_target_failed" }));

    let logs = admin_get_json(
        &http,
        server_port,
        &token,
        "/api/admin/logs?error_only=true&q=/fail&limit=5",
    )
    .await;
    assert!(logs["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["source"] == "request" && row["detail"] == "local_target_failed"));

    let alerts = admin_get_json(&http, server_port, &token, "/api/admin/alerts").await;
    let codes = alerts["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|alert| alert["code"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"recent_proxy_errors"));
    assert!(codes.contains(&"recent_5xx"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_doctor_reports_protocol_stored_tunnel_and_websocket_target() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("doctor-enhanced");
    let client_home = unique_test_dir("doctor-enhanced-home");
    std::fs::create_dir_all(&test_dir).unwrap();
    std::fs::create_dir_all(client_home.join(".http-tunnel")).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "doctorx").await;
    std::fs::write(
        client_home.join(".http-tunnel/client.toml"),
        format!(
            "server = \"http://127.0.0.1:{server_port}\"\ntarget = \"http://127.0.0.1:{target_port}\"\nsubdomain = \"doctorx\"\ntunnel_id = \"{}\"\ntoken = \"{}\"\npersist_token = true\n",
            created["data"]["id"].as_str().unwrap(),
            created["data"]["token"].as_str().unwrap()
        ),
    )
    .unwrap();

    let output = Command::new("cargo")
        .current_dir(&workspace)
        .args([
            "run",
            "-q",
            "-p",
            "http-tunnel-client",
            "--",
            "doctor",
            "--json",
            "--websocket-path",
            "/ws",
        ])
        .env("HOME", client_home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], true);
    let checks = report["checks"].as_array().unwrap();
    for name in ["server_protocol", "stored_tunnel_state", "target_websocket"] {
        assert!(checks.iter().any(|check| check["name"] == name));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_creation_is_rate_limited() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("rate-limit");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[("HTTP_TUNNEL_RATE_LIMIT_PER_IP", "1".to_string())],
    )
    .await;
    server.setup().await;

    let http = reqwest::Client::new();
    let first = create_tunnel(&http, server_port, "one").await;
    assert!(first["data"]["id"].as_str().is_some());

    let second = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .json(&serde_json::json!({"subdomain": "two"}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turnstile_verification_endpoint_can_be_mocked() {
    let workspace = workspace_root();
    let server_port = free_port();
    let turnstile_port = free_port();
    let test_dir = unique_test_dir("turnstile-mock");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _turnstile = start_turnstile_mock(turnstile_port).await;
    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[
            ("HTTP_TUNNEL_TURNSTILE_SECRET", "secret".to_string()),
            (
                "HTTP_TUNNEL_TURNSTILE_VERIFY_URL",
                format!("http://127.0.0.1:{turnstile_port}/turnstile"),
            ),
        ],
    )
    .await;
    server.setup().await;

    let http = reqwest::Client::new();
    let missing = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .json(&serde_json::json!({"subdomain": "turnstile-missing"}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::FORBIDDEN);

    let failed = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .json(&serde_json::json!({
            "subdomain": "turnstile-failed",
            "turnstile_token": "bad"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::FORBIDDEN);

    let created = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .json(&serde_json::json!({
            "subdomain": "turnstile-ok",
            "turnstile_token": "ok"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_creation_can_require_admin_generated_bearer_token() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("create-token-required");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let admin_token = server.login().await;
    let rotated: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnel-create-token/rotate"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let create_token = rotated["data"]["token"].as_str().unwrap();

    let mut config: Value = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/config"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    config["data"]["public_tunnel_create_enabled"] = Value::Bool(false);
    let updated = http
        .put(format!("http://127.0.0.1:{server_port}/api/admin/config"))
        .bearer_auth(&admin_token)
        .json(&config["data"])
        .send()
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);

    let blocked = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .json(&serde_json::json!({"subdomain": "blocked"}))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);

    let allowed = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .bearer_auth(create_token)
        .json(&serde_json::json!({"subdomain": "allowed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_uses_create_token_when_public_creation_is_disabled() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("client-create-token");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let admin_token = server.login().await;
    let rotated: Value = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnel-create-token/rotate"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let create_token = rotated["data"]["token"].as_str().unwrap();

    let mut config: Value =
        admin_get_json(&http, server_port, &admin_token, "/api/admin/config").await;
    assert!(config["data"]["admin_password_hash"].is_null());
    assert!(config["data"]["admin_session_secret"].is_null());
    assert!(config["data"]["reconnect_token_secret"].is_null());
    assert!(config["data"]["tunnel_create_bearer_token_hash"].is_null());
    assert_eq!(
        config["data"]["tunnel_create_bearer_token_configured"],
        true
    );
    config["data"]["public_tunnel_create_enabled"] = Value::Bool(false);
    let updated = http
        .put(format!("http://127.0.0.1:{server_port}/api/admin/config"))
        .bearer_auth(&admin_token)
        .json(&config["data"])
        .send()
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);

    let _client = start_client_with_create_token(
        &workspace,
        server_port,
        target_port,
        "clienttoken",
        create_token,
    );
    let response = wait_for_tunnel_get(&http, server_port, "clienttoken", "/hello").await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_creation_enforces_active_tunnels_per_ip_limit() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("create-active-ip-limit");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let token = server.login().await;
    let mut config: Value = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/config"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    config["data"]["max_active_tunnels_per_ip"] = Value::from(1);
    let updated = http
        .put(format!("http://127.0.0.1:{server_port}/api/admin/config"))
        .bearer_auth(&token)
        .json(&config["data"])
        .send()
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);

    let first = create_tunnel(&http, server_port, "first-limit").await;
    assert!(first["data"]["id"].as_str().is_some());
    let second = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .json(&serde_json::json!({"subdomain": "second-limit"}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limit_ignores_forwarded_for_when_proxy_headers_are_untrusted() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("rate-limit-untrusted-proxy");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[
            ("HTTP_TUNNEL_RATE_LIMIT_PER_IP", "1".to_string()),
            ("HTTP_TUNNEL_TRUST_PROXY_HEADERS", "false".to_string()),
        ],
    )
    .await;
    server.setup().await;

    let http = reqwest::Client::new();
    let first = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .header("x-forwarded-for", "198.51.100.1")
        .json(&serde_json::json!({"subdomain": "xff-one"}))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .header("x-forwarded-for", "198.51.100.2")
        .json(&serde_json::json!({"subdomain": "xff-two"}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limit_uses_forwarded_for_from_trusted_proxy() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("rate-limit-trusted-proxy");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[
            ("HTTP_TUNNEL_RATE_LIMIT_PER_IP", "1".to_string()),
            ("HTTP_TUNNEL_TRUST_PROXY_HEADERS", "true".to_string()),
            (
                "HTTP_TUNNEL_TRUSTED_PROXY_CIDRS",
                "127.0.0.1/32".to_string(),
            ),
        ],
    )
    .await;
    server.setup().await;

    let http = reqwest::Client::new();
    let first = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .header("x-forwarded-for", "198.51.100.1")
        .json(&serde_json::json!({"subdomain": "xff-trusted-one"}))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .header("x-forwarded-for", "198.51.100.2")
        .json(&serde_json::json!({"subdomain": "xff-trusted-two"}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_login_is_rate_limited() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("admin-login-rate-limit");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    for _ in 0..10 {
        let response = http
            .post(format!("http://127.0.0.1:{server_port}/api/admin/login"))
            .json(&serde_json::json!({"password": "wrong-password"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let limited = http
        .post(format!("http://127.0.0.1:{server_port}/api/admin/login"))
        .json(&serde_json::json!({"password": "wrong-password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_config_update_persists_pending_restart_and_logs() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("pending-restart");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let token = server.login().await;
    let mut config: Value = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/config"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    config["data"]["addr"] = Value::String(format!("127.0.0.1:{}", free_port()));
    let updated: Value = http
        .put(format!("http://127.0.0.1:{server_port}/api/admin/config"))
        .bearer_auth(&token)
        .json(&config["data"])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["data"]["pending_restart"], true);

    let status: Value = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/status"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["data"]["pending_restart"], true);

    let logs: Value = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/logs"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(logs["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["kind"] == "admin_config_updated"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_config_update_rejects_invalid_config() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("invalid-config");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let token = server.login().await;
    let mut config: Value = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/config"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    config["data"]["public_scheme"] = Value::String("ftp".to_string());
    let response = http
        .put(format!("http://127.0.0.1:{server_port}/api/admin/config"))
        .bearer_auth(&token)
        .json(&config["data"])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let audit = admin_get_json(
        &http,
        server_port,
        &token,
        "/api/admin/audit?action=config_update&result=failure",
    )
    .await;
    assert!(audit["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["detail"] == "validation failed"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_cleanup_uses_configured_retention_and_reports_counts() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("manual-cleanup");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let http = reqwest::Client::new();
    let created = create_tunnel(&http, server_port, "cleanup").await;
    let tunnel_id = created["data"]["id"].as_str().unwrap();
    let pool = server_db(&server).await;
    sqlx::query(
        "INSERT INTO request_logs (id, tunnel_id, method, path, started_at, error) \
         VALUES ('old_req', ?1, 'GET', '/old', datetime('now', '-100 days'), 'old_error')",
    )
    .bind(tunnel_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO events (id, tunnel_id, kind, message, created_at) \
         VALUES ('old_evt', ?1, 'old_event', 'old', datetime('now', '-100 days'))",
    )
    .bind(tunnel_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO audit_logs (id, action, result, created_at) \
         VALUES ('old_audit', 'old_action', 'success', datetime('now', '-100 days'))",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sessions (id, tunnel_id, connected_at, disconnected_at, last_seen_at) \
         VALUES ('old_session', ?1, datetime('now', '-100 days'), datetime('now', '-100 days'), datetime('now', '-100 days'))",
    )
    .bind(tunnel_id)
    .execute(&pool)
    .await
    .unwrap();

    let cleanup: Value = http
        .post(format!("http://127.0.0.1:{server_port}/api/admin/cleanup"))
        .bearer_auth(server.login().await)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(cleanup["data"]["deleted_request_logs"].as_u64().unwrap() >= 1);
    assert!(cleanup["data"]["deleted_events"].as_u64().unwrap() >= 1);
    assert!(cleanup["data"]["deleted_audit_logs"].as_u64().unwrap() >= 1);
    assert!(cleanup["data"]["deleted_sessions"].as_u64().unwrap() >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_init_rejects_invalid_config() {
    let workspace = workspace_root();
    let server_port = free_port();
    let test_dir = unique_test_dir("invalid-setup");
    std::fs::create_dir_all(&test_dir).unwrap();

    let server = TestServer::start(&workspace, &test_dir, server_port).await;

    let http = reqwest::Client::new();
    let response = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/setup/init"
        ))
        .json(&serde_json::json!({
            "admin_password": "password123",
            "confirm_password": "password123",
            "domain": "https://bad.example.com/path",
            "public_scheme": "ftp",
            "addr": format!("127.0.0.1:{}", server.port),
            "database_url": "postgres://localhost/db",
            "release_repo": "bad/repo/extra"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_rejects_request_headers_over_limit() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("header-limit");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[("HTTP_TUNNEL_MAX_HEADER_BYTES", "512".to_string())],
    )
    .await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "headers");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "headers", "/hello").await;

    let response = http
        .get(format!("http://127.0.0.1:{server_port}/hello"))
        .header(HOST, "headers.127.0.0.1")
        .header("x-large-header", "x".repeat(1024))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_reconnects_after_admin_disconnect() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("reconnect");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "reconnect");
    let http = reqwest::Client::new();
    let initial = wait_for_tunnel_get(&http, server_port, "reconnect", "/hello").await;
    assert_eq!(initial.status(), StatusCode::OK);

    let tunnels: Value = http
        .get(format!("http://127.0.0.1:{server_port}/api/admin/tunnels"))
        .bearer_auth(server.login().await)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tunnel_id = tunnels["data"][0]["id"].as_str().unwrap();
    let token = server.login().await;
    let disconnected = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels/{tunnel_id}/disconnect"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(disconnected.status(), StatusCode::OK);

    let recovered = wait_for_tunnel_get(&http, server_port, "reconnect", "/hello").await;
    assert_eq!(recovered.status(), StatusCode::OK);

    let events = admin_get_json(
        &http,
        server_port,
        &server.login().await,
        "/api/admin/events?kind=client_reconnect_token_accepted&limit=5",
    )
    .await;
    assert!(!events["data"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_disconnect_cancels_pending_http_request_and_request_detail_is_available() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("disconnect-cancels-request");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start(&workspace, &test_dir, server_port).await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "cancelreq");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "cancelreq", "/hello").await;

    let token = server.login().await;
    let tunnels =
        admin_get_json(&http, server_port, &token, "/api/admin/tunnels?q=cancelreq").await;
    let tunnel_id = tunnels["data"][0]["id"].as_str().unwrap().to_string();
    let request_http = http.clone();
    let request_task = tokio::spawn(async move {
        request_http
            .get(format!("http://127.0.0.1:{server_port}/slow"))
            .header(HOST, "cancelreq.127.0.0.1")
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let disconnected = http
        .post(format!(
            "http://127.0.0.1:{server_port}/api/admin/tunnels/{tunnel_id}/disconnect"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(disconnected.status(), StatusCode::OK);
    let response = tokio::time::timeout(Duration::from_secs(2), request_task)
        .await
        .expect("request was not canceled promptly")
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

    let requests = wait_for_admin_requests(
        &http,
        server_port,
        &token,
        "/api/admin/requests?q=/slow&limit=5",
    )
    .await;
    let request_id = requests["data"][0]["id"].as_str().unwrap();
    let detail = admin_get_json(
        &http,
        server_port,
        &token,
        &format!("/api/admin/requests/{request_id}"),
    )
    .await;
    assert_eq!(detail["data"]["path"], "/slow");
    assert_eq!(detail["data"]["tunnel"]["subdomain"], "cancelreq");
    assert!(detail["data"]["session"]["id"].as_str().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn websocket_idle_timeout_closes_public_websocket() {
    let workspace = workspace_root();
    let server_port = free_port();
    let target_port = free_port();
    let test_dir = unique_test_dir("ws-idle-timeout");
    std::fs::create_dir_all(&test_dir).unwrap();

    let _target = start_target(target_port).await;
    let server = TestServer::start_with_env(
        &workspace,
        &test_dir,
        server_port,
        &[("HTTP_TUNNEL_IDLE_TIMEOUT_SECONDS", "1".to_string())],
    )
    .await;
    server.setup().await;

    let _client = start_client(&workspace, server_port, target_port, "idlews");
    let http = reqwest::Client::new();
    let _ = wait_for_tunnel_get(&http, server_port, "idlews", "/hello").await;

    let mut request = format!("ws://127.0.0.1:{server_port}/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("host", HeaderValue::from_static("idlews.127.0.0.1"));
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("idle websocket was not closed")
        .expect("websocket stream ended without close frame")
        .unwrap();
    assert!(matches!(message, TungsteniteMessage::Close(_)));
}

struct TestServer {
    port: u16,
    database: PathBuf,
    _child: ChildGuard,
}

impl TestServer {
    async fn start(workspace: &Path, test_dir: &Path, port: u16) -> Self {
        Self::start_inner(workspace, test_dir, port, &[]).await
    }

    async fn start_with_max_body(
        workspace: &Path,
        test_dir: &Path,
        port: u16,
        max_body_bytes: u64,
    ) -> Self {
        Self::start_inner(
            workspace,
            test_dir,
            port,
            &[("HTTP_TUNNEL_MAX_BODY_BYTES", max_body_bytes.to_string())],
        )
        .await
    }

    async fn start_with_env(
        workspace: &Path,
        test_dir: &Path,
        port: u16,
        envs: &[(&str, String)],
    ) -> Self {
        Self::start_inner(workspace, test_dir, port, envs).await
    }

    async fn start_inner(
        workspace: &Path,
        test_dir: &Path,
        port: u16,
        envs: &[(&str, String)],
    ) -> Self {
        let config = test_dir.join("server.toml");
        let database = test_dir.join("http-tunnel.sqlite3");
        let mut command = Command::new("cargo");
        command
            .current_dir(workspace)
            .args([
                "run",
                "-q",
                "-p",
                "http-tunnel-server",
                "--",
                "serve",
                "--config",
            ])
            .arg(&config)
            .env("HTTP_TUNNEL_ADDR", format!("127.0.0.1:{port}"))
            .env(
                "HTTP_TUNNEL_DATABASE_URL",
                format!("sqlite://{}", database.display()),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, value) in envs {
            command.env(key, value);
        }
        let child = command.spawn().unwrap();

        let server = Self {
            port,
            database,
            _child: ChildGuard { child },
        };
        server.wait_for_health().await;
        server
    }

    async fn wait_for_health(&self) {
        let http = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/api/v1/health", self.port);
        for _ in 0..120 {
            if let Ok(response) = http.get(&url).send().await {
                if response.status().is_success() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("server did not become healthy");
    }

    async fn setup(&self) {
        let http = reqwest::Client::new();
        let response = http
            .post(format!("http://127.0.0.1:{}/api/admin/setup/init", self.port))
            .json(&serde_json::json!({
                "admin_password": "password123",
                "confirm_password": "password123",
                "domain": "127.0.0.1",
                "public_scheme": "http",
                "addr": format!("127.0.0.1:{}", self.port),
                "database_url": format!("sqlite://{}", unique_test_dir("unused-db").join("db.sqlite3").display())
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    async fn login(&self) -> String {
        let http = reqwest::Client::new();
        let response: Value = http
            .post(format!("http://127.0.0.1:{}/api/admin/login", self.port))
            .json(&serde_json::json!({"password": "password123"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        response["data"]["token"].as_str().unwrap().to_string()
    }
}

async fn start_target(port: u16) -> TargetGuard {
    let app = Router::new()
        .route("/ws", get(target_ws_handler))
        .fallback(any(target_handler));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });
    TargetGuard {
        shutdown: Some(shutdown_tx),
        task,
    }
}

async fn start_turnstile_mock(port: u16) -> TargetGuard {
    let app = Router::new().route("/turnstile", any(turnstile_mock_handler));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await;
    });
    TargetGuard {
        shutdown: Some(shutdown_tx),
        task,
    }
}

async fn turnstile_mock_handler(request: Request<Body>) -> Response {
    let body = to_bytes(request.into_body(), 4096).await.unwrap();
    let body = String::from_utf8_lossy(&body);
    let success = body.split('&').any(|part| part == "response=ok");
    axum::Json(serde_json::json!({"success": success})).into_response()
}

async fn target_ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(target_ws_echo).into_response()
}

async fn target_ws_echo(mut ws: WebSocket) {
    while let Some(message) = ws.recv().await {
        let Ok(message) = message else {
            break;
        };
        match message {
            AxumWsMessage::Text(text) => {
                if ws.send(AxumWsMessage::Text(text)).await.is_err() {
                    break;
                }
            }
            AxumWsMessage::Binary(bytes) => {
                if ws.send(AxumWsMessage::Binary(bytes)).await.is_err() {
                    break;
                }
            }
            AxumWsMessage::Close(close) => {
                let _ = ws.send(AxumWsMessage::Close(close)).await;
                break;
            }
            AxumWsMessage::Ping(bytes) => {
                let _ = ws.send(AxumWsMessage::Pong(bytes)).await;
            }
            AxumWsMessage::Pong(_) => {}
        }
    }
}

async fn target_handler(headers: HeaderMap, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    if path == "/sse" {
        return sse_response();
    }
    if path == "/slow" {
        tokio::time::sleep(Duration::from_secs(5)).await;
        return (
            StatusCode::OK,
            [("x-target", "ok"), ("content-type", "text/plain")],
            "slow done\n",
        )
            .into_response();
    }
    let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
    if path == "/len" {
        return (
            StatusCode::CREATED,
            [("x-target", "ok"), ("content-type", "text/plain")],
            format!("len={}\n", body.len()),
        )
            .into_response();
    }
    let body = String::from_utf8_lossy(&body);
    let subdomain = headers
        .get("x-http-tunnel-subdomain")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let forwarded_for = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let forwarded_host = headers
        .get("x-forwarded-host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let forwarded_proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let dynamic_hop = headers
        .get("x-dynamic-hop")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let response_body = format!(
        "method={method}\npath={path}\nbody={body}\nsubdomain={subdomain}\nforwarded_for={forwarded_for}\nforwarded_host={forwarded_host}\nforwarded_proto={forwarded_proto}\ndynamic_hop={dynamic_hop}\n"
    );
    let status = if method == "POST" {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (
        status,
        [("x-target", "ok"), ("content-type", "text/plain")],
        response_body,
    )
        .into_response()
}

fn streaming_body(chunks: impl IntoIterator<Item = &'static str>) -> reqwest::Body {
    let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(8);
    let chunks = chunks.into_iter().collect::<Vec<_>>();
    tokio::spawn(async move {
        for chunk in chunks {
            if tx
                .send(Ok(Bytes::from_static(chunk.as_bytes())))
                .await
                .is_err()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
    reqwest::Body::wrap_stream(ReceiverStream::new(rx))
}

fn sse_response() -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, Infallible>>(8);
    tokio::spawn(async move {
        for event in ["one", "two", "three"] {
            let chunk = Bytes::from(format!("data: {event}\n\n"));
            if tx.send(Ok(chunk)).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
    });

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-target", "ok")
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap()
}

fn start_client(
    workspace: &Path,
    server_port: u16,
    target_port: u16,
    subdomain: &str,
) -> ChildGuard {
    let client_home = unique_test_dir(&format!("client-home-{subdomain}"));
    let child = Command::new("cargo")
        .current_dir(workspace)
        .args([
            "run",
            "-q",
            "-p",
            "http-tunnel-client",
            "--",
            "connect",
            "--server",
            &format!("http://127.0.0.1:{server_port}"),
            "--subdomain",
            subdomain,
            "--target",
            &format!("http://127.0.0.1:{target_port}"),
        ])
        .env("HOME", client_home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    ChildGuard { child }
}

fn start_client_with_create_token(
    workspace: &Path,
    server_port: u16,
    target_port: u16,
    subdomain: &str,
    create_token: &str,
) -> ChildGuard {
    let client_home = unique_test_dir(&format!("client-home-create-token-{subdomain}"));
    let child = Command::new("cargo")
        .current_dir(workspace)
        .args([
            "run",
            "-q",
            "-p",
            "http-tunnel-client",
            "--",
            "connect",
            "--server",
            &format!("http://127.0.0.1:{server_port}"),
            "--subdomain",
            subdomain,
            "--target",
            &format!("http://127.0.0.1:{target_port}"),
            "--create-token",
            create_token,
        ])
        .env("HOME", client_home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    ChildGuard { child }
}

type TestTunnelWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_tunnel_ws(server_port: u16, tunnel_id: &str, tunnel_token: &str) -> TestTunnelWs {
    tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{server_port}/api/v1/tunnels/{tunnel_id}/connect?token={tunnel_token}"
    ))
    .await
    .unwrap()
    .0
}

async fn send_tunnel_hello(ws: &mut TestTunnelWs, reconnect_token: Option<String>) -> HelloAck {
    let payload = encode_payload(&Hello {
        target: "http://127.0.0.1:1".to_string(),
        client_version: Some("e2e".to_string()),
        protocol_version: Some(PROTOCOL_VERSION),
        capabilities: vec!["http".to_string(), "websocket".to_string()],
        reconnect_token,
        client_source: None,
    })
    .unwrap();
    ws.send(TungsteniteMessage::Binary(
        encode_frame(&Frame::new(FrameType::Hello, 0, payload)).unwrap(),
    ))
    .await
    .unwrap();
    loop {
        let frame = next_tunnel_frame(ws).await;
        if frame.frame_type == FrameType::HelloAck {
            return decode_payload::<HelloAck>(&frame.payload).unwrap();
        }
    }
}

async fn next_tunnel_frame(ws: &mut TestTunnelWs) -> Frame {
    loop {
        let message = ws.next().await.unwrap().unwrap();
        if let TungsteniteMessage::Binary(bytes) = message {
            return decode_frame(&bytes).unwrap();
        }
    }
}

async fn next_tunnel_frame_of_type(ws: &mut TestTunnelWs, frame_type: FrameType) -> Frame {
    loop {
        let frame = next_tunnel_frame(ws).await;
        if frame.frame_type == frame_type {
            return frame;
        }
    }
}

async fn send_tunnel_http_response(ws: &mut TestTunnelWs, stream_id: u64, body: &str) {
    let start = encode_payload(&ResponseStart {
        status: 200,
        headers: vec![("content-type".to_string(), "text/plain".to_string())],
    })
    .unwrap();
    ws.send(TungsteniteMessage::Binary(
        encode_frame(&Frame::new(FrameType::ResponseStart, stream_id, start)).unwrap(),
    ))
    .await
    .unwrap();
    ws.send(TungsteniteMessage::Binary(
        encode_frame(&Frame::new(
            FrameType::ResponseBody,
            stream_id,
            body.as_bytes().to_vec(),
        ))
        .unwrap(),
    ))
    .await
    .unwrap();
    ws.send(TungsteniteMessage::Binary(
        encode_frame(&Frame::new(FrameType::ResponseEnd, stream_id, Vec::new())).unwrap(),
    ))
    .await
    .unwrap();
}

async fn wait_for_tunnel_get(
    http: &reqwest::Client,
    server_port: u16,
    subdomain: &str,
    path: &str,
) -> reqwest::Response {
    let url = format!("http://127.0.0.1:{server_port}{path}");
    let host = format!("{subdomain}.127.0.0.1");
    let mut last_status = None;
    for _ in 0..120 {
        match http.get(&url).header(HOST, &host).send().await {
            Ok(response) if response.status().is_success() => return response,
            Ok(response) => last_status = Some(response.status()),
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("tunnel did not become available, last status: {last_status:?}");
}

async fn create_tunnel(http: &reqwest::Client, server_port: u16, subdomain: &str) -> Value {
    let response = http
        .post(format!("http://127.0.0.1:{server_port}/api/v1/tunnels"))
        .json(&serde_json::json!({"subdomain": subdomain, "ttl_seconds": 3600}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn admin_get_json(
    http: &reqwest::Client,
    server_port: u16,
    token: &str,
    path: &str,
) -> Value {
    let response = http
        .get(format!("http://127.0.0.1:{server_port}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn wait_for_admin_requests(
    http: &reqwest::Client,
    server_port: u16,
    token: &str,
    path: &str,
) -> Value {
    let mut last = None;
    for _ in 0..80 {
        let value = admin_get_json(http, server_port, token, path).await;
        if !value["data"].as_array().unwrap().is_empty() {
            return value;
        }
        last = Some(value);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("admin request rows did not appear: {last:?}");
}

async fn wait_for_active_session_count(
    http: &reqwest::Client,
    server_port: u16,
    token: &str,
    tunnel_id: &str,
    expected: usize,
) {
    let mut last = None;
    for _ in 0..80 {
        let value = admin_get_json(
            http,
            server_port,
            token,
            &format!("/api/admin/tunnels/{tunnel_id}/detail"),
        )
        .await;
        let count = value["data"]["active_sessions"].as_array().unwrap().len();
        if count == expected {
            return;
        }
        last = Some(count);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("active session count did not become {expected}, last: {last:?}");
}

async fn mark_tunnel_expired(server: &TestServer, tunnel_id: &str) {
    let pool = server_db(server).await;
    sqlx::query(
        "UPDATE tunnels SET status = 'expired', expires_at = datetime('now', '-1 second') WHERE id = ?1",
    )
    .bind(tunnel_id)
    .execute(&pool)
    .await
    .unwrap();
}

async fn server_db(server: &TestServer) -> sqlx::SqlitePool {
    sqlx::SqlitePool::connect(&format!("sqlite://{}", server.database.display()))
        .await
        .unwrap()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn unique_test_dir(name: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    workspace_root()
        .join("target")
        .join("e2e-tests")
        .join(format!("{name}-{now}"))
}

fn free_port() -> u16 {
    static USED_PORTS: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();
    let used_ports = USED_PORTS.get_or_init(|| Mutex::new(HashSet::new()));
    loop {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let port = addr.port();
        if used_ports.lock().unwrap().insert(port) {
            return port;
        }
    }
}
