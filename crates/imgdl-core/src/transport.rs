use std::time::{Duration, Instant};

use bytes::Bytes;
use http::uri::{Parts, PathAndQuery, Uri};
use http::{HeaderMap, StatusCode};
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::{http1, http2};
use tracing::Instrument;

use crate::pool::ReqBody;
use crate::types::DownloadError;

/// Data returned from a successful HTTP request execution.
/// This is an internal type -- the orchestrator converts it to `DownloadOutcome`.
#[derive(Debug)]
pub(crate) struct ResponseData {
    pub bytes: Bytes,
    pub headers: HeaderMap,
    pub elapsed: Duration,
}

/// Subset of Config relevant to a single request execution.
#[derive(Debug, Clone)]
pub(crate) struct RequestConfig {
    pub user_agent: String,
    pub request_timeout: Duration,
    pub max_redirects: u8,
}

/// Result of `execute_request` when a cross-host redirect is encountered.
/// The orchestrator must re-acquire a connection for the new host.
#[derive(Debug)]
pub(crate) struct CrossHostRedirect {
    pub new_uri: Uri,
    pub redirects_remaining: u8,
}

/// Protocol-specific sender for execute_request.
pub(crate) enum Sender {
    H2(http2::SendRequest<ReqBody>),
    H1(http1::SendRequest<ReqBody>),
}

impl From<crate::pool::PoolHandleSender> for Sender {
    fn from(phs: crate::pool::PoolHandleSender) -> Self {
        match phs {
            crate::pool::PoolHandleSender::H2(s) => Sender::H2(s),
            crate::pool::PoolHandleSender::H1(s) => Sender::H1(s),
        }
    }
}

/// Successful transport result, or a cross-host redirect signal.
#[derive(Debug)]
pub(crate) enum TransportResult {
    Response(ResponseData),
    Redirect(CrossHostRedirect),
}

/// Execute a single HTTP GET request on the provided sender handle.
///
/// Handles:
/// - Building the request with correct headers (Host, User-Agent, Accept)
/// - Wrapping in a request timeout
/// - Following same-host redirects (up to max_redirects hops)
/// - Collecting the response body
/// - Returning structured ResponseData or DownloadError
///
/// For cross-host redirects, returns `TransportResult::Redirect` so the
/// orchestrator can re-acquire a connection for the new host.
pub(crate) async fn execute_request(
    sender: &mut Sender,
    url: &Uri,
    host: &str,
    config: &RequestConfig,
) -> Result<TransportResult, DownloadError> {
    let span = tracing::debug_span!("http_request", method = "GET", url = %url);
    execute_request_inner(sender, url, host, config)
        .instrument(span)
        .await
}

async fn execute_request_inner(
    sender: &mut Sender,
    url: &Uri,
    host: &str,
    config: &RequestConfig,
) -> Result<TransportResult, DownloadError> {
    let start = Instant::now();

    let result = tokio::time::timeout(
        config.request_timeout,
        execute_with_redirects(sender, url, host, config),
    )
    .await;

    match result {
        Ok(Ok(TransportResult::Response(mut data))) => {
            data.elapsed = start.elapsed();
            Ok(TransportResult::Response(data))
        }
        Ok(Ok(redirect @ TransportResult::Redirect(_))) => Ok(redirect),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(DownloadError::Timeout),
    }
}

async fn execute_with_redirects(
    sender: &mut Sender,
    url: &Uri,
    host: &str,
    config: &RequestConfig,
) -> Result<TransportResult, DownloadError> {
    let mut current_url = url.clone();
    let mut redirects = 0u8;

    loop {
        let path = current_url
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");

        let request = http::Request::builder()
            .method(http::Method::GET)
            .uri(path)
            .header("Host", host)
            .header("User-Agent", &config.user_agent)
            .header("Accept", "image/*")
            .body(Empty::<Bytes>::new())
            .map_err(|e| {
                DownloadError::ConnectionFailed(format!("failed to build request: {e}"))
            })?;

        let response = send_request(sender, request).await?;

        let status = response.status();

        if matches!(
            status,
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        ) {
            redirects += 1;
            if redirects > config.max_redirects {
                return Err(DownloadError::TooManyRedirects(format!(
                    "exceeded {} redirects",
                    config.max_redirects
                )));
            }

            let location = response
                .headers()
                .get("location")
                .ok_or_else(|| {
                    DownloadError::ConnectionFailed(
                        "redirect response missing Location header".into(),
                    )
                })?
                .to_str()
                .map_err(|e| {
                    DownloadError::ConnectionFailed(format!("invalid Location header: {e}"))
                })?
                .to_owned();

            // Drain the redirect response body to keep the HTTP/1.1 connection usable.
            let _ = response.into_body().collect().await;

            let resolved = resolve_redirect(&current_url, &location)?;

            // Check for cross-host redirect
            let new_host = resolved.host().unwrap_or("");
            if !new_host.eq_ignore_ascii_case(host) {
                tracing::debug!("cross-host redirect from {} to {}", host, new_host);
                return Ok(TransportResult::Redirect(CrossHostRedirect {
                    new_uri: resolved,
                    redirects_remaining: config.max_redirects - redirects,
                }));
            }

            tracing::debug!("following redirect {} -> {}", current_url, resolved);
            current_url = resolved;
            continue;
        }

        // Non-redirect response: collect body
        if !status.is_success() {
            // Collect body for error message but also check for Retry-After
            let headers = response.headers().clone();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|e| {
                    DownloadError::ConnectionFailed(format!("failed to read error body: {e}"))
                })?
                .to_bytes();

            let retry_after = parse_retry_after(&headers);
            let message = String::from_utf8_lossy(&body).chars().take(200).collect();

            if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE
            {
                tracing::warn!(status = %status.as_u16(), "transient HTTP status");
            }

            return Err(DownloadError::HttpStatus {
                code: status.as_u16(),
                message,
                retry_after,
            });
        }

        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|e| {
                DownloadError::ConnectionFailed(format!("failed to read response body: {e}"))
            })?
            .to_bytes();

        return Ok(TransportResult::Response(ResponseData {
            bytes: body,
            headers,
            elapsed: Duration::ZERO, // overwritten by caller
        }));
    }
}

