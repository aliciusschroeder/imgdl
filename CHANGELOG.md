# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

<!-- next-release -->

## [0.2.0] - 2026-08-22

Extracted from a private parent monorepo into a standalone repository. The download
engine is unchanged; everything around it was rebuilt.

### Added

- `uv add git+https://github.com/aliciusschroeder/imgdl` installs with no `#subdirectory=`
  fragment, and CI proves it on every pull request.
- Prebuilt `abi3` wheels for Linux (glibc + musl, x86_64 + aarch64), macOS
  (arm64 + x86_64) and Windows, attached to GitHub Releases and published to
  PyPI via trusted publishing.
- `just` as the single entry point: `just` sets up a complete dev environment,
  `just ci` reproduces CI locally, `just doctor` diagnoses a broken setup.
- `imgdl.__version__`, and an `imgdl` console script for one-off downloads.
- Coverage in two flavours: Rust-from-Rust-tests, and Rust-reached-through-the-
  Python-API, uploaded under separate Codecov flags.
- `cargo deny` licence and advisory auditing, Dependabot for all three
  dependency graphs, and pre-commit hooks matching CI.
- Offline successful-download tests. The suite previously had none: with no way
  to point pytest at a local origin, every offline test could only assert on
  connection failures. `imgdl._imgdl._download_images_plaintext` is a
  documented test-only hook that closes that hole — see `docs/known-gaps.md`.

### Changed

- The version now lives in exactly one place (`[workspace.package] version`);
  the six-way duplication and the script that policed it are gone.
- The extension module is `imgdl._imgdl`, wrapped by an explicit
  `python/imgdl/__init__.py`, instead of relying on maturin's implicit
  "generate a package because a `.pyi` exists" behaviour.
- Crates renamed `imgdl_core` → `imgdl-core`, `imgdl_python` → `imgdl-py`.
  The Python import name is unchanged.
- `python/imgdl/_imgdl.pyi` is committed and drift-checked in CI, so types work
  without a Rust build.

### Removed

- The vendored `release/` directory — an unzipped wheel, `.so` and `.pyd`
  included, committed to git so a uv workspace could depend on it. Wheels are
  build artifacts now.
- `LD_LIBRARY_PATH` plumbing from the build, the editor config and CI. A
  `build.rs` bakes the libpython rpath into test binaries instead.
- `scripts/check_imgdl_version.py`, made unnecessary by single-sourcing.

## [0.1.0]

Initial in-monorepo implementation.
