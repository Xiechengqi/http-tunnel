use super::{plain_response, redirect_response, rewrite};
use crate::state::GithubProxyActivityGuard;
use axum::{
    body::Body,
    http::{header, HeaderMap, Request, StatusCode},
    response::Response,
};
use futures_util::StreamExt;
use http_tunnel_common::headers::filtered_headers;
use reqwest::Url;
use std::time::Duration;

pub(crate) async fn proxy_request(
    client: &reqwest::Client,
    req: Request<Body>,
    upstream_url: Url,
    route_prefix: &str,
    size_limit_bytes: u64,
    timeout: Duration,
    activity_guard: Option<GithubProxyActivityGuard>,
) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    let mut builder = client
        .request(method.clone(), upstream_url.clone())
        .timeout(timeout);

    let request_headers = filtered_headers(&parts.headers);
    for (name, value) in request_headers.iter() {
        if name == header::HOST {
            continue;
        }
        builder = builder.header(name.clone(), value.clone());
    }

    if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
        builder = builder.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }

    let upstream = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, %upstream_url, "github proxy upstream request failed");
            let status = if error.is_timeout() {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            return plain_response(status, "github proxy upstream request failed");
        }
    };

    let status = upstream.status();
    let headers = upstream.headers().clone();
    if content_length(&headers).is_some_and(|length| length > size_limit_bytes) {
        return redirect_response(StatusCode::FOUND, upstream_url.as_str());
    }

    let mut response_headers = filtered_headers(&headers);
    rewrite_location(&mut response_headers, &upstream_url, route_prefix);
    let stream = upstream.bytes_stream().map(move |chunk| {
        let _keep_alive = activity_guard.as_ref();
        chunk
    });

    let mut builder = Response::builder().status(status);
    for (name, value) in response_headers.iter() {
        builder = builder.header(name.clone(), value.clone());
    }
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to build github proxy response");
            plain_response(StatusCode::BAD_GATEWAY, "github proxy response failed")
        })
}

fn rewrite_location(headers: &mut HeaderMap, upstream_url: &Url, route_prefix: &str) {
    let Some(location) = headers
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    let Some(rewritten) = rewrite::proxied_location(location, upstream_url, route_prefix) else {
        return;
    };
    if let Ok(value) = rewritten.parse() {
        headers.insert(header::LOCATION, value);
    }
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{HeaderValue, Method, Request, StatusCode},
        response::{IntoResponse, Response},
        routing::any,
        Router,
    };
    use bytes::Bytes;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn proxies_get_and_filters_headers() {
        let (addr, _task) = start_mock_upstream().await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("github.com", addr)
            .build()
            .unwrap();
        let upstream_url = Url::parse(&format!(
            "http://github.com:{}/owner/repo/archive/main.zip",
            addr.port()
        ))
        .unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/gh/https://github.com/owner/repo/archive/main.zip")
            .header("connection", "x-hop")
            .header("x-hop", "remove")
            .body(Body::empty())
            .unwrap();

        let response = proxy_request(
            &client,
            request,
            upstream_url,
            "/gh",
            1024,
            Duration::from_secs(5),
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("x-upstream").unwrap(), "ok");
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body, Bytes::from_static(b"method=GET\nx-hop=\n"));
    }

    #[tokio::test]
    async fn redirects_when_content_length_exceeds_limit() {
        let (addr, _task) = start_mock_upstream().await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("github.com", addr)
            .build()
            .unwrap();
        let upstream_url = Url::parse(&format!(
            "http://github.com:{}/owner/repo/releases/download/v1/large",
            addr.port()
        ))
        .unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/gh/https://github.com/owner/repo/releases/download/v1/large")
            .body(Body::empty())
            .unwrap();

        let response = proxy_request(
            &client,
            request,
            upstream_url,
            "/gh",
            4,
            Duration::from_secs(5),
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::FOUND);
        assert!(response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("http://github.com:"));
    }

    #[tokio::test]
    async fn rewrites_allowed_location() {
        let (addr, _task) = start_mock_upstream().await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("github.com", addr)
            .build()
            .unwrap();
        let upstream_url = Url::parse(&format!(
            "http://github.com:{}/owner/repo/archive/redirect",
            addr.port()
        ))
        .unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri("/gh/https://github.com/owner/repo/archive/redirect")
            .body(Body::empty())
            .unwrap();

        let response = proxy_request(
            &client,
            request,
            upstream_url,
            "/gh",
            1024,
            Duration::from_secs(5),
            None,
        )
        .await;

        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(location.starts_with("/gh/http://github.com:"));
        assert!(location.ends_with("/owner/repo/archive/main.zip"));
    }

    async fn start_mock_upstream() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let app = Router::new().fallback(any(mock_handler));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (addr, task)
    }

    async fn mock_handler(req: Request<Body>) -> Response {
        let path = req.uri().path().to_string();
        if path.ends_with("/large") {
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_LENGTH, "16")
                .body(Body::from("0123456789abcdef"))
                .unwrap();
        }
        if path.ends_with("/redirect") {
            return Response::builder()
                .status(StatusCode::FOUND)
                .header(header::LOCATION, "/owner/repo/archive/main.zip")
                .body(Body::empty())
                .unwrap();
        }
        let x_hop = req
            .headers()
            .get("x-hop")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        (
            StatusCode::OK,
            [("x-upstream", HeaderValue::from_static("ok"))],
            format!("method={}\nx-hop={x_hop}\n", req.method()),
        )
            .into_response()
    }
}
