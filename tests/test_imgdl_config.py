"""Tests for the Config class."""

import imgdl
import pytest


def test_config_defaults():
    """Config() with no args should produce correct default values."""
    config = imgdl.Config()
    assert config.buffer_size == 524_288
    assert config.connections_per_host == 1
    assert config.dns_cache_ttl_secs == 300
    assert config.max_concurrent == 200
    assert config.max_concurrent_per_host == 100
    assert config.max_retries == 3
    assert config.retry_base_delay_ms == 100
    assert config.connect_timeout_secs == 10.0
    assert config.request_timeout_secs == 30.0
    assert config.batch_timeout_secs is None
    assert config.naming_strategy == "url_based"
    assert config.write_metadata is False
    assert config.write_summary is False
    assert config.runtime_threads == 0


def test_config_custom_values():
    """Config() with all keyword args should store all values correctly."""
    config = imgdl.Config(
        buffer_size=1024,
        connections_per_host=4,
        dns_cache_ttl_secs=600,
        max_concurrent=50,
        max_concurrent_per_host=10,
        max_retries=5,
        retry_base_delay_ms=200,
        connect_timeout_secs=5.0,
        request_timeout_secs=60.0,
        batch_timeout_secs=120.0,
        naming_strategy="content_hash",
        write_metadata=True,
        write_summary=True,
        runtime_threads=4,
    )
    assert config.buffer_size == 1024
    assert config.connections_per_host == 4
    assert config.dns_cache_ttl_secs == 600
    assert config.max_concurrent == 50
    assert config.max_concurrent_per_host == 10
    assert config.max_retries == 5
    assert config.retry_base_delay_ms == 200
    assert config.connect_timeout_secs == 5.0
    assert config.request_timeout_secs == 60.0
    assert config.batch_timeout_secs == 120.0
    assert config.naming_strategy == "content_hash"
    assert config.write_metadata is True
    assert config.write_summary is True
    assert config.runtime_threads == 4


def test_config_invalid_naming_strategy():
    """Config(naming_strategy="invalid") should raise ValueError."""
    with pytest.raises(ValueError, match="naming_strategy"):
        imgdl.Config(naming_strategy="invalid")


def test_config_empty_naming_strategy():
    """Config(naming_strategy="") should raise ValueError."""
    with pytest.raises(ValueError, match="Invalid naming_strategy"):
        imgdl.Config(naming_strategy="")


def test_config_naming_strategy_content_hash():
    """Config(naming_strategy="content_hash") should be accepted."""
    config = imgdl.Config(naming_strategy="content_hash")
    assert config.naming_strategy == "content_hash"


def test_config_naming_strategy_url_based():
    """Config(naming_strategy="url_based") should be accepted."""
    config = imgdl.Config(naming_strategy="url_based")
    assert config.naming_strategy == "url_based"


def test_config_naming_strategy_sequential():
    """Config(naming_strategy="sequential") should be accepted."""
    config = imgdl.Config(naming_strategy="sequential")
    assert config.naming_strategy == "sequential"


def test_config_naming_strategy_file_header():
    """Config(naming_strategy="file_header") should be accepted."""
    config = imgdl.Config(naming_strategy="file_header")
    assert config.naming_strategy == "file_header"


def test_config_fields_are_readonly():
    """Setting any Config field should raise AttributeError (frozen class)."""
    config = imgdl.Config()
    with pytest.raises(AttributeError):
        config.buffer_size = 999  # type: ignore[invalid-assignment]
    with pytest.raises(AttributeError):
        config.max_concurrent = 999  # type: ignore[invalid-assignment]
    with pytest.raises(AttributeError):
        config.naming_strategy = "sequential"  # type: ignore[invalid-assignment]
