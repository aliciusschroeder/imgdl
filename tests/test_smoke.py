"""The tests that would have caught a broken package layout.

Everything here is about packaging rather than downloading: is the extension
importable, does it carry its types, is the version single-sourced, is the
private module actually private.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import imgdl
import pytest


def test_public_api_is_explicit():
    """__all__ is hand-maintained, not `import *` leakage."""
    assert set(imgdl.__all__) == {
        "Config",
        "DownloadResult",
        "__version__",
        "download_images",
    }
    for name in imgdl.__all__:
        assert hasattr(imgdl, name), name


def test_version_is_single_sourced():
    """imgdl.__version__ comes from Cargo.toml via env!("CARGO_PKG_VERSION").

    If this ever disagrees with the workspace version, someone has reintroduced
    a second place to write the version down.
    """
    assert imgdl.__version__.count(".") >= 2
    assert imgdl.__version__ == imgdl._imgdl.__version__


def test_package_ships_type_information():
    """py.typed and the generated stub must be installed, not just present in git."""
    pkg = Path(imgdl.__file__).parent
    assert (pkg / "py.typed").is_file(), "py.typed missing — check tool.maturin.python-source"
    assert (pkg / "_imgdl.pyi").is_file(), "stub missing — check tool.maturin.include"


def test_stub_matches_the_runtime_module():
    """Every name the stub promises actually exists at runtime."""
    stub = (Path(imgdl.__file__).parent / "_imgdl.pyi").read_text()
    compile(stub, "_imgdl.pyi", "exec")
    for name in ("class Config", "class DownloadResult", "def download_images"):
        assert name in stub, name
    for name in ("Config", "DownloadResult", "download_images"):
        assert hasattr(imgdl._imgdl, name), name


def test_stub_narrows_naming_strategy_values():
    """The generated stub exposes runtime-accepted naming strategies precisely."""
    stub = (Path(imgdl.__file__).parent / "_imgdl.pyi").read_text()
    naming_strategy_type = (
        'typing.Literal["content_hash", "url_based", "sequential", "file_header"]'
    )
    assert f"def naming_strategy(self) -> {naming_strategy_type}: ..." in stub
    assert f"naming_strategy: {naming_strategy_type} = 'url_based'" in stub


def test_extension_is_private():
    """The compiled module is an implementation detail behind __init__.py."""
    assert imgdl._imgdl.__name__ == "imgdl._imgdl"
    assert imgdl.Config is imgdl._imgdl.Config


def test_docstrings_survive_the_binding_layer():
    assert imgdl.__doc__
    assert imgdl.Config.__doc__
    assert imgdl.download_images.__doc__


@pytest.mark.parametrize("args", [["--version"], ["--help"]])
def test_cli_entry_point(args: list[str]):
    """The console script is wired up and importable in a subprocess."""
    result = subprocess.run(
        [sys.executable, "-m", "imgdl", *args],
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "imgdl" in result.stdout
