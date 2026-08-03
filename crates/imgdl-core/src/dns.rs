use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::Instant;

use tracing::Instrument;

use crate::types::DownloadError;

/// Thread-safe async DNS resolver with TTL-based caching.
///
/// Wraps `tokio::net::lookup_host()` and caches resolved addresses
/// per hostname. Designed for workloads with a small number of distinct
/// hosts (~10) where a full DNS library like hickory-dns is overkill.
pub(crate) struct DnsCache {
    cache: RwLock<HashMap<String, CacheEntry>>,
    ttl: Duration,
    negative_ttl: Duration,
}

struct CacheEntry {
    result: Result<Vec<SocketAddr>, String>,
    expires_at: Instant,
}

impl DnsCache {
    /// Create a new DNS cache with the given positive TTL.
    /// Negative cache entries use a hardcoded 30-second TTL.
    pub(crate) fn new(ttl: Duration) -> Self {
        DnsCache {
            cache: RwLock::new(HashMap::new()),
            ttl,
            negative_ttl: Duration::from_secs(30),
        }
    }

    /// Resolve a hostname to socket addresses, using the cache.
    pub(crate) async fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<SocketAddr>, DownloadError> {
        self.resolve_inner(host, port)
            .instrument(tracing::debug_span!("dns_resolve", host = %host))
            .await
    }

    async fn resolve_inner(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, DownloadError> {
        // Check cache with read lock
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(host) {
                if Instant::now() < entry.expires_at {
                    return match &entry.result {
                        Ok(addrs) => {
                            tracing::debug!("dns cache hit");
                            Ok(addrs.clone())
                        }
                        Err(msg) => {
                            tracing::debug!("dns negative cache hit");
                            Err(DownloadError::DnsResolutionFailed(msg.clone()))
                        }
                    };
                }
            }
        }

        // Cache miss or expired - perform resolution
        let lookup_str = format!("{host}:{port}");
        let lookup_result = tokio::net::lookup_host(&lookup_str)
            .await
            .map(|addrs_iter| addrs_iter.collect::<Vec<SocketAddr>>());

        match lookup_result {
            Ok(addrs) => {
                tracing::debug!(addrs = ?addrs, "dns resolved");
                let mut cache = self.cache.write().await;
                cache.insert(
                    host.to_string(),
                    CacheEntry {
                        result: Ok(addrs.clone()),
                        expires_at: Instant::now() + self.ttl,
                    },
                );
                Ok(addrs)
            }
            Err(e) => {
                let error_message = e.to_string();
                tracing::warn!(error = %e, "dns resolution failed");
                let mut cache = self.cache.write().await;
                cache.insert(
                    host.to_string(),
                    CacheEntry {
                        result: Err(error_message.clone()),
                        expires_at: Instant::now() + self.negative_ttl,
                    },
                );
                Err(DownloadError::DnsResolutionFailed(error_message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn resolve_returns_addresses_for_valid_hostname() {
        let cache = DnsCache::new(Duration::from_secs(60));
        let result = cache.resolve("localhost", 80).await;
        assert!(result.is_ok(), "Expected Ok, got {result:?}");
        let addrs = result.unwrap();
        assert!(!addrs.is_empty(), "Expected at least one address");
    }

    #[tokio::test]
    async fn resolve_returns_cached_result_on_second_call() {
        let cache = DnsCache::new(Duration::from_secs(300));
        let first = cache.resolve("localhost", 80).await.unwrap();
        let second = cache.resolve("localhost", 80).await.unwrap();
        assert_eq!(first, second, "Cached result should match first result");
    }

    #[tokio::test]
    async fn resolve_re_resolves_after_ttl_expires() {
        let cache = DnsCache::new(Duration::from_millis(1));
        let first = cache.resolve("localhost", 80).await.unwrap();
        assert!(!first.is_empty());
        tokio::time::sleep(Duration::from_millis(10)).await;
        let second = cache.resolve("localhost", 80).await.unwrap();
        assert!(!second.is_empty(), "Should re-resolve after TTL expires");
    }

    #[tokio::test]
    async fn resolve_caches_dns_failures_with_shorter_ttl() {
        let cache = DnsCache::new(Duration::from_secs(300));
        let first = cache
            .resolve("this-host-definitely-does-not-exist.invalid", 80)
            .await;
        assert!(first.is_err(), "Expected DNS failure");

        let start = Instant::now();
        let second = cache
            .resolve("this-host-definitely-does-not-exist.invalid", 80)
            .await;
        let elapsed = start.elapsed();
        assert!(second.is_err(), "Expected cached DNS failure");
        assert!(
            elapsed < Duration::from_millis(100),
            "Negative cache hit should be fast, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn resolve_returns_dns_resolution_failed_for_bad_hostname() {
        let cache = DnsCache::new(Duration::from_secs(60));
        let result = cache.resolve("nonexistent.invalid", 80).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            DownloadError::DnsResolutionFailed(_) => {}
            other => panic!("Expected DnsResolutionFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_works_for_different_hosts_independently() {
        let cache = DnsCache::new(Duration::from_secs(60));
        let result_80 = cache.resolve("localhost", 80).await;
        let result_443 = cache.resolve("localhost", 443).await;
        assert!(result_80.is_ok());
        assert!(result_443.is_ok());
    }

    #[tokio::test]
    async fn concurrent_resolve_calls_are_safe() {
        use std::sync::Arc;

        let cache = Arc::new(DnsCache::new(Duration::from_secs(60)));
        let mut handles = vec![];
        for _ in 0..10 {
            let cache = Arc::clone(&cache);
            handles.push(tokio::spawn(
                async move { cache.resolve("localhost", 80).await },
            ));
        }
        let mut results = vec![];
        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
            results.push(result.unwrap());
        }
        // All should resolve to the same set of addresses
        for r in &results[1..] {
            assert_eq!(&results[0], r);
        }
    }
}
