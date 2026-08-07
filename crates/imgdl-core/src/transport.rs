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