async fn send_request(
    sender: &mut Sender,
    request: http::Request<Empty<Bytes>>,
) -> Result<http::Response<hyper::body::Incoming>, DownloadError> {
    match sender {
        Sender::H2(s) => {
            s.ready()
                .await
                .map_err(|e| DownloadError::ConnectionFailed(format!("sender not ready: {e}")))?;
            s.send_request(request)
                .await
                .map_err(|e| DownloadError::ConnectionFailed(format!("send failed: {e}")))
        }
        Sender::H1(s) => {
            s.ready()
                .await
                .map_err(|e| DownloadError::ConnectionFailed(format!("sender not ready: {e}")))?;
            s.send_request(request)
                .await
                .map_err(|e| DownloadError::ConnectionFailed(format!("send failed: {e}")))
        }
    }
}

/// Resolve a Location header value against the current URL.
fn resolve_redirect(current: &Uri, location: &str) -> Result<Uri, DownloadError> {
    // Try parsing as absolute URI first
    if let Ok(uri) = location.parse::<Uri>() {
        if uri.scheme().is_some() {
            return Ok(uri);
        }
    }

    // Relative URI: resolve against current URL
    let mut parts = Parts::from(current.clone());
    let new_pq: PathAndQuery = location.parse().map_err(|e| {
        DownloadError::ConnectionFailed(format!("invalid redirect location '{location}': {e}"))
    })?;
    parts.path_and_query = Some(new_pq);
    Uri::from_parts(parts).map_err(|e| {
        DownloadError::ConnectionFailed(format!("failed to construct redirect URI: {e}"))
    })
}

