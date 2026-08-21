use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use http::Uri;
use indexmap::IndexMap;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use tracing::Instrument;

use crate::config::{Config, NamingStrategy};
use crate::dns::DnsCache;
use crate::output::metadata::{write_metadata_sidecar, FileMetadata};
use crate::output::naming::generate_filename;
use crate::output::summary::write_batch_summary;
use crate::output::validation::validate_response;
use crate::output::writer::write_image;
use crate::pool::ConnectionPool;
use crate::retry::with_retry;
use crate::tls::build_tls_config;
use crate::transport::{execute_request, RequestConfig, Sender, TransportResult};
use crate::types::{DownloadError, DownloadOutcome, DownloadResult};

/// The main entry point for batch image downloading.
///
/// Owns persistent state (connection pool, DNS cache, TLS config) that survives
/// across sequential `download_batch()` calls, enabling connection reuse.
pub struct Downloader {
    pool: Arc<ConnectionPool>,
    config: Arc<Config>,
}

impl std::fmt::Debug for Downloader {
    // Hand-written: the pool holds live connections whose Debug output
    // would be enormous and would change between runs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Downloader")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Downloader {
    /// Create a new Downloader with the given configuration.
    pub fn new(config: Config) -> Self {
        let tls_config = build_tls_config();
        let config = Arc::new(config);
        let dns_cache = Arc::new(DnsCache::new(config.dns_cache_ttl));
        let pool = Arc::new(ConnectionPool::new(
            config.clone(),
            dns_cache.clone(),
            tls_config.clone(),
        ));
        Downloader { pool, config }
    }

    /// Download a batch of URLs to the specified output directory.
    ///
    /// Returns results in the same order as the input URLs. Duplicate URLs are
    /// downloaded once and the result is cloned to all positions.
    pub async fn download_batch(&self, urls: &[&str], output_dir: &Path) -> Vec<DownloadResult> {
        self.download_batch_inner(urls, output_dir)
            .instrument(tracing::info_span!("download_batch", urls = urls.len()))
            .await
    }

