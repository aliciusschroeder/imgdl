"""End-to-end throughput through the Python API, against a local server.

Run with `just bench-py`. Skipped unless pytest-benchmark is installed
(`uv sync --group bench`).
"""

from __future__ import annotations

import http.server
import threading
from collections.abc import Iterator
from pathlib import Path

import imgdl
import pytest

pytest.importorskip("pytest_benchmark", reason="run with: uv sync --group bench")

# Routed through the plaintext hook rather than `imgdl.download_images`: the
# production transport always negotiates TLS, so it cannot talk to a local HTTP
# origin. Everything measured here -- orchestration, concurrency, pooling,
# buffer handling, disk writes -- is the same code path. See docs/known-gaps.md;
# when that gap closes, swap this back to `imgdl.download_images`.
from imgdl._imgdl import _download_images_plaintext as _download

PAYLOAD = b"\xff\xd8\xff\xe0" + b"\x00" * 40_000 + b"\xff\xd9"  # ~40 kB fake JPEG


class _Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "image/jpeg")
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.end_headers()
        self.wfile.write(PAYLOAD)

    def log_message(self, *args: object) -> None:
        pass


@pytest.fixture(scope="module")
def server() -> Iterator[str]:
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{httpd.server_address[1]}"
    finally:
        httpd.shutdown()


@pytest.mark.parametrize("count", [100, 1000])
def test_throughput(benchmark, server: str, tmp_path: Path, count: int) -> None:
    urls = [f"{server}/img{i}.jpg" for i in range(count)]
    config = imgdl.Config(max_concurrent=256, max_retries=0)

    def run() -> list:
        return _download(urls, output_dir=str(tmp_path), config=config)

    results = benchmark(run)
    assert sum(r.success for r in results) == count
    benchmark.extra_info["mb_per_s"] = count * len(PAYLOAD) / 1e6 / benchmark.stats["mean"]
