use sha2::{Digest, Sha256};

use crate::config::NamingStrategy;

/// Generate a filename for a downloaded image based on the configured naming strategy.
pub(crate) fn generate_filename(
    url: &str,
    index: usize,
    bytes: &[u8],
    strategy: &NamingStrategy,
) -> String {
    let raw = match strategy {
        NamingStrategy::UrlBased => url_based_name(url),
        NamingStrategy::Sequential => sequential_name(index, url, bytes),
        NamingStrategy::ContentHash => content_hash_name(bytes, url),
        NamingStrategy::FileHeader => file_header_name(url, bytes),
    };
    truncate_filename(&raw)
}

/// Detect image format from magic bytes, returning the file extension (with leading dot).
fn detect_extension_from_magic_bytes(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return ".jpg";
    }
    if bytes.len() >= 4 && bytes[0..4] == [0x89, 0x50, 0x4E, 0x47] {
        return ".png";
    }
    if bytes.len() >= 4 && bytes[0..4] == [0x47, 0x49, 0x46, 0x38] {
        return ".gif";
    }
    if bytes.len() >= 12
        && bytes[0..4] == [0x52, 0x49, 0x46, 0x46]
        && bytes[8..12] == [0x57, 0x45, 0x42, 0x50]
    {
        return ".webp";
    }
    if bytes.len() >= 8 && bytes[4..8] == [0x66, 0x74, 0x79, 0x70] {
        return ".avif";
    }
    ".bin"
}

/// Strip query parameters and fragment from a URL, returning just the path portion.
fn strip_url_query_and_fragment(url: &str) -> &str {
    let without_query = url.split('?').next().unwrap_or(url);
    without_query.split('#').next().unwrap_or(without_query)
}

/// Extract file extension from URL path, returning None if not found.
fn extension_from_url(url: &str) -> Option<String> {
    let path = strip_url_query_and_fragment(url);
    let last_segment = path.rsplit('/').next()?;
    let dot_pos = last_segment.rfind('.')?;
    let ext = &last_segment[dot_pos + 1..];
    if ext.is_empty() || ext.len() > 10 {
        return None;
    }
    // Only allow alphanumeric extensions
    if ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        Some(ext.to_lowercase())
    } else {
        None
    }
}

/// Sanitize a filename by replacing special characters with underscores.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Truncate a filename to 200 characters (preserving extension).
/// Uses char_indices for safe multi-byte UTF-8 handling.
fn truncate_filename(name: &str) -> String {
    if name.chars().count() <= 200 {
        return name.to_string();
    }
    if let Some(dot_pos) = name.rfind('.') {
        let ext = &name[dot_pos..];
        let ext_chars = ext.chars().count();
        let max_stem_chars = 200 - ext_chars;
        if max_stem_chars > 0 {
            let stem_end = name
                .char_indices()
                .nth(max_stem_chars)
                .map(|(i, _)| i)
                .unwrap_or(dot_pos);
            let stem_end = stem_end.min(dot_pos);
            return format!("{}{}", &name[..stem_end], ext);
        }
    }
    let end = name
        .char_indices()
        .nth(200)
        .map(|(i, _)| i)
        .unwrap_or(name.len());
    name[..end].to_string()
}

fn url_based_name(url: &str) -> String {
    let path = strip_url_query_and_fragment(url);
    let segment = path.rsplit('/').next().unwrap_or("");

    if segment.is_empty() || !segment.contains('.') {
        // Fall back to hash of URL
        let hash = Sha256::digest(url.as_bytes());
        let hex: String = hash.iter().take(16).map(|b| format!("{b:02x}")).collect();
        return format!("{hex}.bin");
    }

    sanitize(segment)
}

fn sequential_name(index: usize, url: &str, bytes: &[u8]) -> String {
    let ext = extension_from_url(url).unwrap_or_else(|| {
        detect_extension_from_magic_bytes(bytes)
            .trim_start_matches('.')
            .to_string()
    });
    format!("{index:03}.{ext}")
}

fn content_hash_name(bytes: &[u8], url: &str) -> String {
    let hash = Sha256::digest(bytes);
    let hex: String = hash.iter().take(16).map(|b| format!("{b:02x}")).collect();
    let ext = extension_from_url(url).unwrap_or_else(|| {
        detect_extension_from_magic_bytes(bytes)
            .trim_start_matches('.')
            .to_string()
    });
    format!("{hex}.{ext}")
}