/// Parse Retry-After header value (integer seconds only).
fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Create an HTTP/1.1 sender connected to a wiremock server.
    async fn make_h1_sender(server: &MockServer) -> http1::SendRequest<ReqBody> {
        let addr = server.address();
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let io = hyper_util::rt::TokioIo::new(stream);
        let (sender, conn) = http1::Builder::new().handshake(io).await.unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });
        sender
    }

    fn test_config() -> RequestConfig {
        RequestConfig {
            user_agent: "imgdl/0.1".to_string(),
            request_timeout: Duration::from_secs(5),
            max_redirects: 5,
        }
    }

    fn server_uri(server: &MockServer, p: &str) -> Uri {
        format!("http://127.0.0.1:{}{}", server.address().port(), p)
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn sends_correct_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("Host", "127.0.0.1"))
            .and(header("User-Agent", "imgdl/0.1"))
            .and(header("Accept", "image/*"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok"))
            .expect(1)
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/test.jpg");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn no_accept_encoding_header() {
        let server = MockServer::start().await;
        // wiremock doesn't have a negative header matcher, so we'll verify
        // by checking the received request doesn't contain Accept-Encoding.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok"))
            .expect(1)
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/test.jpg");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config).await;
        assert!(result.is_ok());

        // Verify mock was matched (if Accept-Encoding was sent, mock still matches
        // but we've verified our builder doesn't add it by code inspection)
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        assert!(
            !received[0].headers.contains_key("accept-encoding"),
            "should NOT send Accept-Encoding header"
        );
    }

    #[tokio::test]
    async fn returns_response_data_correctly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"image-data-here")
                    .insert_header("Content-Type", "image/jpeg"),
            )
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/img.jpg");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config)
            .await
            .unwrap();

        match result {
            TransportResult::Response(data) => {
                assert_eq!(data.bytes.as_ref(), b"image-data-here");
                assert_eq!(data.headers.get("content-type").unwrap(), "image/jpeg");
                assert!(data.elapsed.as_nanos() > 0);
            }
            TransportResult::Redirect(_) => panic!("expected response, got redirect"),
        }
    }

    #[tokio::test]
    async fn returns_timeout_on_slow_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"slow")
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/slow.jpg");
        let config = RequestConfig {
            user_agent: "imgdl/0.1".to_string(),
            request_timeout: Duration::from_millis(100),
            max_redirects: 5,
        };
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config).await;
        assert!(matches!(result, Err(DownloadError::Timeout)));
    }

    #[tokio::test]
    async fn returns_http_status_for_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_bytes(b"Not Found"))
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/missing.jpg");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config).await;
        match result {
            Err(DownloadError::HttpStatus {
                code: 404, message, ..
            }) => {
                assert!(message.contains("Not Found"));
            }
            other => panic!("expected HttpStatus 404, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn follows_301_redirect() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/old"))
            .respond_with(ResponseTemplate::new(301).insert_header("Location", "/new"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/new"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"redirected"))
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/old");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config)
            .await
            .unwrap();
        match result {
            TransportResult::Response(data) => {
                assert_eq!(data.bytes.as_ref(), b"redirected");
            }
            TransportResult::Redirect(_) => panic!("expected response after same-host redirect"),
        }
    }

    #[tokio::test]
    async fn follows_302_redirect() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/temp-old"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/temp-new"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/temp-new"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"found"))
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/temp-old");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config)
            .await
            .unwrap();
        match result {
            TransportResult::Response(data) => {
                assert_eq!(data.bytes.as_ref(), b"found");
            }
            _ => panic!("expected response"),
        }
    }

    #[tokio::test]
    async fn follows_307_redirect() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(307).insert_header("Location", "/b"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"307ok"))
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/a");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config)
            .await
            .unwrap();
        match result {
            TransportResult::Response(data) => {
                assert_eq!(data.bytes.as_ref(), b"307ok");
            }
            _ => panic!("expected response"),
        }
    }

    #[tokio::test]
    async fn follows_308_redirect() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/c"))
            .respond_with(ResponseTemplate::new(308).insert_header("Location", "/d"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/d"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"308ok"))
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/c");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config)
            .await
            .unwrap();
        match result {
            TransportResult::Response(data) => {
                assert_eq!(data.bytes.as_ref(), b"308ok");
            }
            _ => panic!("expected response"),
        }
    }

    #[tokio::test]
    async fn returns_too_many_redirects() {
        let server = MockServer::start().await;

        // Redirect loop: /loop -> /loop
        Mock::given(method("GET"))
            .and(path("/loop"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/loop"))
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/loop");
        let config = RequestConfig {
            user_agent: "imgdl/0.1".to_string(),
            request_timeout: Duration::from_secs(5),
            max_redirects: 3,
        };
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config).await;
        assert!(
            matches!(result, Err(DownloadError::TooManyRedirects(_))),
            "expected TooManyRedirects, got {result:?}"
        );
    }

    #[tokio::test]
    async fn returns_connection_failed_for_broken_connection() {
        // Create a TCP listener, accept one connection, then drop it immediately
        // to simulate a broken pipe / connection reset.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn a task that accepts and immediately drops the connection
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(stream); // immediately close
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let io = hyper_util::rt::TokioIo::new(stream);
        let (h1, conn) = http1::Builder::new().handshake(io).await.unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });

        // Give time for the server to close the connection
        tokio::time::sleep(Duration::from_millis(50)).await;

        let uri: Uri = format!("http://127.0.0.1:{}/test.jpg", addr.port())
            .parse()
            .unwrap();
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config).await;
        assert!(
            matches!(result, Err(DownloadError::ConnectionFailed(_))),
            "expected ConnectionFailed, got {result:?}"
        );
    }

    #[tokio::test]
    async fn cross_host_redirect_returns_redirect_signal() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/cross"))
            .respond_with(
                ResponseTemplate::new(301)
                    .insert_header("Location", "https://other-host.example.com/image.jpg"),
            )
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/cross");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config)
            .await
            .unwrap();
        match result {
            TransportResult::Redirect(redir) => {
                assert_eq!(redir.new_uri.host().unwrap(), "other-host.example.com");
                assert_eq!(redir.redirects_remaining, 4); // 5 - 1
            }
            _ => panic!("expected cross-host redirect signal"),
        }
    }

    #[tokio::test]
    async fn returns_429_with_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_bytes(b"rate limited")
                    .insert_header("Retry-After", "2"),
            )
            .mount(&server)
            .await;

        let h1 = make_h1_sender(&server).await;
        let uri = server_uri(&server, "/limited");
        let config = test_config();
        let mut sender = Sender::H1(h1);

        let result = execute_request(&mut sender, &uri, "127.0.0.1", &config).await;
        match result {
            Err(DownloadError::HttpStatus {
                code: 429,
                retry_after,
                ..
            }) => {
                assert_eq!(retry_after, Some(Duration::from_secs(2)));
            }
            other => panic!("expected HttpStatus 429, got {other:?}"),
        }
    }
}
