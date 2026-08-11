"""Fast concurrent image downloader.

The heavy lifting happens in Rust (:mod:`imgdl._imgdl`): HTTP/2 multiplexing
over a pooled set of connections, an in-process DNS cache, and pre-allocated
buffers written straight from the socket to disk.

This module is the public surface. Import from here, not from ``_imgdl``::

    import imgdl

    results = imgdl.download_images(
        ["https://example.com/a.jpg", "https://example.com/b.jpg"],
        output_dir="./out",
        config=imgdl.Config(max_concurrent=256),
    )
    ok = sum(r.success for r in results)
"""

from __future__ import annotations

from imgdl import _imgdl
from imgdl._imgdl import Config, DownloadResult, download_images

#: Version of the compiled extension, single-sourced from ``Cargo.toml`` via
#: ``env!("CARGO_PKG_VERSION")``. Correct even in an uninstalled
#: ``maturin develop`` build, where ``importlib.metadata.version("imgdl")`` can
#: disagree with the extension that is actually loaded.
#:
#: Read with ``getattr`` because pyo3-stub-gen does not describe module-level
#: constants, so the generated stub cannot declare it.
__version__: str = getattr(_imgdl, "__version__", "0+unknown")

__all__ = ["Config", "DownloadResult", "__version__", "download_images"]
