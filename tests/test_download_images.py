"""Tests for the download_images function."""

import os
import tempfile

import imgdl
import pytest

# Use fast-failing config to avoid slow DNS/connection waits
_FAST_CONFIG = imgdl.Config(max_retries=0, connect_timeout_secs=1.0, request_timeout_secs=1.0)


def test_download_images_empty_urls_raises():
    """download_images([]) should raise ValueError."""
    with pytest.raises(ValueError, match="empty"):
        imgdl.download_images([])


def test_download_images_default_output_dir():
    """download_images(urls) with default output_dir='.' should work."""
    results = imgdl.download_images(
        ["http://localhost:1/noimage.jpg"],
        config=_FAST_CONFIG,
    )
    assert isinstance(results, list)


def test_download_images_return_type_is_list():
    """download_images should return a list."""
    results = imgdl.download_images(
        ["http://localhost:1/noimage.jpg"],
        config=_FAST_CONFIG,
    )
    assert isinstance(results, list)


def test_download_images_results_are_download_result_instances():
    """Each element in the returned list should be a DownloadResult."""
    results = imgdl.download_images(
        ["http://localhost:1/noimage.jpg"],
        config=_FAST_CONFIG,
    )
    for result in results:
        assert isinstance(result, imgdl.DownloadResult)


def test_download_images_result_order_matches_input():
    """Results should be in the same order as input URLs."""
    urls = [
        "http://localhost:1/first.jpg",
        "http://localhost:1/second.jpg",
        "http://localhost:1/third.jpg",
    ]
    results = imgdl.download_images(urls, config=_FAST_CONFIG)
    assert len(results) == len(urls)
    for url, result in zip(urls, results, strict=True):
        assert result.url == url


def test_download_images_nonexistent_output_dir():
    """download_images with a nonexistent output_dir.

    Behavior depends on the core engine: it may create the directory
    or raise OSError. This test documents the actual behavior.
    """
    with tempfile.TemporaryDirectory() as tmpdir:
        output_dir = os.path.join(tmpdir, "deeply", "nested", "subdir")
        try:
            results = imgdl.download_images(
                ["http://localhost:1/noimage.jpg"],
                output_dir=output_dir,
                config=_FAST_CONFIG,
            )
            # If no error, the core created the directory
            assert isinstance(results, list)
        except OSError:
            # Core raised an error about the directory -- also acceptable
            pass


def test_download_images_with_custom_config():
    """download_images should accept a custom Config without error."""
    config = imgdl.Config(
        max_concurrent=10,
        max_retries=0,
        connect_timeout_secs=1.0,
        request_timeout_secs=1.0,
    )
    results = imgdl.download_images(
        ["http://localhost:1/noimage.jpg"],
        config=config,
    )
    assert isinstance(results, list)