    async fn download_batch_inner(&self, urls: &[&str], output_dir: &Path) -> Vec<DownloadResult> {
        tracing::info!("batch started");

        // Phase 1: Early returns
        if urls.is_empty() {
            return Vec::new();
        }

        if let Err(e) = tokio::fs::create_dir_all(output_dir).await {
            return urls
                .iter()
                .map(|url| DownloadResult {
                    url: url.to_string(),
                    outcome: DownloadOutcome::Failure {
                        error: DownloadError::WriteError(format!(
                            "failed to create output directory: {e}"
                        )),
                        elapsed: std::time::Duration::ZERO,
                        retries_attempted: 0,
                    },
                })
                .collect();
        }

        let global_sem = Arc::new(Semaphore::new(self.config.max_concurrent_global));
        let host_sems: Arc<Mutex<HashMap<String, Arc<Semaphore>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let batch_start = Instant::now();

        // Phase 2: URL Deduplication
        let mut dedup: IndexMap<String, Vec<usize>> = IndexMap::new();
        for (i, url) in urls.iter().enumerate() {
            dedup.entry(url.to_string()).or_default().push(i);
        }
        let unique_count = dedup.len();
        let total_count = urls.len();

        // Phase 3: Fan-out
        let mut join_set: JoinSet<(String, DownloadResult)> = JoinSet::new();

        for (url_str, indices) in &dedup {
            let url_str = url_str.clone();
            let first_index = indices[0];
            let config = self.config.clone();
            let pool = self.pool.clone();
            let global_sem = global_sem.clone();
            let host_sems = host_sems.clone();
            let output_dir = output_dir.to_path_buf();

            let span = tracing::info_span!("download", url = %url_str);
            join_set.spawn(
                Self::download_one(
                    url_str,
                    first_index,
                    config,
                    pool,
                    global_sem,
                    host_sems,
                    output_dir,
                )
                .instrument(span),
            );
        }

        // Phase 3+4: Collection with optional batch timeout
        let mut result_map: HashMap<String, DownloadResult> = HashMap::new();
        let deadline = self
            .config
            .batch_timeout
            .map(|d| tokio::time::Instant::now() + d);

        loop {
            let join_result = if let Some(dl) = deadline {
                match tokio::time::timeout_at(dl, join_set.join_next()).await {
                    Ok(Some(r)) => r,
                    Ok(None) => break,
                    Err(_) => break,
                }
            } else {
                match join_set.join_next().await {
                    Some(r) => r,
                    None => break,
                }
            };

            match join_result {
                Ok((url, dr)) => {
                    result_map.insert(url, dr);
                }
                Err(e) => {
                    tracing::error!("task panicked: {e}");
                }
            }
        }

        // Drop remaining tasks (cancels timed-out tasks)
        drop(join_set);

        // Phase 4: Result Assembly - map back to input order
        let mut results: Vec<DownloadResult> = Vec::with_capacity(total_count);
        for url in urls {
            let url_str = url.to_string();
            if let Some(result) = result_map.get(&url_str) {
                results.push(result.clone());
            } else {
                // URL missing from results: batch timeout or task panic
                let error = if self.config.batch_timeout.is_some() {
                    DownloadError::Timeout
                } else {
                    DownloadError::ConnectionFailed("task panicked".to_string())
                };
                results.push(DownloadResult {
                    url: url_str,
                    outcome: DownloadOutcome::Failure {
                        error,
                        elapsed: batch_start.elapsed(),
                        retries_attempted: 0,
                    },
                });
            }
        }

        let batch_elapsed = batch_start.elapsed();

        // Phase 5: Summary
        if self.config.write_summary {
            if let Err(e) = write_batch_summary(
                output_dir,
                &self.config,
                &results,
                total_count,
                unique_count,
                batch_elapsed,
            )
            .await
            {
                tracing::warn!(error = %e, "failed to write batch summary");
            }
        }

        let successful = results
            .iter()
            .filter(|r| matches!(r.outcome, DownloadOutcome::Success { .. }))
            .count();
        let failed = results.len() - successful;
        tracing::info!(
            successful = successful,
            failed = failed,
            elapsed_ms = batch_elapsed.as_millis() as u64,
            "batch complete"
        );

        results
    }

    async fn download_one(
        url_str: String,
        first_index: usize,
        config: Arc<Config>,
        pool: Arc<ConnectionPool>,
        global_sem: Arc<Semaphore>,
        host_sems: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
        output_dir: std::path::PathBuf,
    ) -> (String, DownloadResult) {
        let download_start = Instant::now();

        // Parse URL
        let uri: Uri = match url_str.parse() {
            Ok(u) => u,
            Err(e) => {
                return (
                    url_str.clone(),
                    DownloadResult {
                        url: url_str,
                        outcome: DownloadOutcome::Failure {
                            error: DownloadError::ConnectionFailed(format!("invalid URL: {e}")),
                            elapsed: download_start.elapsed(),
                            retries_attempted: 0,
                        },
                    },
                );
            }
        };

        let host = match uri.host() {
            Some(h) => h.to_string(),
            None => {
                return (
                    url_str.clone(),
                    DownloadResult {
                        url: url_str,
                        outcome: DownloadOutcome::Failure {
                            error: DownloadError::ConnectionFailed("URL missing host".to_string()),
                            elapsed: download_start.elapsed(),
                            retries_attempted: 0,
                        },
                    },
                );
            }
        };

        let port = uri
            .port_u16()
            .unwrap_or(if uri.scheme_str() == Some("http") {
                80
            } else {
                443
            });

        // Acquire global semaphore
        let _global_permit = global_sem.acquire().await.unwrap();

        // Acquire per-host semaphore (lazily created)
        let host_sem = {
            let mut sems = host_sems.lock().await;
            sems.entry(host.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(config.max_concurrent_per_host)))
                .clone()
        };
        let _host_permit = host_sem.acquire().await.unwrap();

        let req_config = RequestConfig {
            user_agent: config.user_agent.clone(),
            request_timeout: config.request_timeout,
            max_redirects: config.max_redirects,
        };

