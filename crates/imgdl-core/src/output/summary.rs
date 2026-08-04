use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::config::Config;
use crate::types::{DownloadError, DownloadOutcome, DownloadResult};

/// Summary of an entire batch download operation.
#[derive(Serialize)]
pub(crate) struct BatchSummary {
    pub config: Config,
    pub total_urls: usize,
    pub unique_urls: usize,
    pub successful: usize,
    pub failed: usize,
    pub total_bytes: u64,
    pub total_elapsed_ms: f64,
    pub per_host_stats: HashMap<String, HostStats>,
    pub results: Vec<ResultSummary>,
}

/// Per-host aggregate statistics.
#[derive(Serialize)]
pub(crate) struct HostStats {
    pub requests: usize,
    pub successful: usize,
    pub failed: usize,
    pub total_bytes: u64,
    pub avg_elapsed_ms: f64,
}

/// Compact per-URL result for the summary.
#[derive(Serialize)]
pub(crate) struct ResultSummary {
    pub url: String,
    pub success: bool,
    pub size_bytes: Option<u64>,
    pub elapsed_ms: f64,
    pub error: Option<String>,
}

/// Extract host from a URL string, falling back to "unknown".
fn extract_host(url: &str) -> String {
    url.split("//")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .and_then(|s| s.split(':').next())
        .unwrap_or("unknown")
        .to_string()
}

/// Write a batch summary JSON file to the output directory.
pub(crate) async fn write_batch_summary(
    output_dir: &Path,
    config: &Config,
    results: &[DownloadResult],
    total_urls: usize,
    unique_urls: usize,
    batch_elapsed: Duration,
) -> Result<(), DownloadError> {
    let mut successful = 0usize;
    let mut failed = 0usize;
    let mut total_bytes = 0u64;
    let mut host_data: HashMap<String, Vec<(bool, u64, f64)>> = HashMap::new();
    let mut result_summaries = Vec::with_capacity(results.len());

    for r in results {
        let host = extract_host(&r.url);
        match &r.outcome {
            DownloadOutcome::Success {
                size_bytes,
                elapsed,
                ..
            } => {
                successful += 1;
                total_bytes += size_bytes;
                let ms = elapsed.as_secs_f64() * 1000.0;
                host_data
                    .entry(host)
                    .or_default()
                    .push((true, *size_bytes, ms));
                result_summaries.push(ResultSummary {
                    url: r.url.clone(),
                    success: true,
                    size_bytes: Some(*size_bytes),
                    elapsed_ms: ms,
                    error: None,
                });
            }
            DownloadOutcome::Failure { error, elapsed, .. } => {
                failed += 1;
                let ms = elapsed.as_secs_f64() * 1000.0;
                host_data.entry(host).or_default().push((false, 0, ms));
                result_summaries.push(ResultSummary {
                    url: r.url.clone(),
                    success: false,
                    size_bytes: None,
                    elapsed_ms: ms,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    let per_host_stats: HashMap<String, HostStats> = host_data
        .into_iter()
        .map(|(host, entries)| {
            let requests = entries.len();
            let host_successful = entries.iter().filter(|(s, _, _)| *s).count();
            let host_failed = requests - host_successful;
            let host_bytes: u64 = entries.iter().map(|(_, b, _)| b).sum();
            let total_ms: f64 = entries.iter().map(|(_, _, ms)| ms).sum();
            let avg_ms = if requests > 0 {
                total_ms / requests as f64
            } else {
                0.0
            };
            (
                host,
                HostStats {
                    requests,
                    successful: host_successful,
                    failed: host_failed,
                    total_bytes: host_bytes,
                    avg_elapsed_ms: avg_ms,
                },
            )
        })
        .collect();

    let summary = BatchSummary {
        config: config.clone(),
        total_urls,
        unique_urls,
        successful,
        failed,
        total_bytes,
        total_elapsed_ms: batch_elapsed.as_secs_f64() * 1000.0,
        per_host_stats,
        results: result_summaries,
    };

    let json = serde_json::to_string_pretty(&summary)
        .map_err(|e| DownloadError::WriteError(e.to_string()))?;

    let summary_path = output_dir.join("summary.json");
    tokio::fs::write(&summary_path, json)
        .await
        .map_err(|e| DownloadError::WriteError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_results() -> Vec<DownloadResult> {
        vec![
            DownloadResult {
                url: "https://cdn.example.com/img1.jpg".to_string(),
                outcome: DownloadOutcome::Success {
                    path: PathBuf::from("/tmp/img1.jpg"),
                    size_bytes: 1024,
                    content_hash: None,
                    elapsed: Duration::from_millis(100),
                },
            },
            DownloadResult {
                url: "https://cdn.example.com/img2.jpg".to_string(),
                outcome: DownloadOutcome::Success {
                    path: PathBuf::from("/tmp/img2.jpg"),
                    size_bytes: 2048,
                    content_hash: None,
                    elapsed: Duration::from_millis(200),
                },
            },
            DownloadResult {
                url: "https://other.example.com/img3.jpg".to_string(),
                outcome: DownloadOutcome::Failure {
                    error: DownloadError::Timeout,
                    elapsed: Duration::from_millis(300),
                    retries_attempted: 3,
                },
            },
        ]
    }

    #[tokio::test]
    async fn summary_contains_expected_fields() {
        let dir = TempDir::new().unwrap();
        let config = Config::default();
        let results = make_results();
        write_batch_summary(dir.path(), &config, &results, 3, 3, Duration::from_secs(1))
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(dir.path().join("summary.json"))
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(value.get("config").is_some());
        assert_eq!(value["total_urls"], 3);
        assert_eq!(value["unique_urls"], 3);
        assert_eq!(value["successful"], 2);
        assert_eq!(value["failed"], 1);
    }

    #[tokio::test]
    async fn summary_per_host_stats_correct() {
        let dir = TempDir::new().unwrap();
        let config = Config::default();
        let results = make_results();
        write_batch_summary(dir.path(), &config, &results, 3, 3, Duration::from_secs(1))
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(dir.path().join("summary.json"))
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        let stats = value["per_host_stats"].as_object().unwrap();

        let cdn = &stats["cdn.example.com"];
        assert_eq!(cdn["requests"], 2);
        assert_eq!(cdn["successful"], 2);
        assert_eq!(cdn["failed"], 0);

        let other = &stats["other.example.com"];
        assert_eq!(other["requests"], 1);
        assert_eq!(other["successful"], 0);
        assert_eq!(other["failed"], 1);
    }

    #[tokio::test]
    async fn summary_total_bytes_correct() {
        let dir = TempDir::new().unwrap();
        let config = Config::default();
        let results = make_results();
        write_batch_summary(dir.path(), &config, &results, 3, 3, Duration::from_secs(1))
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(dir.path().join("summary.json"))
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["total_bytes"], 1024 + 2048);
    }

    #[tokio::test]
    async fn summary_written_to_correct_path() {
        let dir = TempDir::new().unwrap();
        let config = Config::default();
        let results = make_results();
        write_batch_summary(dir.path(), &config, &results, 3, 3, Duration::from_secs(1))
            .await
            .unwrap();

        assert!(dir.path().join("summary.json").exists());
    }

    #[tokio::test]
    async fn summary_is_valid_json() {
        let dir = TempDir::new().unwrap();
        let config = Config::default();
        let results = make_results();
        write_batch_summary(dir.path(), &config, &results, 3, 3, Duration::from_secs(1))
            .await
            .unwrap();

        let content = tokio::fs::read_to_string(dir.path().join("summary.json"))
            .await
            .unwrap();
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
        assert!(parsed.is_ok());
    }
}
