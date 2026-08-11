use std::path::PathBuf;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3_stub_gen::derive::*;

mod config;
mod result;
mod runtime;

use config::Config;
use result::DownloadResult;

use config::python_config_to_core;
use result::core_result_to_python;
use runtime::{get_downloader, get_plain_downloader, get_runtime};

/// Download images from URLs to the output directory.
///
/// Args:
///     urls: List of image URLs to download.
///     output_dir: Directory to save downloaded images. Defaults to current directory.
///     config: Download configuration. Defaults to Config() with all defaults.
///
/// Returns:
///     List of DownloadResult objects, one per URL, in input order.
///
/// Raises:
///     ValueError: If urls is empty.
///     OSError: If output directory cannot be created or written to.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (urls, output_dir=".", config=None))]
fn download_images(
    py: Python<'_>,
    urls: Vec<String>,
    output_dir: &str,
    config: Option<&Config>,
) -> PyResult<Vec<DownloadResult>> {
    // Step 1: Input validation
    if urls.is_empty() {
        return Err(PyValueError::new_err("urls list cannot be empty"));
    }

    // Step 2: Config resolution
    let py_config;
    let config_ref = match config {
        Some(c) => c,
        None => {
            py_config = Config::with_defaults();
            &py_config
        }
    };
    let core_config = python_config_to_core(config_ref)?;

    // Step 3: Path conversion
    let output_path = PathBuf::from(output_dir);

    // Step 4: Prepare URL references
    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();

    // Step 5: Runtime acquisition
    let rt = get_runtime(config_ref.runtime_threads);

    // Step 6: Execute with GIL released
    let downloader = get_downloader(core_config);
    let core_results =
        py.detach(|| rt.block_on(downloader.download_batch(&url_refs, &output_path)));

    // Step 7: Convert results
    Ok(core_results
        .into_iter()
        .map(core_result_to_python)
        .collect())
}

/// Same as `download_images`, but over plain TCP with no TLS negotiation.
///
/// **Test-only.** Not re-exported from the `imgdl` package and not covered by
/// any stability guarantee.
///
/// The production path always negotiates TLS, so an `http://` URL cannot
/// succeed through `download_images`. Without this hook, the Python test suite
/// could only ever assert on error paths offline — which is why the original
/// suite had no successful-download coverage at all. This lets pytest run the
/// real orchestrator, pool and output layers against a local HTTP server.
///
/// Uses its own process-global downloader, so it cannot disturb the state that
/// `download_images` relies on.
#[gen_stub_pyfunction]
#[pyfunction]
#[pyo3(signature = (urls, output_dir=".", config=None))]
fn _download_images_plaintext(
    py: Python<'_>,
    urls: Vec<String>,
    output_dir: &str,
    config: Option<&Config>,
) -> PyResult<Vec<DownloadResult>> {
    if urls.is_empty() {
        return Err(PyValueError::new_err("urls list cannot be empty"));
    }

    let py_config;
    let config_ref = match config {
        Some(c) => c,
        None => {
            py_config = Config::with_defaults();
            &py_config
        }
    };
    let core_config = python_config_to_core(config_ref)?;
    let output_path = PathBuf::from(output_dir);
    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();

    let rt = get_runtime(config_ref.runtime_threads);
    let downloader = get_plain_downloader(core_config);
    let core_results =
        py.detach(|| rt.block_on(downloader.download_batch(&url_refs, &output_path)));

    Ok(core_results
        .into_iter()
        .map(core_result_to_python)
        .collect())
}

/// Private extension module backing the public `imgdl` package.
///
/// Everything users touch is re-exported from `python/imgdl/__init__.py`;
/// this module is an implementation detail and may change shape between
/// patch releases.
#[pymodule]
fn _imgdl(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Config>()?;
    m.add_class::<DownloadResult>()?;
    m.add_function(wrap_pyfunction!(download_images, m)?)?;
    m.add_function(wrap_pyfunction!(_download_images_plaintext, m)?)?;
    // Single-sourced from Cargo.toml, so `imgdl.__version__` cannot drift from
    // the crate version even in an uninstalled `maturin develop` build.
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

/// Entry point used by the `stub_gen` binary.
///
/// This is a hand-written replacement for `define_stub_info_gatherer!`, which
/// assumes `pyproject.toml` sits next to `Cargo.toml`. Ours lives at the
/// workspace root so that `uv add git+<repo>` works without a `#subdirectory=`
/// fragment, so the path is spelled out here.
///
/// It must live in this crate (not in the `stub_gen` binary) because
/// `inventory` collects the `#[gen_stub_*]` registrations per-crate.
pub fn stub_info() -> pyo3_stub_gen::Result<pyo3_stub_gen::StubInfo> {
    let manifest_dir: &std::path::Path = env!("CARGO_MANIFEST_DIR").as_ref();
    let pyproject = manifest_dir.join("..").join("..").join("pyproject.toml");
    pyo3_stub_gen::StubInfo::from_pyproject_toml(pyproject)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_urls_rejected() {
        Python::attach(|py| {
            let result = download_images(py, vec![], ".", None);
            assert!(result.is_err());
        });
    }

    fn fast_config() -> Config {
        Config {
            buffer_size: 524_288,
            connections_per_host: 1,
            dns_cache_ttl_secs: 300,
            max_concurrent: 200,
            max_concurrent_per_host: 100,
            max_retries: 0,
            retry_base_delay_ms: 100,
            connect_timeout_secs: 0.5,
            request_timeout_secs: 0.5,
            batch_timeout_secs: None,
            naming_strategy: "url_based".to_string(),
            write_metadata: false,
            write_summary: false,
            runtime_threads: 0,
        }
    }

    #[test]
    fn test_download_images_with_default_config() {
        Python::attach(|py| {
            let config = fast_config();
            let result = download_images(
                py,
                vec!["http://localhost:1/nonexistent.jpg".to_string()],
                "/tmp",
                Some(&config),
            );
            assert!(result.is_ok());
            let results = result.unwrap();
            assert_eq!(results.len(), 1);
            assert!(!results[0].success);
        });
    }

    #[test]
    fn test_download_images_with_explicit_config() {
        Python::attach(|py| {
            let config = fast_config();
            let result = download_images(
                py,
                vec!["http://localhost:1/nonexistent.jpg".to_string()],
                "/tmp",
                Some(&config),
            );
            assert!(result.is_ok());
            let results = result.unwrap();
            assert_eq!(results.len(), 1);
        });
    }
}
