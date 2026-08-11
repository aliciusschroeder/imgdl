use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3_stub_gen::derive::*;
use std::time::Duration;

const VALID_NAMING_STRATEGIES: &[&str] =
    &["content_hash", "url_based", "sequential", "file_header"];

/// Download configuration. All fields have sensible defaults.
#[gen_stub_pyclass]
#[pyclass(frozen)]
#[derive(Debug)]
pub struct Config {
    // Transport
    #[pyo3(get)]
    pub buffer_size: u32,
    #[pyo3(get)]
    pub connections_per_host: usize,
    #[pyo3(get)]
    pub dns_cache_ttl_secs: u64,

    // Concurrency
    #[pyo3(get)]
    pub max_concurrent: usize,
    #[pyo3(get)]
    pub max_concurrent_per_host: usize,

    // Retry
    #[pyo3(get)]
    pub max_retries: u32,
    #[pyo3(get)]
    pub retry_base_delay_ms: u64,

    // Timeouts
    #[pyo3(get)]
    pub connect_timeout_secs: f64,
    #[pyo3(get)]
    pub request_timeout_secs: f64,
    #[pyo3(get)]
    pub batch_timeout_secs: Option<f64>,

    // Output
    #[pyo3(get)]
    pub naming_strategy: String,
    #[pyo3(get)]
    pub write_metadata: bool,
    #[pyo3(get)]
    pub write_summary: bool,

    // Python-only
    #[pyo3(get)]
    pub runtime_threads: usize,
}

#[gen_stub_pymethods]
#[pymethods]
impl Config {
    #[new]
    #[pyo3(signature = (
        buffer_size = 524_288,
        connections_per_host = 1,
        dns_cache_ttl_secs = 300,
        max_concurrent = 200,
        max_concurrent_per_host = 100,
        max_retries = 3,
        retry_base_delay_ms = 100,
        connect_timeout_secs = 10.0,
        request_timeout_secs = 30.0,
        batch_timeout_secs = None,
        naming_strategy = String::from("url_based"),
        write_metadata = false,
        write_summary = false,
        runtime_threads = 0,
    ))]
    // Fourteen parameters is the point: every one is a keyword argument with a
    // default on the Python side, which is a far better API than an opaque
    // dict or a builder chain nobody can discover from a docstring.
    #[allow(clippy::too_many_arguments)]
    fn new(
        buffer_size: u32,
        connections_per_host: usize,
        dns_cache_ttl_secs: u64,
        max_concurrent: usize,
        max_concurrent_per_host: usize,
        max_retries: u32,
        retry_base_delay_ms: u64,
        connect_timeout_secs: f64,
        request_timeout_secs: f64,
        batch_timeout_secs: Option<f64>,
        naming_strategy: String,
        write_metadata: bool,
        write_summary: bool,
        runtime_threads: usize,
    ) -> PyResult<Self> {
        if !VALID_NAMING_STRATEGIES.contains(&naming_strategy.as_str()) {
            return Err(PyValueError::new_err(format!(
                "Invalid naming_strategy '{naming_strategy}'. Valid options: content_hash, url_based, sequential, file_header"
            )));
        }

        if connect_timeout_secs < 0.0 {
            return Err(PyValueError::new_err(
                "connect_timeout_secs must be non-negative",
            ));
        }
        if request_timeout_secs < 0.0 {
            return Err(PyValueError::new_err(
                "request_timeout_secs must be non-negative",
            ));
        }
        if let Some(bt) = batch_timeout_secs {
            if bt < 0.0 {
                return Err(PyValueError::new_err(
                    "batch_timeout_secs must be non-negative",
                ));
            }
        }

        Ok(Self {
            buffer_size,
            connections_per_host,
            dns_cache_ttl_secs,
            max_concurrent,
            max_concurrent_per_host,
            max_retries,
            retry_base_delay_ms,
            connect_timeout_secs,
            request_timeout_secs,
            batch_timeout_secs,
            naming_strategy,
            write_metadata,
            write_summary,
            runtime_threads,
        })
    }
}

impl Config {
    /// Create a Config with all default values (crate-internal use).
    pub(crate) fn with_defaults() -> Self {
        Self {
            buffer_size: 524_288,
            connections_per_host: 1,
            dns_cache_ttl_secs: 300,
            max_concurrent: 200,
            max_concurrent_per_host: 100,
            max_retries: 3,
            retry_base_delay_ms: 100,
            connect_timeout_secs: 10.0,
            request_timeout_secs: 30.0,
            batch_timeout_secs: None,
            naming_strategy: "url_based".to_string(),
            write_metadata: false,
            write_summary: false,
            runtime_threads: 0,
        }
    }
}

