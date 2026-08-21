"""Shared pytest fixtures.

These tests run against the compiled extension, so `just test-py` (which
rebuilds it first) is the right way to invoke them. A bare `pytest` tests
whatever `.so` happens to be installed.

Two things to know about the fixtures here:

1. `local_server` serves plain HTTP. The production `download_images` always
   negotiates TLS, so it cannot talk to this server — tests that need a
   *successful* download offline use `plaintext_download`, which routes through
   `imgdl._imgdl._download_images_plaintext`. See `docs/known-gaps.md`.
2. Anything that reaches the public internet must be marked
   `@pytest.mark.network`. CI runs `-m "not network"`.
"""

from __future__ import annotations

import http.server
import threading
from collections.abc import Callable, Iterator
from typing import Any

import pytest

# Minimal but valid magic bytes — enough for imgdl-core's content sniffing.
JPEG = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00" + b"\xaa" * 80
PNG = b"\x89PNG\r\n\x1a\n" + b"\xbb" * 88


@pytest.fixture
def fast_config() -> Any:
    """Config that fails fast: no retries, one-second timeouts.

    Every test that expects a failure should use this. Without it the suite
    spends most of its wall clock waiting for retries it does not care about.
    """
    import imgdl

    return imgdl.Config(max_retries=0, connect_timeout_secs=1.0, request_timeout_secs=1.0)


class _Handler(http.server.BaseHTTPRequestHandler):
    """Serves `*.jpg` and `*.png`; 404 for everything else."""

    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:
        path = self.path.split("?")[0]
        if path.endswith((".jpg", ".jpeg")):
            self._send(JPEG, "image/jpeg")
        elif path.endswith(".png"):
            self._send(PNG, "image/png")
        else:
            # A hand-rolled 404 rather than send_error(): the stdlib version
            # sends `Connection: close`, which kills the pooled keep-alive
            # connection and makes every later request in the same batch fail.
            self._send(b"not found", "text/plain", status=404)

    def _send(self, body: bytes, content_type: str, status: int = 200) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args: object) -> None:
        pass

    def handle_one_request(self) -> None:
        # A client that hangs up mid-request is normal here; letting the
        # exception escape turns into a pytest "unraisable exception" failure
        # in an unrelated test.
        try:
            super().handle_one_request()
        except (ConnectionError, OSError):
            self.close_connection = True


@pytest.fixture(scope="session")
def local_server() -> Iterator[str]:
    """A real HTTP server on an ephemeral localhost port."""
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    httpd.daemon_threads = True
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{httpd.server_address[1]}"
    finally:
        httpd.shutdown()
        httpd.server_close()
        thread.join(timeout=5)


@pytest.fixture(scope="session")
def plaintext_download() -> Callable[..., list]:
    """The no-TLS download entry point, for offline success-path tests.

    Exercises the same orchestrator, pool, retry and output code as the public
    API — only the transport's TLS layer is bypassed.
    """
    from imgdl._imgdl import _download_images_plaintext

    return _download_images_plaintext
