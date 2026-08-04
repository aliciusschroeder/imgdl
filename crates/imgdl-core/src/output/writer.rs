use std::path::Path;

use bytes::Bytes;

use crate::types::DownloadError;

/// Write image bytes to disk at the specified path.
///
/// Uses tokio::fs::write() for simplicity since images are <500kB.
/// The parent directory must already exist.
/// Maps I/O errors to DownloadError::WriteError.
pub(crate) async fn write_image(bytes: &Bytes, path: &Path) -> Result<(), DownloadError> {
    tokio::fs::write(path, bytes)
        .await
        .map_err(|e| DownloadError::WriteError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tempfile::TempDir;

    #[tokio::test]
    async fn writes_bytes_to_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.jpg");
        let data = Bytes::from_static(b"image data");
        write_image(&data, &path).await.unwrap();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn written_bytes_match_input() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.png");
        let data = Bytes::from(vec![1u8, 2, 3, 4, 5]);
        write_image(&data, &path).await.unwrap();
        let read_back = tokio::fs::read(&path).await.unwrap();
        assert_eq!(read_back, vec![1u8, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn returns_write_error_on_permission_denied() {
        let path = Path::new("/proc/nonexistent/test.jpg");
        let data = Bytes::from_static(b"data");
        let result = write_image(&data, path).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            DownloadError::WriteError(_) => {}
            other => panic!("expected WriteError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn writes_complete_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("complete.jpg");
        let data = Bytes::from_static(b"complete write test");
        write_image(&data, &path).await.unwrap();
        let content = tokio::fs::read(&path).await.unwrap();
        assert_eq!(content, b"complete write test");
    }
}
