use bytes::Bytes;
use http::HeaderMap;

use crate::types::DownloadError;

/// Validate a downloaded response body against its headers.
///
/// Checks:
/// 1. Zero-byte rejection: empty body returns ValidationFailed
/// 2. Content-Length mismatch: if header present, must match actual body length
pub(crate) fn validate_response(bytes: &Bytes, headers: &HeaderMap) -> Result<(), DownloadError> {
    if bytes.is_empty() {
        return Err(DownloadError::ValidationFailed(
            "empty response body".into(),
        ));
    }

    if let Some(cl) = headers.get(http::header::CONTENT_LENGTH) {
        if let Ok(expected) = cl.to_str().unwrap_or("").parse::<usize>() {
            let actual = bytes.len();
            if expected != actual {
                return Err(DownloadError::ValidationFailed(format!(
                    "content-length mismatch: expected {expected}, got {actual}"
                )));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http::HeaderMap;

    #[test]
    fn passes_valid_response_with_matching_content_length() {
        let body = Bytes::from(vec![0u8; 100]);
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, "100".parse().unwrap());
        assert!(validate_response(&body, &headers).is_ok());
    }

    #[test]
    fn passes_when_no_content_length_header() {
        let body = Bytes::from(vec![1u8; 50]);
        let headers = HeaderMap::new();
        assert!(validate_response(&body, &headers).is_ok());
    }

    #[test]
    fn rejects_zero_byte_body() {
        let body = Bytes::new();
        let headers = HeaderMap::new();
        let err = validate_response(&body, &headers).unwrap_err();
        assert!(matches!(err, DownloadError::ValidationFailed(_)));
    }

    #[test]
    fn rejects_content_length_mismatch() {
        let body = Bytes::from(vec![0u8; 500]);
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, "1000".parse().unwrap());
        let err = validate_response(&body, &headers).unwrap_err();
        match err {
            DownloadError::ValidationFailed(msg) => {
                assert!(msg.contains("content-length mismatch"));
            }
            _ => panic!("expected ValidationFailed"),
        }
    }

    #[test]
    fn passes_exact_content_length_match() {
        let body = Bytes::from(vec![0u8; 256]);
        let mut headers = HeaderMap::new();
        headers.insert(http::header::CONTENT_LENGTH, "256".parse().unwrap());
        assert!(validate_response(&body, &headers).is_ok());
    }
}