fn file_header_name(url: &str, bytes: &[u8]) -> String {
    let detected_ext = detect_extension_from_magic_bytes(bytes);
    let path = strip_url_query_and_fragment(url);
    let segment = path.rsplit('/').next().unwrap_or("");

    if segment.is_empty() || !segment.contains('.') {
        let hash = Sha256::digest(url.as_bytes());
        let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
        return format!("{hex}{detected_ext}");
    }

    let sanitized = sanitize(segment);
    // Replace the extension with the detected one
    if let Some(dot_pos) = sanitized.rfind('.') {
        format!("{}{}", &sanitized[..dot_pos], detected_ext)
    } else {
        format!("{sanitized}{detected_ext}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_based_extracts_filename_from_path() {
        let name = generate_filename(
            "https://example.com/images/photo.jpg",
            0,
            &[],
            &NamingStrategy::UrlBased,
        );
        assert_eq!(name, "photo.jpg");
    }

    #[test]
    fn url_based_sanitizes_special_characters() {
        let name = generate_filename(
            "https://example.com/photo (1).jpg",
            0,
            &[],
            &NamingStrategy::UrlBased,
        );
        assert_eq!(name, "photo__1_.jpg");
    }

    #[test]
    fn url_based_falls_back_to_hash_when_no_path() {
        let name = generate_filename("https://example.com/", 0, &[], &NamingStrategy::UrlBased);
        assert!(name.ends_with(".bin"));
        assert!(name.len() > 4); // has a hash prefix
    }

    #[test]
    fn url_based_preserves_extension() {
        let name = generate_filename(
            "https://example.com/test.png",
            0,
            &[],
            &NamingStrategy::UrlBased,
        );
        assert!(name.ends_with(".png"));
    }

    #[test]
    fn sequential_generates_zero_padded_names() {
        let name = generate_filename(
            "https://example.com/photo.jpg",
            1,
            &[],
            &NamingStrategy::Sequential,
        );
        assert_eq!(name, "001.jpg");

        let name = generate_filename(
            "https://example.com/photo.jpg",
            42,
            &[],
            &NamingStrategy::Sequential,
        );
        assert_eq!(name, "042.jpg");
    }

    #[test]
    fn sequential_infers_extension() {
        // From URL
        let name = generate_filename(
            "https://example.com/photo.png",
            1,
            &[],
            &NamingStrategy::Sequential,
        );
        assert_eq!(name, "001.png");

        // From magic bytes when URL has no extension
        let jpeg_bytes = [0xFF, 0xD8, 0xFF, 0xE0];
        let name = generate_filename(
            "https://example.com/image",
            2,
            &jpeg_bytes,
            &NamingStrategy::Sequential,
        );
        assert_eq!(name, "002.jpg");
    }

    #[test]
    fn content_hash_generates_sha256_filename() {
        let data = b"hello world";
        let name = generate_filename(
            "https://example.com/photo.jpg",
            0,
            data,
            &NamingStrategy::ContentHash,
        );
        // SHA-256 of "hello world" starts with b94d27b9...
        assert!(name.starts_with("b94d27b9"));
        assert!(name.ends_with(".jpg"));
        // 16 hex chars + dot + ext
        let stem = name.split('.').next().unwrap();
        assert_eq!(stem.len(), 32);
    }

    #[test]
    fn content_hash_differs_for_different_content() {
        let name1 = generate_filename(
            "https://example.com/a.jpg",
            0,
            b"content1",
            &NamingStrategy::ContentHash,
        );
        let name2 = generate_filename(
            "https://example.com/a.jpg",
            0,
            b"content2",
            &NamingStrategy::ContentHash,
        );
        assert_ne!(name1, name2);
    }

    #[test]
    fn content_hash_same_for_identical_content() {
        let name1 = generate_filename(
            "https://example.com/a.jpg",
            0,
            b"same",
            &NamingStrategy::ContentHash,
        );
        let name2 = generate_filename(
            "https://example.com/b.jpg",
            0,
            b"same",
            &NamingStrategy::ContentHash,
        );
        // Same content hash but different extensions
        let stem1 = name1.split('.').next().unwrap();
        let stem2 = name2.split('.').next().unwrap();
        assert_eq!(stem1, stem2);
    }

    #[test]
    fn file_header_detects_jpeg() {
        let bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        let name = generate_filename(
            "https://example.com/photo.png",
            0,
            &bytes,
            &NamingStrategy::FileHeader,
        );
        assert!(name.ends_with(".jpg"));
    }

    #[test]
    fn file_header_detects_png() {
        let bytes = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A];
        let name = generate_filename(
            "https://example.com/photo.jpg",
            0,
            &bytes,
            &NamingStrategy::FileHeader,
        );
        assert!(name.ends_with(".png"));
    }

    #[test]
    fn file_header_detects_gif() {
        let bytes = [0x47, 0x49, 0x46, 0x38, 0x39, 0x61];
        let name = generate_filename(
            "https://example.com/photo.jpg",
            0,
            &bytes,
            &NamingStrategy::FileHeader,
        );
        assert!(name.ends_with(".gif"));
    }

    #[test]
    fn file_header_detects_webp() {
        let bytes = [
            0x52, 0x49, 0x46, 0x46, // RIFF
            0x00, 0x00, 0x00, 0x00, // size (doesn't matter)
            0x57, 0x45, 0x42, 0x50, // WEBP
        ];
        let name = generate_filename(
            "https://example.com/photo.jpg",
            0,
            &bytes,
            &NamingStrategy::FileHeader,
        );
        assert!(name.ends_with(".webp"));
    }

    #[test]
    fn file_header_falls_back_to_bin() {
        let bytes = [0x00, 0x00, 0x00, 0x00];
        let name = generate_filename(
            "https://example.com/photo.jpg",
            0,
            &bytes,
            &NamingStrategy::FileHeader,
        );
        assert!(name.ends_with(".bin"));
    }

    #[test]
    fn long_filenames_truncated_to_200_chars() {
        let long_url = format!("https://example.com/{}.jpg", "a".repeat(250));
        let name = generate_filename(&long_url, 0, &[], &NamingStrategy::UrlBased);
        assert!(name.len() <= 200);
        assert!(name.ends_with(".jpg"));
    }
}