        // Retry-wrapped download (connection acquired inside closure)
        let (transport_result, retries_attempted) =
            with_retry(config.max_retries, config.retry_base_delay, || {
                let pool = pool.clone();
                let host = host.clone();
                let uri = uri.clone();
                let req_config = req_config.clone();
                async move {
                    let handle = pool.acquire(&host, port).await?;
                    let conn_id = handle.connection_id;
                    let mut sender: Sender = handle.sender.into();
                    let result = execute_request(&mut sender, &uri, &host, &req_config).await;

                    let usable = matches!(&result, Ok(_) | Err(DownloadError::HttpStatus { .. }));
                    finish_connection(&pool, &host, port, conn_id, sender, usable).await;

                    result
                }
            })
            .await;

        // Handle transport result (including cross-host redirects)
        let response_data = match transport_result {
            Ok(TransportResult::Response(data)) => data,
            Ok(TransportResult::Redirect(redir)) => {
                match Self::follow_cross_host_redirect(&pool, &config, redir).await {
                    Ok(data) => data,
                    Err(e) => {
                        tracing::error!(error = %e, "download failed");
                        return (
                            url_str.clone(),
                            DownloadResult {
                                url: url_str,
                                outcome: DownloadOutcome::Failure {
                                    error: e,
                                    elapsed: download_start.elapsed(),
                                    retries_attempted,
                                },
                            },
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "download failed");
                return (
                    url_str.clone(),
                    DownloadResult {
                        url: url_str,
                        outcome: DownloadOutcome::Failure {
                            error: e,
                            elapsed: download_start.elapsed(),
                            retries_attempted,
                        },
                    },
                );
            }
        };

        // Validate response
        if let Err(e) = validate_response(&response_data.bytes, &response_data.headers) {
            tracing::error!(error = %e, "download failed");
            return (
                url_str.clone(),
                DownloadResult {
                    url: url_str,
                    outcome: DownloadOutcome::Failure {
                        error: e,
                        elapsed: download_start.elapsed(),
                        retries_attempted,
                    },
                },
            );
        }

        let bytes = &response_data.bytes;
        let size_bytes = bytes.len() as u64;

        // Content hash (only when needed)
        let need_hash = matches!(config.naming_strategy, NamingStrategy::ContentHash)
            || config.write_metadata
            || config.write_summary;
        let content_hash = if need_hash {
            let hash = Sha256::digest(bytes);
            Some(
                hash.iter()
                    .take(16) // Truncate to 32 hex chars (128 bits) for brevity
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            )
        } else {
            None
        };

        // Generate filename and write file
        let filename = generate_filename(&url_str, first_index, bytes, &config.naming_strategy);
        let file_path = output_dir.join(&filename);

        if let Err(e) = write_image(bytes, &file_path).await {
            tracing::error!(error = %e, "download failed");
            return (
                url_str.clone(),
                DownloadResult {
                    url: url_str,
                    outcome: DownloadOutcome::Failure {
                        error: e,
                        elapsed: download_start.elapsed(),
                        retries_attempted,
                    },
                },
            );
        }

        let elapsed = download_start.elapsed();
        tracing::info!(path = %file_path.display(), size = size_bytes, "download complete");

        // Metadata sidecar (optional)
        if config.write_metadata {
            let headers_map: HashMap<String, String> = response_data
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let metadata = FileMetadata {
                url: url_str.clone(),
                filename: filename.clone(),
                size_bytes,
                content_hash: content_hash.clone(),
                content_type: response_data
                    .headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string()),
                elapsed_ms: elapsed.as_secs_f64() * 1000.0,
                downloaded_at: chrono::Utc::now().to_rfc3339(),
                headers: headers_map,
            };
            if let Err(e) = write_metadata_sidecar(&file_path, &metadata).await {
                tracing::warn!(error = %e, "failed to write metadata sidecar");
            }
        }

        (
            url_str.clone(),
            DownloadResult {
                url: url_str,
                outcome: DownloadOutcome::Success {
                    path: file_path,
                    size_bytes,
                    content_hash,
                    elapsed,
                },
            },
        )
    }

