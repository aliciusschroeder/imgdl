use serde::Serialize;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Result of downloading a single URL.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadResult {
    pub url: String,
    pub outcome: DownloadOutcome,
}

/// The outcome of a download attempt -- either success with file info, or failure with error details.
#[derive(Debug, Clone, Serialize)]
pub enum DownloadOutcome {
    Success {
        path: PathBuf,
        size_bytes: u64,
        /// SHA-256 hex string. Only computed when naming strategy is ContentHash,
        /// or when write_metadata/write_summary is enabled.
        content_hash: Option<String>,
        elapsed: Duration,
    },
    Failure {
        error: DownloadError,
        elapsed: Duration,
        retries_attempted: u32,
    },
}

/// All possible failure modes for a download.
#[derive(Debug, Clone, Serialize)]
pub enum DownloadError {
    /// Non-success HTTP status after exhausting retries for transient codes.
    HttpStatus {
        code: u16,
        message: String,
        /// Parsed Retry-After value (integer seconds) from 429 responses.
        #[serde(skip)]
        retry_after: Option<Duration>,
    },
    /// Request or connect timeout.
    Timeout,
    /// TCP or HTTP/2 handshake failure.
    ConnectionFailed(String),
    /// TLS handshake or certificate error.
    TlsError(String),
    /// Filesystem write failure.
    WriteError(String),
    /// Content-length mismatch, zero-byte response, or truncation.
    ValidationFailed(String),
    /// Hostname resolution failure.
    DnsResolutionFailed(String),
    /// Exceeded max redirect hops.
    TooManyRedirects(String),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::HttpStatus { code, message, .. } => {
                write!(f, "HTTP error {code}: {message}")
            }
            DownloadError::Timeout => write!(f, "request timed out"),
            DownloadError::ConnectionFailed(msg) => write!(f, "connection failed: {msg}"),
            DownloadError::TlsError(msg) => write!(f, "TLS error: {msg}"),
            DownloadError::WriteError(msg) => write!(f, "write error: {msg}"),
            DownloadError::ValidationFailed(msg) => write!(f, "validation failed: {msg}"),
            DownloadError::DnsResolutionFailed(msg) => {
                write!(f, "DNS resolution failed: {msg}")
            }
            DownloadError::TooManyRedirects(msg) => write!(f, "too many redirects: {msg}"),
        }
    }
}

impl std::error::Error for DownloadError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn test_download_error_display_all_variants() {
        let cases = vec![
            (
                DownloadError::HttpStatus {
                    code: 404,
                    message: "Not Found".into(),
                    retry_after: None,
                },
                "HTTP error 404: Not Found",
            ),
            (DownloadError::Timeout, "request timed out"),
            (
                DownloadError::ConnectionFailed("reset".into()),
                "connection failed: reset",
            ),
            (
                DownloadError::TlsError("bad cert".into()),
                "TLS error: bad cert",
            ),
            (
                DownloadError::WriteError("permission denied".into()),
                "write error: permission denied",
            ),
            (
                DownloadError::ValidationFailed("empty response body".into()),
                "validation failed: empty response body",
            ),
            (
                DownloadError::DnsResolutionFailed("NXDOMAIN".into()),
                "DNS resolution failed: NXDOMAIN",
            ),
            (
                DownloadError::TooManyRedirects("exceeded 5 hops".into()),
                "too many redirects: exceeded 5 hops",
            ),
        ];
        for (error, expected_substring) in cases {
            let display = format!("{error}");
            assert!(
                display.contains(expected_substring),
                "Expected '{display}' to contain '{expected_substring}'"
            );
        }
    }

    #[test]
    fn test_download_error_implements_std_error() {
        let error: Box<dyn std::error::Error> = Box::new(DownloadError::Timeout);
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn test_result_types_implement_debug_clone_serialize() {
        let result = DownloadResult {
            url: "https://example.com/img.jpg".to_string(),
            outcome: DownloadOutcome::Success {
                path: PathBuf::from("/tmp/img.jpg"),
                size_bytes: 1024,
                content_hash: None,
                elapsed: Duration::from_millis(100),
            },
        };
        let cloned = result.clone();
        let debug = format!("{cloned:?}");
        assert!(!debug.is_empty());
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("example.com"));
    }

    #[test]
    fn test_success_with_content_hash_none_serializes() {
        let outcome = DownloadOutcome::Success {
            path: PathBuf::from("/tmp/a.jpg"),
            size_bytes: 512,
            content_hash: None,
            elapsed: Duration::from_millis(50),
        };
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("null") || json.contains("content_hash"));
    }

    #[test]
    fn test_success_with_content_hash_some_serializes() {
        let outcome = DownloadOutcome::Success {
            path: PathBuf::from("/tmp/a.jpg"),
            size_bytes: 512,
            content_hash: Some("abc123".to_string()),
            elapsed: Duration::from_millis(50),
        };
        let json = serde_json::to_string(&outcome).unwrap();
        assert!(json.contains("abc123"));
    }
}
