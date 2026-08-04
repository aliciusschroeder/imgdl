use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::types::DownloadError;

/// Metadata about a single downloaded file, serialized as JSON sidecar.
#[derive(Serialize)]
pub(crate) struct FileMetadata {
    pub url: String,
    pub filename: String,
    pub size_bytes: u64,
    pub content_hash: Option<String>,
    pub content_type: Option<String>,
    pub elapsed_ms: f64,
    pub downloaded_at: String,
    pub headers: HashMap<String, String>,
}

/// Write a metadata sidecar JSON file for a downloaded image.
///
/// The sidecar filename is the image filename with `.json` appended.
/// For example: `photo.jpg` -> `photo.jpg.json`.
pub(crate) async fn write_metadata_sidecar(
    image_path: &Path,
    metadata: &FileMetadata,
) -> Result<(), DownloadError> {
    let sidecar_path = {
        let mut name = image_path.file_name().unwrap_or_default().to_os_string();
        name.push(".json");
        image_path.with_file_name(name)
    };

    let json = serde_json::to_string_pretty(metadata)
        .map_err(|e| DownloadError::WriteError(e.to_string()))?;

    tokio::fs::write(&sidecar_path, json)
        .await
        .map_err(|e| DownloadError::WriteError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_metadata() -> FileMetadata {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "image/jpeg".to_string());
        headers.insert("content-length".to_string(), "1024".to_string());

        FileMetadata {
            url: "https://example.com/photo.jpg".to_string(),
            filename: "photo.jpg".to_string(),
            size_bytes: 1024,
            content_hash: Some("abcdef1234567890".to_string()),
            content_type: Some("image/jpeg".to_string()),
            elapsed_ms: 150.5,
            downloaded_at: "2025-01-15T10:30:00+00:00".to_string(),
            headers,
        }
    }

    #[tokio::test]
    async fn metadata_contains_all_fields() {
        let dir = TempDir::new().unwrap();
        let image_path = dir.path().join("photo.jpg");
        tokio::fs::write(&image_path, b"fake image").await.unwrap();

        let meta = sample_metadata();
        write_metadata_sidecar(&image_path, &meta).await.unwrap();

        let sidecar_path = dir.path().join("photo.jpg.json");
        let content = tokio::fs::read_to_string(&sidecar_path).await.unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(value.get("url").is_some());
        assert!(value.get("filename").is_some());
        assert!(value.get("size_bytes").is_some());
        assert!(value.get("content_hash").is_some());
        assert!(value.get("content_type").is_some());
        assert!(value.get("elapsed_ms").is_some());
        assert!(value.get("downloaded_at").is_some());
        assert!(value.get("headers").is_some());
    }

    #[test]
    fn metadata_filename_appends_json() {
        let image_path = Path::new("/tmp/photo.jpg");
        let mut name = image_path.file_name().unwrap().to_os_string();
        name.push(".json");
        let sidecar = image_path.with_file_name(name);
        assert_eq!(sidecar.file_name().unwrap(), "photo.jpg.json");
    }

    #[test]
    fn metadata_content_hash_populated() {
        let meta = sample_metadata();
        assert_eq!(meta.content_hash, Some("abcdef1234567890".to_string()));
    }

    #[tokio::test]
    async fn metadata_has_valid_iso_timestamp() {
        let dir = TempDir::new().unwrap();
        let image_path = dir.path().join("test.jpg");
        tokio::fs::write(&image_path, b"fake").await.unwrap();

        let mut meta = sample_metadata();
        meta.downloaded_at = chrono::Utc::now().to_rfc3339();
        write_metadata_sidecar(&image_path, &meta).await.unwrap();

        let sidecar = dir.path().join("test.jpg.json");
        let content = tokio::fs::read_to_string(&sidecar).await.unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        let ts = value["downloaded_at"].as_str().unwrap();
        // RFC 3339 timestamps contain 'T' and timezone info
        assert!(ts.contains('T'));
    }

    #[tokio::test]
    async fn metadata_contains_response_headers() {
        let dir = TempDir::new().unwrap();
        let image_path = dir.path().join("test.jpg");
        tokio::fs::write(&image_path, b"fake").await.unwrap();

        let meta = sample_metadata();
        write_metadata_sidecar(&image_path, &meta).await.unwrap();

        let sidecar = dir.path().join("test.jpg.json");
        let content = tokio::fs::read_to_string(&sidecar).await.unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        let headers = value["headers"].as_object().unwrap();
        assert!(headers.contains_key("content-type"));
        assert!(headers.contains_key("content-length"));
    }

    #[tokio::test]
    async fn metadata_is_valid_json() {
        let dir = TempDir::new().unwrap();
        let image_path = dir.path().join("test.jpg");
        tokio::fs::write(&image_path, b"fake").await.unwrap();

        let meta = sample_metadata();
        write_metadata_sidecar(&image_path, &meta).await.unwrap();

        let sidecar = dir.path().join("test.jpg.json");
        let content = tokio::fs::read_to_string(&sidecar).await.unwrap();
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
        assert!(parsed.is_ok());
    }
}
