use serde::Serialize;
use std::time::Duration;

/// Strategy for generating output filenames for downloaded images.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub enum NamingStrategy {
    /// Extract filename from the URL path segment. Sanitize special characters.
    #[default]
    UrlBased,
    /// Zero-padded sequential index: 001.jpg, 002.png, etc.
    Sequential,
    /// SHA-256 hash of image bytes, truncated to 16 hex characters.
    ContentHash,
    /// Detect image format from magic bytes and use detected extension.
    FileHeader,
}

/// Configuration for the image downloader.
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    pub stream_window_size: u32,
    pub connections_per_host: usize,
    pub dns_cache_ttl: Duration,
    pub user_agent: String,
    pub max_redirects: u8,
    pub max_concurrent_global: usize,
    pub max_concurrent_per_host: usize,
    pub max_retries: u32,
    pub retry_base_delay: Duration,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub batch_timeout: Option<Duration>,
    pub naming_strategy: NamingStrategy,
    pub write_metadata: bool,
    pub write_summary: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            stream_window_size: 524_288,
            connections_per_host: 1,
            dns_cache_ttl: Duration::from_secs(300),
            user_agent: "imgdl/0.1".to_string(),
            max_redirects: 5,
            max_concurrent_global: 200,
            max_concurrent_per_host: 100,
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            batch_timeout: None,
            naming_strategy: NamingStrategy::default(),
            write_metadata: false,
            write_summary: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_config_default_values() {
        let config = Config::default();
        assert_eq!(config.stream_window_size, 524_288);
        assert_eq!(config.connections_per_host, 1);
        assert_eq!(config.dns_cache_ttl, Duration::from_secs(300));
        assert_eq!(config.user_agent, "imgdl/0.1");
        assert_eq!(config.max_redirects, 5);
        assert_eq!(config.max_concurrent_global, 200);
        assert_eq!(config.max_concurrent_per_host, 100);
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.retry_base_delay, Duration::from_millis(100));
        assert_eq!(config.connect_timeout, Duration::from_secs(10));
        assert_eq!(config.request_timeout, Duration::from_secs(30));
        assert!(config.batch_timeout.is_none());
        assert_eq!(config.naming_strategy, NamingStrategy::UrlBased);
        assert!(!config.write_metadata);
        assert!(!config.write_summary);
    }

    #[test]
    fn test_config_fields_are_publicly_modifiable() {
        let config = Config {
            max_retries: 5,
            connections_per_host: 4,
            batch_timeout: Some(Duration::from_secs(60)),
            ..Default::default()
        };
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.connections_per_host, 4);
        assert_eq!(config.batch_timeout, Some(Duration::from_secs(60)));
    }

    #[test]
    fn test_config_implements_clone_debug_serialize() {
        let config = Config::default();
        let cloned = config.clone();
        let debug_str = format!("{cloned:?}");
        assert!(!debug_str.is_empty());
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("stream_window_size"));
    }

    #[test]
    fn test_naming_strategy_default_is_url_based() {
        assert_eq!(NamingStrategy::default(), NamingStrategy::UrlBased);
    }

    #[test]
    fn test_naming_strategy_implements_required_traits() {
        let strategy = NamingStrategy::ContentHash;
        let cloned = strategy.clone();
        let debug_str = format!("{cloned:?}");
        assert!(!debug_str.is_empty());
        let json = serde_json::to_string(&strategy).unwrap();
        assert!(json.contains("ContentHash"));
        assert_eq!(strategy, NamingStrategy::ContentHash);
    }
}
