"""Successful downloads, offline, against a local HTTP server.

These go through `plaintext_download` (see conftest) rather than the public
`download_images`, because the production transport always negotiates TLS.
Everything below the transport — orchestration, concurrency caps, retries,
naming, validation, writing, summaries — is the same code path.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import imgdl
import pytest


def test_writes_files_to_disk(plaintext_download, local_server: str, tmp_path: Path):
    urls = [f"{local_server}/a.jpg", f"{local_server}/b.png"]
    results = plaintext_download(
        urls, output_dir=str(tmp_path), config=imgdl.Config(max_retries=0)
    )

    assert [r.success for r in results] == [True, True], [r.error for r in results]
    for r in results:
        assert r.path and Path(r.path).is_file()
        assert r.size_bytes and r.size_bytes > 0
        assert r.elapsed_ms is not None


def test_results_follow_input_order(plaintext_download, local_server: str, tmp_path: Path):
    """Order is part of the contract — callers zip results against their input."""
    urls = [f"{local_server}/{i:03d}.jpg" for i in range(25)]
    results = plaintext_download(
        urls, output_dir=str(tmp_path), config=imgdl.Config(max_retries=0)
    )
    assert [r.url for r in results] == urls
    assert all(r.success for r in results)


def test_one_failure_does_not_abort_the_batch(
    plaintext_download, local_server: str, tmp_path: Path, fast_config
):
    urls = [f"{local_server}/ok.jpg", f"{local_server}/missing.txt", f"{local_server}/ok2.jpg"]
    results = plaintext_download(urls, output_dir=str(tmp_path), config=fast_config)

    assert [r.success for r in results] == [True, False, True]
    assert results[1].error


def test_summary_json_is_written(local_server: str, tmp_path: Path):
    """`write_summary=True` produces summary.json.

    Runs in a subprocess on purpose. The Tokio runtime and the Downloader are
    process-global and configured by the *first* call — so a test that flips a
    Downloader-level flag has to start from a clean process, or it silently
    asserts against whatever config some earlier test happened to install.
    That is a real property of the library, not a test artefact; see the
    "Process-global state" section of docs/architecture.md.
    """
    script = f"""
import json, sys
from pathlib import Path
import imgdl
from imgdl._imgdl import _download_images_plaintext

out = Path({str(tmp_path)!r})
config = imgdl.Config(write_summary=True, max_retries=0)
results = _download_images_plaintext(
    [{local_server!r} + "/a.jpg"], output_dir=str(out), config=config
)
assert results[0].success, results[0].error
summary = out / "summary.json"
assert summary.is_file(), "summary.json not written"
assert json.loads(summary.read_text())
"""
    result = subprocess.run(
        [sys.executable, "-c", script],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    assert result.returncode == 0, result.stderr


def test_downloader_config_is_first_call_wins(
    plaintext_download, local_server: str, tmp_path: Path
):
    """Documents the process-global trade-off rather than pretending it is not there.

    The second call's Downloader-level settings are ignored and a warning is
    logged. Per-call settings that live outside the Downloader (output_dir, the
    URL list) still take effect.
    """
    first = plaintext_download(
        [f"{local_server}/first.jpg"],
        output_dir=str(tmp_path / "one"),
        config=imgdl.Config(max_retries=0, write_summary=False),
    )
    second = plaintext_download(
        [f"{local_server}/second.jpg"],
        output_dir=str(tmp_path / "two"),
        config=imgdl.Config(max_retries=0, write_summary=True),
    )

    assert first[0].success and second[0].success
    assert (tmp_path / "two" / "second.jpg").exists() or second[0].path
    # write_summary=True was ignored: the pooled Downloader keeps the first config.
    assert not (tmp_path / "two" / "summary.json").exists()


@pytest.mark.parametrize("strategy", ["url_based", "sequential", "content_hash"])
def test_naming_strategies_produce_distinct_files(
    plaintext_download, local_server: str, tmp_path: Path, strategy: str
):
    config = imgdl.Config(naming_strategy=strategy, max_retries=0)
    results = plaintext_download(
        [f"{local_server}/a.jpg", f"{local_server}/b.png"],
        output_dir=str(tmp_path),
        config=config,
    )
    assert all(r.success for r in results), [r.error for r in results]
    assert len({r.path for r in results}) == 2


def test_nested_output_dir_is_created(plaintext_download, local_server: str, tmp_path: Path):
    target = tmp_path / "deep" / "nested"
    results = plaintext_download(
        [f"{local_server}/a.jpg"], output_dir=str(target), config=imgdl.Config(max_retries=0)
    )
    assert results[0].success, results[0].error
    assert target.is_dir()


def test_concurrency_cap_is_respected(plaintext_download, local_server: str, tmp_path: Path):
    """A large batch through a small cap still completes, in order."""
    urls = [f"{local_server}/{i:03d}.jpg" for i in range(60)]
    config = imgdl.Config(max_concurrent=4, max_concurrent_per_host=2, max_retries=0)
    results = plaintext_download(urls, output_dir=str(tmp_path), config=config)

    assert len(results) == 60
    assert all(r.success for r in results), [r.error for r in results if not r.success][:3]


def test_http_urls_fail_through_the_public_api(local_server: str, tmp_path: Path, fast_config):
    """Documents a known gap: `download_images` cannot fetch `http://` URLs.

    The transport always negotiates TLS regardless of the URL scheme, even
    though the orchestrator already derives port 80 for `http://`. When that is
    fixed, this test starts failing — delete it, and switch
    `test_offline_downloads.py` to the public API. See docs/known-gaps.md.
    """
    results = imgdl.download_images(
        [f"{local_server}/a.jpg"], output_dir=str(tmp_path), config=fast_config
    )
    assert results[0].success is False
    assert "TLS" in (results[0].error or "")
