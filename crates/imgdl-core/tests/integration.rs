mod helpers;

use helpers::*;
use imgdl_core::{Config, DownloadError, DownloadOutcome, Downloader, NamingStrategy};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_downloader(config: Config) -> Downloader {
    Downloader::new_plain(config)
}

// ── Full batch download ─────────────────────────────────────────────────

#[tokio::test]
async fn full_batch_download_wiremock() {
    init_tracing();

    let server = MockServer::start().await;
    let jpeg = fake_jpeg();
    let png = fake_png();
    let unknown = fake_unknown();

    mount_image_mock(&server, "/photo.jpg", &jpeg).await;
    mount_image_mock(&server, "/logo.png", &png).await;
    mount_image_mock(&server, "/data.bin", &unknown).await;

    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();

    let url1 = format!("{}/photo.jpg", server.uri());
    let url2 = format!("{}/logo.png", server.uri());
    let url3 = format!("{}/data.bin", server.uri());
    let results = dl.download_batch(&[&url1, &url2, &url3], dir.path()).await;

    assert_eq!(results.len(), 3);
    for (i, r) in results.iter().enumerate() {
        match &r.outcome {
            DownloadOutcome::Success {
                path, size_bytes, ..
            } => {
                assert!(path.exists(), "file {i} should exist on disk");
                let expected_len = match i {
                    0 => jpeg.len(),
                    1 => png.len(),
                    2 => unknown.len(),
                    _ => unreachable!(),
                };
                assert_eq!(*size_bytes, expected_len as u64);
                let content = tokio::fs::read(path).await.unwrap();
                let expected = match i {
                    0 => &jpeg,
                    1 => &png,
                    2 => &unknown,
                    _ => unreachable!(),
                };
                assert_eq!(content, *expected);
            }
            other => panic!("expected Success for URL {i}, got {other:?}"),
        }
    }
}

// ── Concurrency proof (timing-based) ────────────────────────────────────

#[tokio::test]
async fn concurrent_downloads_timing_proof() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();

    // Mount 10 mocks each with 50ms delay
    for i in 0..10 {
        mount_delayed_mock(
            &server,
            &format!("/{i}.jpg"),
            &body,
            Duration::from_millis(50),
        )
        .await;
    }

    // Allow multiple concurrent HTTP/1.1 connections to prove concurrency
    let config = Config {
        connections_per_host: 10,
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();

    let urls: Vec<String> = (0..10)
        .map(|i| format!("{}/{i}.jpg", server.uri()))
        .collect();
    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();

    let start = Instant::now();
    let results = dl.download_batch(&url_refs, dir.path()).await;
    let elapsed = start.elapsed();

    assert_eq!(results.len(), 10);
    for r in &results {
        assert!(
            matches!(r.outcome, DownloadOutcome::Success { .. }),
            "expected success, got {:?}",
            r.outcome
        );
    }

    // Sequential would take 500ms+. Concurrent should be well under.
    assert!(
        elapsed < Duration::from_millis(400),
        "expected concurrent execution, took {elapsed:?}"
    );
}

// ── HTTP/1.1 fallback ───────────────────────────────────────────────────

#[tokio::test]
async fn http1_1_fallback() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();
    mount_image_mock(&server, "/fallback.jpg", &body).await;

    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();
    let url = format!("{}/fallback.jpg", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;

    assert_eq!(results.len(), 1);
    match &results[0].outcome {
        DownloadOutcome::Success { path, .. } => {
            assert!(path.exists());
            let content = tokio::fs::read(path).await.unwrap();
            assert_eq!(content, body);
        }
        other => panic!("expected Success, got {other:?}"),
    }
}

// ── Retry 429 then 200 ─────────────────────────────────────────────────

