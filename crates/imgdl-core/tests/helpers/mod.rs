use std::time::Duration;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Initialize tracing for tests. Safe to call multiple times.
pub(crate) fn init_tracing() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init()
            .ok();
    });
}

/// Returns bytes starting with JPEG magic bytes (FF D8 FF).
pub(crate) fn fake_jpeg() -> Vec<u8> {
    let mut data = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
    data.extend(vec![0xAA; 90]); // pad to 100 bytes
    data
}

/// Returns bytes starting with PNG magic bytes (89 50 4E 47 ...).
pub(crate) fn fake_png() -> Vec<u8> {
    let mut data = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    data.extend(vec![0xBB; 92]); // pad to 100 bytes
    data
}

/// Returns arbitrary non-image bytes for testing .bin fallback.
pub(crate) fn fake_unknown() -> Vec<u8> {
    vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09]
        .into_iter()
        .cycle()
        .take(100)
        .collect()
}

/// Mount a wiremock mock that returns the given body bytes with a 200 status.
pub(crate) async fn mount_image_mock(server: &MockServer, url_path: &str, body: &[u8]) {
    Mock::given(method("GET"))
        .and(path(url_path))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(server)
        .await;
}

/// Mount a mock that returns the given status code with a small body.
pub(crate) async fn mount_status_mock(server: &MockServer, url_path: &str, status: u16) {
    Mock::given(method("GET"))
        .and(path(url_path))
        .respond_with(ResponseTemplate::new(status).set_body_bytes(b"error".to_vec()))
        .mount(server)
        .await;
}

/// Mount a mock that returns a redirect to the given location.
pub(crate) async fn mount_redirect_mock(
    server: &MockServer,
    url_path: &str,
    status: u16,
    location: &str,
) {
    Mock::given(method("GET"))
        .and(path(url_path))
        .respond_with(ResponseTemplate::new(status).insert_header("Location", location))
        .mount(server)
        .await;
}

/// Mount a mock that delays for the given duration before responding.
pub(crate) async fn mount_delayed_mock(
    server: &MockServer,
    url_path: &str,
    body: &[u8],
    delay: Duration,
) {
    Mock::given(method("GET"))
        .and(path(url_path))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.to_vec())
                .set_delay(delay),
        )
        .mount(server)
        .await;
}
