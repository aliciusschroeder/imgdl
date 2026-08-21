"""Tests for the DownloadResult class."""

import imgdl
import pytest

# Use fast-failing config to avoid slow DNS lookups
_FAST_CONFIG = imgdl.Config(max_retries=0, connect_timeout_secs=1.0, request_timeout_secs=1.0)


def _get_failed_result():
    """Get a failed DownloadResult by downloading an invalid URL."""
    results = imgdl.download_images(
        ["http://localhost:1/noimage.jpg"],
        config=_FAST_CONFIG,
    )
    return results[0]


def test_download_result_fields_are_readonly():
    """Setting any DownloadResult field should raise AttributeError (frozen class)."""
    result = _get_failed_result()
    with pytest.raises(AttributeError):
        result.url = "other"
    with pytest.raises(AttributeError):
        result.success = True


def test_download_result_repr():
    """repr() should return a readable string containing key fields."""
    result = _get_failed_result()
    r = repr(result)
    assert "DownloadResult" in r
    assert "url=" in r
    assert "success=" in r


def test_download_result_to_dict_keys():
    """to_dict() should return a dict with all expected keys."""
    result = _get_failed_result()
    d = result.to_dict()
    expected_keys = {
        "url",
        "success",
        "path",
        "error",
        "size_bytes",
        "elapsed_ms",
        "content_hash",
        "retries_attempted",
    }
    assert set(d.keys()) == expected_keys


def test_download_result_to_dict_types():
    """to_dict() values should have correct Python types."""
    result = _get_failed_result()
    d = result.to_dict()
    assert isinstance(d["url"], str)
    assert isinstance(d["success"], bool)
    # For a failed result, path and size_bytes and content_hash should be None
    # error should be a string
    if not d["success"]:
        assert d["path"] is None
        assert d["size_bytes"] is None
        assert d["content_hash"] is None
        assert isinstance(d["error"], str)
