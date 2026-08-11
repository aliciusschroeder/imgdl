use std::sync::OnceLock;
use tokio::runtime::Runtime;

/// Holds the runtime and the thread count it was initialized with.
struct RuntimeState {
    runtime: Runtime,
    threads: usize,
}

static RUNTIME_STATE: OnceLock<RuntimeState> = OnceLock::new();

/// Build a new Tokio multi-thread runtime.
///
/// - `threads == 0`: Use Tokio defaults (one worker thread per CPU core).
/// - `threads > 0`: Use exactly that many worker threads.
fn build_runtime(threads: usize) -> std::io::Result<Runtime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if threads > 0 {
        builder.worker_threads(threads);
    }
    builder.build()
}

/// Get or create the module-level Tokio runtime.
///
/// The first call initializes the runtime with the given thread count.
/// Subsequent calls return the existing runtime. If the requested thread
/// count differs from the initial one, a warning is logged via `tracing::warn!`.
///
/// # Panics
///
/// Panics if the Tokio runtime cannot be created (e.g., OS thread limit reached).
pub(crate) fn get_runtime(threads: usize) -> &'static Runtime {
    let state = RUNTIME_STATE.get_or_init(|| RuntimeState {
        runtime: build_runtime(threads).expect("Failed to create Tokio runtime"),
        threads,
    });
    if state.threads != threads {
        tracing::warn!(
            "Tokio runtime already initialized with {} threads, ignoring requested {}",
            state.threads,
            threads
        );
    }
    &state.runtime
}

static DOWNLOADER: OnceLock<imgdl_core::Downloader> = OnceLock::new();

/// Get or create the cached Downloader instance.
///
/// The first call creates a Downloader with the given config.
/// Subsequent calls return the existing Downloader, ignoring the config
/// (similar to runtime thread count behavior).
pub(crate) fn get_downloader(config: imgdl_core::Config) -> &'static imgdl_core::Downloader {
    DOWNLOADER.get_or_init(|| imgdl_core::Downloader::new(config))
}

static PLAIN_DOWNLOADER: OnceLock<imgdl_core::Downloader> = OnceLock::new();

/// Get or create a plaintext (no TLS) Downloader.
///
/// Test-only escape hatch, exposed to Python as `_download_images_plaintext`.
///
/// Why it exists: the production path always negotiates TLS, so a `http://`
/// URL cannot succeed. That left the Python test suite unable to assert
/// anything about a *successful* download without reaching the public
/// internet — every offline test could only check error paths, and
/// `just cov-py` reported almost no coverage of the download path as a result.
///
/// This lets pytest point at a local HTTP server and exercise the real
/// orchestrator, pool and output layers offline. It does not touch the
/// production path: `download_images` still uses `get_downloader`.
pub(crate) fn get_plain_downloader(config: imgdl_core::Config) -> &'static imgdl_core::Downloader {
    PLAIN_DOWNLOADER.get_or_init(|| imgdl_core::Downloader::new_plain(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_runtime_default_threads() {
        let rt = build_runtime(0);
        assert!(rt.is_ok());
        let rt = rt.unwrap();
        let result = rt.block_on(async { 42 });
        assert_eq!(result, 42);
    }

    #[test]
    fn test_build_runtime_specific_threads() {
        let rt = build_runtime(2);
        assert!(rt.is_ok());
        let rt = rt.unwrap();
        let result = rt.block_on(async { 42 });
        assert_eq!(result, 42);
    }

    #[test]
    fn test_get_runtime_returns_same_instance() {
        let rt1 = get_runtime(0);
        let rt2 = get_runtime(0);
        assert!(std::ptr::eq(rt1, rt2));
    }

    #[test]
    fn test_get_runtime_different_thread_count_no_panic() {
        let _rt1 = get_runtime(0);
        let _rt2 = get_runtime(4);
    }
}