    /// Follow a cross-host redirect with its own retry loop.
    async fn follow_cross_host_redirect(
        pool: &Arc<ConnectionPool>,
        config: &Arc<Config>,
        redir: crate::transport::CrossHostRedirect,
    ) -> Result<crate::transport::ResponseData, DownloadError> {
        let new_host = redir
            .new_uri
            .host()
            .ok_or_else(|| {
                DownloadError::TooManyRedirects("redirect to URL without host".to_string())
            })?
            .to_string();
        let new_port =
            redir
                .new_uri
                .port_u16()
                .unwrap_or(if redir.new_uri.scheme_str() == Some("http") {
                    80
                } else {
                    443
                });
        let redir_req_config = RequestConfig {
            user_agent: config.user_agent.clone(),
            request_timeout: config.request_timeout,
            max_redirects: redir.redirects_remaining,
        };
        let redir_uri = redir.new_uri;

        let (result, _) = with_retry(config.max_retries, config.retry_base_delay, || {
            let pool = pool.clone();
            let new_host = new_host.clone();
            let redir_uri = redir_uri.clone();
            let redir_req_config = redir_req_config.clone();
            async move {
                let handle = pool.acquire(&new_host, new_port).await?;
                let conn_id = handle.connection_id;
                let mut sender: Sender = handle.sender.into();
                let result =
                    execute_request(&mut sender, &redir_uri, &new_host, &redir_req_config).await;

                let usable = matches!(&result, Ok(_) | Err(DownloadError::HttpStatus { .. }));
                finish_connection(&pool, &new_host, new_port, conn_id, sender, usable).await;

                result
            }
        })
        .await;

        match result? {
            TransportResult::Response(data) => Ok(data),
            TransportResult::Redirect(_) => Err(DownloadError::TooManyRedirects(
                "too many cross-host redirects".to_string(),
            )),
        }
    }
}

/// Manage connection lifecycle after a request completes.
/// Returns usable connections to the pool; removes dead ones.
async fn finish_connection(
    pool: &ConnectionPool,
    host: &str,
    port: u16,
    conn_id: usize,
    sender: Sender,
    result_is_usable: bool,
) {
    if result_is_usable {
        match sender {
            Sender::H1(h1) => pool.return_h1_connection(host, port, conn_id, h1).await,
            Sender::H2(_) => pool.notify_h2_complete(host, port).await,
        }
    } else {
        match sender {
            Sender::H1(_) => pool.remove_dead_connection(host, port, conn_id).await,
            Sender::H2(_) => pool.notify_h2_complete(host, port).await,
        }
    }
}

impl Downloader {
    /// Create a Downloader that uses plain HTTP (no TLS). For testing only.
    #[doc(hidden)]
    pub fn new_plain(config: Config) -> Self {
        let tls_config = build_tls_config();
        let config = Arc::new(config);
        let dns_cache = Arc::new(DnsCache::new(config.dns_cache_ttl));
        let pool = Arc::new(ConnectionPool::new_plain(
            config.clone(),
            dns_cache.clone(),
            tls_config.clone(),
        ));
        Downloader { pool, config }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn plain_downloader(config: Config) -> Downloader {
        Downloader::new_plain(config)
    }

    #[test]
    fn new_creates_valid_instance() {
        let _dl = Downloader::new(Config::default());
    }

    #[tokio::test]
    async fn empty_urls_returns_empty_vec() {
        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let results = dl.download_batch(&[], dir.path()).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn creates_output_directory_if_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/image.jpg"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]),
            )
            .mount(&server)
            .await;

        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("nested").join("subdir");
        let url = format!("{}/image.jpg", server.uri());
        let results = dl.download_batch(&[&url], &sub).await;

        assert_eq!(results.len(), 1);
        assert!(sub.exists());
        assert!(matches!(
            results[0].outcome,
            DownloadOutcome::Success { .. }
        ));
    }

