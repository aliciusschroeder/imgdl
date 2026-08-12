"""Downloading a large batch, with the settings that actually matter at scale.

The intended workload: tens of thousands of small images from ~10 hosts, from a
long-lived worker process.
"""

import sys
from pathlib import Path

import imgdl

# Configure ONCE, on the first call in the process. The Tokio runtime and the
# connection pool are process-global: a later call with different settings logs
# a warning and reuses the originals.
CONFIG = imgdl.Config(
    # Total in-flight requests. Past a few hundred you are usually bound by the
    # origin, not by us.
    max_concurrent=512,
    # The per-host cap is what keeps you from being rate-limited into oblivion.
    max_concurrent_per_host=64,
    # HTTP/2 multiplexes, so one socket per host is normally right. Raise this
    # only for origins that fall back to HTTP/1.1.
    connections_per_host=1,
    max_retries=3,
    retry_base_delay_ms=200,
    connect_timeout_secs=5.0,
    request_timeout_secs=20.0,
    # A whole-batch deadline; results for unfinished URLs come back as failures
    # rather than hanging the worker.
    batch_timeout_secs=600.0,
    dns_cache_ttl_secs=600,
    naming_strategy="content_hash",  # dedupes identical images for free
    write_summary=True,
    runtime_threads=0,  # one worker per core
)


def main(url_file: str, output_dir: str) -> int:
    urls = [line.strip() for line in Path(url_file).read_text().splitlines() if line.strip()]
    Path(output_dir).mkdir(parents=True, exist_ok=True)

    results = imgdl.download_images(urls, output_dir=output_dir, config=CONFIG)

    failed = [r for r in results if not r.success]
    total_bytes = sum(r.size_bytes or 0 for r in results)
    print(f"{len(results) - len(failed)}/{len(results)} ok, {total_bytes / 1e6:.1f} MB")

    # Group failures by cause — far more useful than a wall of individual errors.
    causes: dict[str, int] = {}
    for r in failed:
        causes[(r.error or "unknown").split(":")[0]] = (
            causes.get((r.error or "unknown").split(":")[0], 0) + 1
        )
    for cause, n in sorted(causes.items(), key=lambda kv: -kv[1]):
        print(f"  {n:>6}  {cause}")

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1], sys.argv[2]))
