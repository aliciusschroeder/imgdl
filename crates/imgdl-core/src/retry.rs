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