/// Convert a Python Config to a core Config.
/// The naming_strategy string has already been validated at construction time.
pub(crate) fn python_config_to_core(config: &Config) -> PyResult<imgdl_core::Config> {
    let naming_strategy = match config.naming_strategy.as_str() {
        "content_hash" => imgdl_core::NamingStrategy::ContentHash,
        "url_based" => imgdl_core::NamingStrategy::UrlBased,
        "sequential" => imgdl_core::NamingStrategy::Sequential,
        "file_header" => imgdl_core::NamingStrategy::FileHeader,
        _ => unreachable!("naming_strategy was validated at construction time"),
    };

    let defaults = imgdl_core::Config::default();

    Ok(imgdl_core::Config {
        stream_window_size: config.buffer_size,
        connections_per_host: config.connections_per_host,
        dns_cache_ttl: Duration::from_secs(config.dns_cache_ttl_secs),
        user_agent: defaults.user_agent,
        max_redirects: defaults.max_redirects,
        max_concurrent_global: config.max_concurrent,
        max_concurrent_per_host: config.max_concurrent_per_host,
        max_retries: config.max_retries,
        retry_base_delay: Duration::from_millis(config.retry_base_delay_ms),
        connect_timeout: Duration::from_secs_f64(config.connect_timeout_secs),
        request_timeout: Duration::from_secs_f64(config.request_timeout_secs),
        batch_timeout: config.batch_timeout_secs.map(Duration::from_secs_f64),
        naming_strategy,
        write_metadata: config.write_metadata,
        write_summary: config.write_summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Construction with defaults ---

    #[test]
    fn test_default_config() {
        let config = Config::new(
            524_288,
            1,
            300,
            200,
            100,
            3,
            100,
            10.0,
            30.0,
            None,
            "url_based".to_string(),
            false,
            false,
            0,
        )
        .unwrap();

        assert_eq!(config.buffer_size, 524_288);
        assert_eq!(config.connections_per_host, 1);
        assert_eq!(config.dns_cache_ttl_secs, 300);
        assert_eq!(config.max_concurrent, 200);
        assert_eq!(config.max_concurrent_per_host, 100);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.retry_base_delay_ms, 100);
        assert_eq!(config.connect_timeout_secs, 10.0);
        assert_eq!(config.request_timeout_secs, 30.0);
        assert_eq!(config.batch_timeout_secs, None);
        assert_eq!(config.naming_strategy, "url_based");
        assert!(!config.write_metadata);
        assert!(!config.write_summary);
        assert_eq!(config.runtime_threads, 0);
    }

    // --- Construction with explicit values ---

    #[test]
    fn test_config_with_explicit_values() {
        let config = Config::new(
            1_048_576,
            4,
            600,
            50,
            25,
            5,
            200,
            5.0,
            60.0,
            Some(120.0),
            "content_hash".to_string(),
            true,
            true,
            4,
        )
        .unwrap();

        assert_eq!(config.buffer_size, 1_048_576);
        assert_eq!(config.connections_per_host, 4);
        assert_eq!(config.dns_cache_ttl_secs, 600);
        assert_eq!(config.max_concurrent, 50);
        assert_eq!(config.max_concurrent_per_host, 25);
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.retry_base_delay_ms, 200);
        assert_eq!(config.connect_timeout_secs, 5.0);
        assert_eq!(config.request_timeout_secs, 60.0);
        assert_eq!(config.batch_timeout_secs, Some(120.0));
        assert_eq!(config.naming_strategy, "content_hash");
        assert!(config.write_metadata);
        assert!(config.write_summary);
        assert_eq!(config.runtime_threads, 4);
    }

    // --- NamingStrategy validation ---

    #[test]
    fn test_valid_naming_strategies() {
        for strategy in &["content_hash", "url_based", "sequential", "file_header"] {
            let result = Config::new(
                524_288,
                1,
                300,
                200,
                100,
                3,
                100,
                10.0,
                30.0,
                None,
                strategy.to_string(),
                false,
                false,
                0,
            );
            assert!(result.is_ok(), "Strategy '{strategy}' should be valid");
        }
    }

    #[test]
    fn test_invalid_naming_strategy() {
        let result = Config::new(
            524_288,
            1,
            300,
            200,
            100,
            3,
            100,
            10.0,
            30.0,
            None,
            "invalid".to_string(),
            false,
            false,
            0,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_naming_strategy() {
        let result = Config::new(
            524_288,
            1,
            300,
            200,
            100,
            3,
            100,
            10.0,
            30.0,
            None,
            "".to_string(),
            false,
            false,
            0,
        );
        assert!(result.is_err());
    }

    // --- Timeout validation ---

    #[test]
    fn test_negative_connect_timeout_rejected() {
        let result = Config::new(
            524_288,
            1,
            300,
            200,
            100,
            3,
            100,
            -1.0,
            30.0,
            None,
            "url_based".to_string(),
            false,
            false,
            0,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_negative_request_timeout_rejected() {
        let result = Config::new(
            524_288,
            1,
            300,
            200,
            100,
            3,
            100,
            10.0,
            -1.0,
            None,
            "url_based".to_string(),
            false,
            false,
            0,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_negative_batch_timeout_rejected() {
        let result = Config::new(
            524_288,
            1,
            300,
            200,
            100,
            3,
            100,
            10.0,
            30.0,
            Some(-1.0),
            "url_based".to_string(),
            false,
            false,
            0,
        );
        assert!(result.is_err());
    }

    // --- python_config_to_core conversion ---

    #[test]
    fn test_python_config_to_core_all_fields() {
        let config = Config::new(
            1_048_576,
            4,
            600,
            50,
            25,
            5,
            200,
            5.0,
            60.0,
            Some(120.0),
            "sequential".to_string(),
            true,
            true,
            4,
        )
        .unwrap();

        let core = python_config_to_core(&config).unwrap();

        assert_eq!(core.stream_window_size, 1_048_576);
        assert_eq!(core.connections_per_host, 4);
        assert_eq!(core.dns_cache_ttl, Duration::from_secs(600));
        assert_eq!(core.max_concurrent_global, 50);
        assert_eq!(core.max_concurrent_per_host, 25);
        assert_eq!(core.max_retries, 5);
        assert_eq!(core.retry_base_delay, Duration::from_millis(200));
        assert_eq!(core.connect_timeout, Duration::from_secs_f64(5.0));
        assert_eq!(core.request_timeout, Duration::from_secs_f64(60.0));
        assert_eq!(core.batch_timeout, Some(Duration::from_secs_f64(120.0)));
        assert_eq!(core.naming_strategy, imgdl_core::NamingStrategy::Sequential);
        assert!(core.write_metadata);
        assert!(core.write_summary);
    }

    #[test]
    fn test_conversion_duration_fields() {
        let config = Config::new(
            524_288,
            1,
            300,
            200,
            100,
            3,
            100,
            10.0,
            30.0,
            None,
            "url_based".to_string(),
            false,
            false,
            0,
        )
        .unwrap();

        let core = python_config_to_core(&config).unwrap();

        assert_eq!(core.dns_cache_ttl, Duration::from_secs(300));
        assert_eq!(core.connect_timeout, Duration::from_secs_f64(10.0));
        assert_eq!(core.retry_base_delay, Duration::from_millis(100));
        assert_eq!(core.batch_timeout, None);
    }

    #[test]
    fn test_conversion_batch_timeout_some() {
        let config = Config::new(
            524_288,
            1,
            300,
            200,
            100,
            3,
            100,
            10.0,
            30.0,
            Some(60.0),
            "url_based".to_string(),
            false,
            false,
            0,
        )
        .unwrap();

        let core = python_config_to_core(&config).unwrap();
        assert_eq!(core.batch_timeout, Some(Duration::from_secs_f64(60.0)));
    }

    #[test]
    fn test_conversion_naming_strategies() {
        let cases = vec![
            ("content_hash", imgdl_core::NamingStrategy::ContentHash),
            ("url_based", imgdl_core::NamingStrategy::UrlBased),
            ("sequential", imgdl_core::NamingStrategy::Sequential),
            ("file_header", imgdl_core::NamingStrategy::FileHeader),
        ];

        for (strategy_str, expected) in cases {
            let config = Config::new(
                524_288,
                1,
                300,
                200,
                100,
                3,
                100,
                10.0,
                30.0,
                None,
                strategy_str.to_string(),
                false,
                false,
                0,
            )
            .unwrap();

            let core = python_config_to_core(&config).unwrap();
            assert_eq!(core.naming_strategy, expected);
        }
    }

    #[test]
    fn test_conversion_preserves_core_defaults_for_unmapped_fields() {
        let config = Config::new(
            524_288,
            1,
            300,
            200,
            100,
            3,
            100,
            10.0,
            30.0,
            None,
            "url_based".to_string(),
            false,
            false,
            0,
        )
        .unwrap();

        let core = python_config_to_core(&config).unwrap();
        let defaults = imgdl_core::Config::default();

        assert_eq!(core.user_agent, defaults.user_agent);
        assert_eq!(core.max_redirects, defaults.max_redirects);
    }
}