    #[tokio::test]
    async fn downloads_single_url_to_disk() {
        let server = MockServer::start().await;
        let body = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46];
        Mock::given(method("GET"))
            .and(path("/photo.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let url = format!("{}/photo.jpg", server.uri());
        let results = dl.download_batch(&[&url], dir.path()).await;

        assert_eq!(results.len(), 1);
        match &results[0].outcome {
            DownloadOutcome::Success {
                path, size_bytes, ..
            } => {
                assert_eq!(*size_bytes, body.len() as u64);
                assert!(path.exists());
                let content = tokio::fs::read(path).await.unwrap();
                assert_eq!(content, body);
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn downloads_multiple_urls() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"aaa".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bbb".to_vec()))
            .mount(&server)
            .await;

        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let url_a = format!("{}/a.jpg", server.uri());
        let url_b = format!("{}/b.jpg", server.uri());
        let results = dl.download_batch(&[&url_a, &url_b], dir.path()).await;

        assert_eq!(results.len(), 2);
        assert!(matches!(
            results[0].outcome,
            DownloadOutcome::Success { .. }
        ));
        assert!(matches!(
            results[1].outcome,
            DownloadOutcome::Success { .. }
        ));
    }

    #[tokio::test]
    async fn returns_results_in_input_order() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow.jpg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"slow".to_vec())
                    .set_delay(Duration::from_millis(200)),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/fast.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fast".to_vec()))
            .mount(&server)
            .await;

        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let url_slow = format!("{}/slow.jpg", server.uri());
        let url_fast = format!("{}/fast.jpg", server.uri());
        let results = dl.download_batch(&[&url_slow, &url_fast], dir.path()).await;

        assert_eq!(results.len(), 2);
        assert!(results[0].url.contains("slow.jpg"));
        assert!(results[1].url.contains("fast.jpg"));
    }

    #[tokio::test]
    async fn deduplicates_urls() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dup.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let url = format!("{}/dup.jpg", server.uri());
        let results = dl.download_batch(&[&url, &url, &url], dir.path()).await;

        assert_eq!(results.len(), 3);
        for r in &results {
            assert!(
                matches!(r.outcome, DownloadOutcome::Success { .. }),
                "expected Success, got {:?}",
                r.outcome
            );
        }
        // All should reference the same file path
        let paths: Vec<_> = results
            .iter()
            .map(|r| match &r.outcome {
                DownloadOutcome::Success { path, .. } => path.clone(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(paths[0], paths[1]);
        assert_eq!(paths[1], paths[2]);
    }

    #[tokio::test]
    async fn dedup_sequential_naming_uses_first_index() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/a.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"aaa".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/b.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bbb".to_vec()))
            .mount(&server)
            .await;

        let config = Config {
            naming_strategy: NamingStrategy::Sequential,
            ..Default::default()
        };
        let dl = plain_downloader(config);
        let dir = TempDir::new().unwrap();
        let url_a = format!("{}/a.jpg", server.uri());
        let url_b = format!("{}/b.jpg", server.uri());
        let results = dl
            .download_batch(&[&url_a, &url_b, &url_a], dir.path())
            .await;

        assert_eq!(results.len(), 3);
        // url_a gets index 0 -> 000.jpg
        match &results[0].outcome {
            DownloadOutcome::Success { path, .. } => {
                let name = path.file_name().unwrap().to_str().unwrap();
                assert!(name.starts_with("000"), "expected 000.*, got {name}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
        // url_b gets index 1 -> 001.jpg
        match &results[1].outcome {
            DownloadOutcome::Success { path, .. } => {
                let name = path.file_name().unwrap().to_str().unwrap();
                assert!(name.starts_with("001"), "expected 001.*, got {name}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
        // Duplicate url_a has same path as first occurrence
        match (&results[0].outcome, &results[2].outcome) {
            (
                DownloadOutcome::Success { path: p0, .. },
                DownloadOutcome::Success { path: p2, .. },
            ) => assert_eq!(p0, p2),
            _ => panic!("expected both Success"),
        }
    }

    #[tokio::test]
    async fn isolates_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/good.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"good".to_vec()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/bad.jpg"))
            .respond_with(ResponseTemplate::new(404).set_body_bytes(b"not found".to_vec()))
            .mount(&server)
            .await;

        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let url_good = format!("{}/good.jpg", server.uri());
        let url_bad = format!("{}/bad.jpg", server.uri());
        let results = dl.download_batch(&[&url_good, &url_bad], dir.path()).await;

        assert_eq!(results.len(), 2);
        assert!(matches!(
            results[0].outcome,
            DownloadOutcome::Success { .. }
        ));
        assert!(matches!(
            results[1].outcome,
            DownloadOutcome::Failure { .. }
        ));
    }

    #[tokio::test]
    async fn respects_batch_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow.jpg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"slow".to_vec())
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&server)
            .await;

        let config = Config {
            batch_timeout: Some(Duration::from_millis(200)),
            max_retries: 0,
            ..Default::default()
        };
        let dl = plain_downloader(config);
        let dir = TempDir::new().unwrap();
        let url = format!("{}/slow.jpg", server.uri());

        let start = Instant::now();
        let results = dl.download_batch(&[&url], dir.path()).await;
        let elapsed = start.elapsed();

        assert_eq!(results.len(), 1);
        match &results[0].outcome {
            DownloadOutcome::Failure { error, .. } => {
                assert!(
                    matches!(error, DownloadError::Timeout),
                    "expected Timeout error, got {error:?}"
                );
            }
            other => panic!("expected Failure, got {other:?}"),
        }
        assert!(
            elapsed < Duration::from_secs(2),
            "batch should have timed out quickly, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn handles_invalid_urls() {
        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let results = dl
            .download_batch(&["not-a-url", "://bad"], dir.path())
            .await;

        assert_eq!(results.len(), 2);
        assert!(matches!(
            results[0].outcome,
            DownloadOutcome::Failure { .. }
        ));
        assert!(matches!(
            results[1].outcome,
            DownloadOutcome::Failure { .. }
        ));
    }

    #[tokio::test]
    async fn handles_all_failing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_bytes(b"error".to_vec()))
            .mount(&server)
            .await;

        let config = Config {
            max_retries: 0,
            ..Default::default()
        };
        let dl = plain_downloader(config);
        let dir = TempDir::new().unwrap();
        let url_a = format!("{}/a.jpg", server.uri());
        let url_b = format!("{}/b.jpg", server.uri());
        let results = dl.download_batch(&[&url_a, &url_b], dir.path()).await;

        assert_eq!(results.len(), 2);
        for r in &results {
            assert!(
                matches!(r.outcome, DownloadOutcome::Failure { .. }),
                "expected Failure, got {:?}",
                r.outcome
            );
        }
    }

    #[tokio::test]
    async fn concurrent_downloads_faster_than_sequential() {
        let server = MockServer::start().await;
        for i in 0..4 {
            Mock::given(method("GET"))
                .and(path(format!("/{i}.jpg")))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_bytes(format!("img{i}").into_bytes())
                        .set_delay(Duration::from_millis(100)),
                )
                .mount(&server)
                .await;
        }

        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let urls: Vec<String> = (0..4)
            .map(|i| format!("{}/{i}.jpg", server.uri()))
            .collect();
        let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();

        let start = Instant::now();
        let results = dl.download_batch(&url_refs, dir.path()).await;
        let elapsed = start.elapsed();

        assert_eq!(results.len(), 4);
        for r in &results {
            assert!(matches!(r.outcome, DownloadOutcome::Success { .. }));
        }
        // Sequential = 400ms+. Concurrent should be well under that.
        assert!(
            elapsed < Duration::from_millis(500),
            "expected concurrent execution, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn connection_reuse_across_batches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/reuse.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"reuse".to_vec()))
            .expect(2)
            .mount(&server)
            .await;

        let dl = plain_downloader(Config::default());
        let dir = TempDir::new().unwrap();
        let url = format!("{}/reuse.jpg", server.uri());

        let r1 = dl.download_batch(&[&url], dir.path()).await;
        assert!(matches!(r1[0].outcome, DownloadOutcome::Success { .. }));

        let r2 = dl.download_batch(&[&url], dir.path()).await;
        assert!(matches!(r2[0].outcome, DownloadOutcome::Success { .. }));
    }
}