#[tokio::test]
async fn retry_429_then_200() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();

    // First request: 429
    Mock::given(method("GET"))
        .and(path("/throttled.jpg"))
        .respond_with(ResponseTemplate::new(429).set_body_bytes(b"rate limited".to_vec()))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    // Second request: 200
    Mock::given(method("GET"))
        .and(path("/throttled.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .expect(1)
        .mount(&server)
        .await;

    let config = Config {
        max_retries: 3,
        retry_base_delay: Duration::from_millis(10),
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();
    let url = format!("{}/throttled.jpg", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;

    assert_eq!(results.len(), 1);
    match &results[0].outcome {
        DownloadOutcome::Success { path, .. } => {
            assert!(path.exists());
            let content = tokio::fs::read(path).await.unwrap();
            assert_eq!(content, body);
        }
        other => panic!("expected Success after retry, got {other:?}"),
    }

    // Verify server received exactly 2 requests
    let received = server.received_requests().await.unwrap();
    let matching = received
        .iter()
        .filter(|r| r.url.path() == "/throttled.jpg")
        .count();
    assert_eq!(matching, 2, "expected 2 requests (1 retry)");
}

// ── Retry 503 then 200 ─────────────────────────────────────────────────

#[tokio::test]
async fn retry_503_then_200() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();

    Mock::given(method("GET"))
        .and(path("/unavail.jpg"))
        .respond_with(ResponseTemplate::new(503).set_body_bytes(b"service unavailable".to_vec()))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/unavail.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .expect(1)
        .mount(&server)
        .await;

    let config = Config {
        max_retries: 3,
        retry_base_delay: Duration::from_millis(10),
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();
    let url = format!("{}/unavail.jpg", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;

    assert_eq!(results.len(), 1);
    assert!(
        matches!(results[0].outcome, DownloadOutcome::Success { .. }),
        "expected Success after retry"
    );

    // Verify server received exactly 2 requests (1 retry)
    let received = server.received_requests().await.unwrap();
    let matching = received
        .iter()
        .filter(|r| r.url.path() == "/unavail.jpg")
        .count();
    assert_eq!(matching, 2, "expected 2 requests (1 retry)");
}

// ── Non-retryable 404 ──────────────────────────────────────────────────

#[tokio::test]
async fn non_retryable_404() {
    init_tracing();

    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/missing.jpg"))
        .respond_with(ResponseTemplate::new(404).set_body_bytes(b"not found".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();
    let url = format!("{}/missing.jpg", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;

    assert_eq!(results.len(), 1);
    match &results[0].outcome {
        DownloadOutcome::Failure { error, .. } => match error {
            DownloadError::HttpStatus { code: 404, .. } => {}
            other => panic!("expected HttpStatus 404, got {other:?}"),
        },
        other => panic!("expected Failure, got {other:?}"),
    }

    // Only 1 request (no retries)
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);

    // No file written
    let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(entries.is_empty(), "no files should be written for 404");
}

// ── Redirect 302 then 200 ──────────────────────────────────────────────

#[tokio::test]
async fn redirect_302_then_200() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();

    mount_redirect_mock(&server, "/redirect", 302, "/final").await;
    mount_image_mock(&server, "/final", &body).await;

    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();
    let url = format!("{}/redirect", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;

    assert_eq!(results.len(), 1);
    match &results[0].outcome {
        DownloadOutcome::Success { path, .. } => {
            let content = tokio::fs::read(path).await.unwrap();
            assert_eq!(content, body);
        }
        other => panic!("expected Success after redirect, got {other:?}"),
    }

    // Server should have received requests to both paths
    let received = server.received_requests().await.unwrap();
    let paths: Vec<_> = received.iter().map(|r| r.url.path().to_string()).collect();
    assert!(paths.contains(&"/redirect".to_string()));
    assert!(paths.contains(&"/final".to_string()));
}

// ── Redirect chain (multiple hops) ─────────────────────────────────────

#[tokio::test]
async fn redirect_chain_multiple_hops() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();

    mount_redirect_mock(&server, "/a", 302, "/b").await;
    mount_redirect_mock(&server, "/b", 302, "/c").await;
    mount_redirect_mock(&server, "/c", 302, "/final").await;
    mount_image_mock(&server, "/final", &body).await;

    // With max_redirects=5, the chain should succeed
    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();
    let url = format!("{}/a", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;
    assert_eq!(results.len(), 1);
    assert!(
        matches!(results[0].outcome, DownloadOutcome::Success { .. }),
        "3-hop redirect should succeed with max_redirects=5"
    );

    // Now test with max_redirects=2 -- should fail
    let config = Config {
        max_redirects: 2,
        ..Default::default()
    };
    let dl2 = test_downloader(config);
    let dir2 = TempDir::new().unwrap();

    let results2 = dl2.download_batch(&[&url], dir2.path()).await;
    assert_eq!(results2.len(), 1);
    match &results2[0].outcome {
        DownloadOutcome::Failure { error, .. } => {
            assert!(
                matches!(error, DownloadError::TooManyRedirects(_)),
                "expected TooManyRedirects, got {error:?}"
            );
        }
        other => panic!("expected Failure, got {other:?}"),
    }
}

// ── Naming strategies ───────────────────────────────────────────────────

#[tokio::test]
async fn naming_strategy_url_based() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();
    mount_image_mock(&server, "/photo.jpg", &body).await;

    let dl = test_downloader(Config::default()); // default is UrlBased
    let dir = TempDir::new().unwrap();
    let url = format!("{}/photo.jpg", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;
    match &results[0].outcome {
        DownloadOutcome::Success { path, .. } => {
            let name = path.file_name().unwrap().to_str().unwrap();
            assert_eq!(name, "photo.jpg");
        }
        other => panic!("expected Success, got {other:?}"),
    }
}

#[tokio::test]
async fn naming_strategy_sequential() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();
    mount_image_mock(&server, "/image.jpg", &body).await;

    let config = Config {
        naming_strategy: NamingStrategy::Sequential,
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();
    let url = format!("{}/image.jpg", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;
    match &results[0].outcome {
        DownloadOutcome::Success { path, .. } => {
            let name = path.file_name().unwrap().to_str().unwrap();
            assert_eq!(name, "000.jpg");
        }
        other => panic!("expected Success, got {other:?}"),
    }
}

#[tokio::test]
async fn naming_strategy_content_hash() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();
    mount_image_mock(&server, "/hash.jpg", &body).await;

    let config = Config {
        naming_strategy: NamingStrategy::ContentHash,
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();
    let url = format!("{}/hash.jpg", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;
    match &results[0].outcome {
        DownloadOutcome::Success { path, .. } => {
            let name = path.file_name().unwrap().to_str().unwrap();
            // ContentHash: 16 hex chars + .jpg
            assert!(
                name.ends_with(".jpg"),
                "expected .jpg extension, got {name}"
            );
            let stem = name.strip_suffix(".jpg").unwrap();
            assert_eq!(stem.len(), 32, "expected 32-char hex hash, got {stem}");
            assert!(
                stem.chars().all(|c| c.is_ascii_hexdigit()),
                "expected hex hash, got {stem}"
            );
        }
        other => panic!("expected Success, got {other:?}"),
    }
}

#[tokio::test]
async fn naming_strategy_file_header() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_png(); // PNG magic bytes
                           // Serve with .jpg extension but PNG content
    mount_image_mock(&server, "/mislabeled.jpg", &body).await;

    let config = Config {
        naming_strategy: NamingStrategy::FileHeader,
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();
    let url = format!("{}/mislabeled.jpg", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;
    match &results[0].outcome {
        DownloadOutcome::Success { path, .. } => {
            let name = path.file_name().unwrap().to_str().unwrap();
            // FileHeader should detect PNG from magic bytes
            assert!(
                name.ends_with(".png"),
                "FileHeader should detect PNG, got {name}"
            );
        }
        other => panic!("expected Success, got {other:?}"),
    }
}

// ── Metadata sidecars ───────────────────────────────────────────────────

#[tokio::test]
async fn metadata_sidecars_written() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();
    mount_image_mock(&server, "/meta.jpg", &body).await;

    let config = Config {
        write_metadata: true,
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();
    let url = format!("{}/meta.jpg", server.uri());

    let results = dl.download_batch(&[&url], dir.path()).await;

    assert_eq!(results.len(), 1);
    match &results[0].outcome {
        DownloadOutcome::Success {
            path, content_hash, ..
        } => {
            // Image file exists
            assert!(path.exists());

            // Sidecar exists
            let sidecar_name = format!("{}.json", path.file_name().unwrap().to_str().unwrap());
            let sidecar_path = path.parent().unwrap().join(sidecar_name);
            assert!(
                sidecar_path.exists(),
                "sidecar should exist at {sidecar_path:?}"
            );

            // Sidecar is valid JSON with expected fields
            let content = tokio::fs::read_to_string(&sidecar_path).await.unwrap();
            let value: serde_json::Value = serde_json::from_str(&content).unwrap();
            assert!(value.get("url").is_some());
            assert!(value.get("filename").is_some());
            assert!(value.get("size_bytes").is_some());
            assert!(value.get("elapsed_ms").is_some());
            assert!(value.get("downloaded_at").is_some());
            assert!(value.get("headers").is_some());

            // content_hash should be populated when metadata is enabled
            assert!(
                content_hash.is_some(),
                "content_hash should be Some when write_metadata=true"
            );
        }
        other => panic!("expected Success, got {other:?}"),
    }
}

// ── Batch summary ───────────────────────────────────────────────────────

#[tokio::test]
async fn batch_summary_written() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();
    mount_image_mock(&server, "/s1.jpg", &body).await;
    mount_image_mock(&server, "/s2.jpg", &body).await;
    mount_status_mock(&server, "/s3.jpg", 500).await;

    let config = Config {
        write_summary: true,
        max_retries: 0,
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();

    let url1 = format!("{}/s1.jpg", server.uri());
    let url2 = format!("{}/s2.jpg", server.uri());
    let url3 = format!("{}/s3.jpg", server.uri());
    let results = dl.download_batch(&[&url1, &url2, &url3], dir.path()).await;

    // Verify results
    let successes = results
        .iter()
        .filter(|r| matches!(r.outcome, DownloadOutcome::Success { .. }))
        .count();
    let failures = results
        .iter()
        .filter(|r| matches!(r.outcome, DownloadOutcome::Failure { .. }))
        .count();
    assert_eq!(successes, 2);
    assert_eq!(failures, 1);

    // Verify summary.json
    let summary_path = dir.path().join("summary.json");
    assert!(summary_path.exists(), "summary.json should exist");

    let content = tokio::fs::read_to_string(&summary_path).await.unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(value["total_urls"], 3);
    assert_eq!(value["successful"], 2);
    assert_eq!(value["failed"], 1);
    assert!(value.get("config").is_some());
    assert!(value.get("per_host_stats").is_some());

    let total_bytes = value["total_bytes"].as_u64().unwrap();
    assert_eq!(total_bytes, (body.len() * 2) as u64);
}

// ── Batch timeout ───────────────────────────────────────────────────────

#[tokio::test]
async fn batch_timeout_cancels_remaining() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();

    // Fast response
    mount_image_mock(&server, "/fast.jpg", &body).await;
    // Slow response (2 seconds)
    mount_delayed_mock(&server, "/slow.jpg", &body, Duration::from_secs(2)).await;

    let config = Config {
        batch_timeout: Some(Duration::from_millis(500)),
        max_retries: 0,
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();

    let url_fast = format!("{}/fast.jpg", server.uri());
    let url_slow = format!("{}/slow.jpg", server.uri());

    let start = Instant::now();
    let results = dl.download_batch(&[&url_fast, &url_slow], dir.path()).await;
    let elapsed = start.elapsed();

    assert_eq!(results.len(), 2);

    // Fast URL should succeed
    assert!(
        matches!(results[0].outcome, DownloadOutcome::Success { .. }),
        "fast URL should succeed, got {:?}",
        results[0].outcome
    );

    // Slow URL should fail with Timeout
    match &results[1].outcome {
        DownloadOutcome::Failure { error, .. } => {
            assert!(
                matches!(error, DownloadError::Timeout),
                "slow URL should timeout, got {error:?}"
            );
        }
        other => panic!("expected Failure for slow URL, got {other:?}"),
    }

    // Total elapsed should be around the batch timeout, not 2 seconds
    assert!(
        elapsed < Duration::from_secs(1),
        "batch should have timed out quickly, took {elapsed:?}"
    );
}

// ── Downloader reuse across batches ─────────────────────────────────────

#[tokio::test]
async fn downloader_reuse_across_batches() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();

    Mock::given(method("GET"))
        .and(path("/reuse.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .expect(2)
        .mount(&server)
        .await;

    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();
    let url = format!("{}/reuse.jpg", server.uri());

    // First batch
    let r1 = dl.download_batch(&[&url], dir.path()).await;
    assert!(matches!(r1[0].outcome, DownloadOutcome::Success { .. }));

    // Second batch with same Downloader (reuses connection)
    let r2 = dl.download_batch(&[&url], dir.path()).await;
    assert!(matches!(r2[0].outcome, DownloadOutcome::Success { .. }));
}

// ── Mixed success and failure ───────────────────────────────────────────

#[tokio::test]
async fn mixed_success_and_failure() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();

    mount_image_mock(&server, "/ok1.jpg", &body).await;
    mount_image_mock(&server, "/ok2.jpg", &body).await;
    mount_image_mock(&server, "/ok3.jpg", &body).await;
    mount_status_mock(&server, "/err500.jpg", 500).await;
    mount_status_mock(&server, "/err404.jpg", 404).await;

    let config = Config {
        max_retries: 0,
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();

    let urls: Vec<String> = [
        "/ok1.jpg",
        "/ok2.jpg",
        "/ok3.jpg",
        "/err500.jpg",
        "/err404.jpg",
    ]
    .iter()
    .map(|p| format!("{}{}", server.uri(), p))
    .collect();
    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();

    let results = dl.download_batch(&url_refs, dir.path()).await;

    assert_eq!(results.len(), 5);

    // Results should be in input order -- verify by URL
    assert!(results[0].url.contains("/ok1.jpg"));
    assert!(results[1].url.contains("/ok2.jpg"));
    assert!(results[2].url.contains("/ok3.jpg"));
    assert!(results[3].url.contains("/err500.jpg"));
    assert!(results[4].url.contains("/err404.jpg"));

    assert!(matches!(
        results[0].outcome,
        DownloadOutcome::Success { .. }
    ));
    assert!(matches!(
        results[1].outcome,
        DownloadOutcome::Success { .. }
    ));
    assert!(matches!(
        results[2].outcome,
        DownloadOutcome::Success { .. }
    ));

    match &results[3].outcome {
        DownloadOutcome::Failure { error, .. } => {
            assert!(matches!(error, DownloadError::HttpStatus { code: 500, .. }));
        }
        other => panic!("expected 500 failure, got {other:?}"),
    }

    match &results[4].outcome {
        DownloadOutcome::Failure { error, .. } => {
            assert!(matches!(error, DownloadError::HttpStatus { code: 404, .. }));
        }
        other => panic!("expected 404 failure, got {other:?}"),
    }

    // Verify successful files exist
    for r in &results[..3] {
        if let DownloadOutcome::Success { path, .. } = &r.outcome {
            assert!(path.exists());
        }
    }
}

// ── URL deduplication ───────────────────────────────────────────────────

#[tokio::test]
async fn url_deduplication_end_to_end() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();

    Mock::given(method("GET"))
        .and(path("/dedup.jpg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .expect(1) // only 1 request despite 3 URLs
        .mount(&server)
        .await;

    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();
    let url = format!("{}/dedup.jpg", server.uri());

    let results = dl.download_batch(&[&url, &url, &url], dir.path()).await;

    assert_eq!(results.len(), 3);

    // All should succeed
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

// ── Empty URL list ──────────────────────────────────────────────────────

#[tokio::test]
async fn empty_url_list() {
    init_tracing();

    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();

    let results = dl.download_batch(&[], dir.path()).await;
    assert!(results.is_empty());
}

// ── Invalid URLs ────────────────────────────────────────────────────────

#[tokio::test]
async fn invalid_urls_no_panic() {
    init_tracing();

    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();

    let results = dl
        .download_batch(&["not-a-url", "://missing-scheme", "http://"], dir.path())
        .await;

    assert_eq!(results.len(), 3);
    for r in &results {
        assert!(
            matches!(r.outcome, DownloadOutcome::Failure { .. }),
            "expected Failure for invalid URL, got {:?}",
            r.outcome
        );
    }
}

// ── All URLs fail ───────────────────────────────────────────────────────

#[tokio::test]
async fn all_urls_fail() {
    init_tracing();

    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500).set_body_bytes(b"error".to_vec()))
        .mount(&server)
        .await;

    let config = Config {
        max_retries: 0,
        ..Default::default()
    };
    let dl = test_downloader(config);
    let dir = TempDir::new().unwrap();

    let urls: Vec<String> = (0..3)
        .map(|i| format!("{}/{i}.jpg", server.uri()))
        .collect();
    let url_refs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();

    let results = dl.download_batch(&url_refs, dir.path()).await;

    assert_eq!(results.len(), 3);
    for r in &results {
        assert!(
            matches!(r.outcome, DownloadOutcome::Failure { .. }),
            "expected Failure, got {:?}",
            r.outcome
        );
    }

    // No files should be written (only directory was created)
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(entries.is_empty(), "no files should be written");
}

// ── Output directory created if missing ─────────────────────────────────

#[tokio::test]
async fn creates_output_directory() {
    init_tracing();

    let server = MockServer::start().await;
    let body = fake_jpeg();
    mount_image_mock(&server, "/deep.jpg", &body).await;

    let dl = test_downloader(Config::default());
    let dir = TempDir::new().unwrap();
    let deep_path = dir.path().join("sub").join("deep").join("path");

    let url = format!("{}/deep.jpg", server.uri());
    let results = dl.download_batch(&[&url], &deep_path).await;

    assert_eq!(results.len(), 1);
    assert!(deep_path.exists(), "directory should have been created");
    match &results[0].outcome {
        DownloadOutcome::Success { path, .. } => {
            assert!(path.exists());
            assert!(path.starts_with(&deep_path));
        }
        other => panic!("expected Success, got {other:?}"),
    }
}
