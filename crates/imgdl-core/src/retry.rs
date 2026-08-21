use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::types::DownloadError;

/// Determine whether a DownloadError should trigger a retry.
pub(crate) fn is_retryable(error: &DownloadError) -> bool {
    matches!(
        error,
        DownloadError::HttpStatus {
            code: 429 | 503,
            ..
        } | DownloadError::Timeout
            | DownloadError::ConnectionFailed(_)
    )
}

/// Compute backoff delay with jitter.
///
/// delay = base_delay * 2^attempt + random_jitter (0..50% of delay)
fn compute_backoff(attempt: u32, base_delay: Duration, retry_after: Option<Duration>) -> Duration {
    if let Some(ra) = retry_after {
        return ra;
    }

    let multiplier = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    let base_ms = (base_delay.as_millis() as u64).saturating_mul(multiplier);

    // Simple jitter using system time nanoseconds to avoid rand dependency
    let jitter_range = base_ms / 2; // 0..50% of delay
    let jitter = if jitter_range > 0 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64;
        // Mix in attempt number for better distribution across attempts
        let seed = nanos
            .wrapping_mul(6364136223846793005)
            .wrapping_add(attempt as u64);
        seed % jitter_range
    } else {
        0
    };

    tracing::debug!(attempt, base_ms, jitter, "computed backoff delay");

    Duration::from_millis(base_ms.saturating_add(jitter))
}

/// Execute an async operation with retry logic for transient errors.
///
/// The operation closure is called on each attempt. It must re-acquire any
/// connection handles internally -- do not capture a sender outside the closure,
/// because a ConnectionFailed error means the captured sender is dead.
///
/// Returns `(Result, retries_attempted)` so the orchestrator can populate
/// `retries_attempted` in `DownloadOutcome::Failure`.
pub(crate) async fn with_retry<T, F, Fut>(
    max_retries: u32,
    base_delay: Duration,
    operation: F,
) -> (Result<T, DownloadError>, u32)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, DownloadError>>,
{
    let mut attempt = 0u32;

    loop {
        let result = operation().await;

        match result {
            Ok(data) => return (Ok(data), attempt),
            Err(ref e) if !is_retryable(e) => return (result, attempt),
            Err(ref e) if attempt >= max_retries => return (result, attempt),
            Err(ref e) => {
                let retry_after = match e {
                    DownloadError::HttpStatus { retry_after, .. } => *retry_after,
                    _ => None,
                };
                let delay = compute_backoff(attempt, base_delay, retry_after);
                tracing::warn!(
                    error = %e,
                    attempt = attempt + 1,
                    max_retries = max_retries,
                    delay_ms = delay.as_millis() as u64,
                    "retrying after transient error"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::HeaderMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    use crate::transport::ResponseData;

    fn ok_response() -> ResponseData {
        ResponseData {
            bytes: Bytes::from_static(b"ok"),
            headers: HeaderMap::new(),
            elapsed: Duration::from_millis(10),
        }
    }

    #[tokio::test]
    async fn returns_immediately_on_success() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) = with_retry(3, Duration::from_millis(10), || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(ok_response())
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(retries, 0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_on_429() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) = with_retry(3, Duration::from_millis(10), || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(DownloadError::HttpStatus {
                        code: 429,
                        message: "Too Many Requests".into(),
                        retry_after: None,
                    })
                } else {
                    Ok(ok_response())
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(retries, 2);
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retries_on_503() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) = with_retry(3, Duration::from_millis(10), || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    Err(DownloadError::HttpStatus {
                        code: 503,
                        message: "Service Unavailable".into(),
                        retry_after: None,
                    })
                } else {
                    Ok(ok_response())
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(retries, 1);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retries_on_timeout() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) = with_retry(3, Duration::from_millis(10), || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    Err(DownloadError::Timeout)
                } else {
                    Ok(ok_response())
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(retries, 1);
    }

    #[tokio::test]
    async fn retries_on_connection_failed() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) = with_retry(3, Duration::from_millis(10), || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    Err(DownloadError::ConnectionFailed("reset".into()))
                } else {
                    Ok(ok_response())
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(retries, 1);
    }

    #[tokio::test]
    async fn does_not_retry_on_404() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) =
            with_retry::<ResponseData, _, _>(3, Duration::from_millis(10), || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err(DownloadError::HttpStatus {
                        code: 404,
                        message: "Not Found".into(),
                        retry_after: None,
                    })
                }
            })
            .await;

        assert!(result.is_err());
        assert_eq!(retries, 0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn does_not_retry_on_tls_error() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) =
            with_retry::<ResponseData, _, _>(3, Duration::from_millis(10), || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err(DownloadError::TlsError("bad cert".into()))
                }
            })
            .await;

        assert!(result.is_err());
        assert_eq!(retries, 0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn does_not_retry_on_dns_failed() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) =
            with_retry::<ResponseData, _, _>(3, Duration::from_millis(10), || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err(DownloadError::DnsResolutionFailed("NXDOMAIN".into()))
                }
            })
            .await;

        assert!(result.is_err());
        assert_eq!(retries, 0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn respects_retry_after_header() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();
        let start = Instant::now();

        let (result, _) = with_retry(3, Duration::from_millis(10), || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    Err(DownloadError::HttpStatus {
                        code: 429,
                        message: "rate limited".into(),
                        retry_after: Some(Duration::from_millis(50)),
                    })
                } else {
                    Ok(ok_response())
                }
            }
        })
        .await;

        assert!(result.is_ok());
        // Should have waited at least 50ms for Retry-After
        assert!(start.elapsed() >= Duration::from_millis(40)); // small tolerance
    }

    #[tokio::test]
    async fn backoff_increases_exponentially() {
        let timestamps = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let ts = timestamps.clone();

        let (_, _) = with_retry::<ResponseData, _, _>(3, Duration::from_millis(20), || {
            let ts = ts.clone();
            async move {
                ts.lock().await.push(Instant::now());
                Err(DownloadError::Timeout)
            }
        })
        .await;

        let ts = timestamps.lock().await;
        assert_eq!(ts.len(), 4); // 1 initial + 3 retries

        // Verify base exponential component: 20ms, 40ms, 80ms
        // With jitter (0..50%), ranges are: 20-30ms, 40-60ms, 80-120ms
        // Second base (40ms) is always > first max (30ms), so this holds
        let delay1 = ts[1].duration_since(ts[0]);
        let delay2 = ts[2].duration_since(ts[1]);
        assert!(
            delay2 > delay1,
            "second delay ({delay2:?}) should be greater than first ({delay1:?})"
        );
    }

    #[tokio::test]
    async fn returns_last_error_after_exhausting_retries() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) =
            with_retry::<ResponseData, _, _>(2, Duration::from_millis(10), || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err(DownloadError::Timeout)
                }
            })
            .await;

        assert!(matches!(result, Err(DownloadError::Timeout)));
        assert_eq!(retries, 2);
        assert_eq!(count.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn retries_attempted_count_is_correct() {
        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let (result, retries) =
            with_retry::<ResponseData, _, _>(2, Duration::from_millis(10), || {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err(DownloadError::HttpStatus {
                        code: 503,
                        message: "Service Unavailable".into(),
                        retry_after: None,
                    })
                }
            })
            .await;

        assert!(result.is_err());
        assert_eq!(retries, 2, "should report 2 retries attempted");
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "should call operation 3 times total"
        );
    }
}
