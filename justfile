# imgdl — task runner. Run `just` for a dev environment, `just --list` for everything.
#
# Prerequisites (three, and that's deliberate):
#   rustup   https://rustup.rs           — the pinned toolchain installs itself
#   uv       https://docs.astral.sh/uv/  — Python env + lockfile
#   just     `uv tool install rust-just` — this file
#
# Everything else (maturin, pytest, ruff, nextest, llvm-cov) is installed by
# `just setup` into the project's own .venv / cargo bin. Nothing is global.

set shell := ["bash", "-uc"]

# Where the generated stub lands. pyo3-stub-gen writes a directory-shaped stub
# for mixed layouts; `just stubs` normalises it to this conventional path.
stub := "python/imgdl/_imgdl.pyi"

# `uv run` re-syncs by default, and because `[tool.uv] cache-keys` covers
# **/*.rs, that means a full fat-LTO *release* rebuild after every Rust edit —
# minutes, for a command you wanted to take seconds. So the project itself is
# never installed by uv (`--no-install-project`); `maturin develop` owns the
# extension, and every uv invocation here opts out of the implicit sync.
# `just sync` is the one place dependencies get resolved.
uvr := "uv run --no-sync"

# Set up a complete dev environment (alias for `just dev`).
default: dev

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

# Full dev environment: venv + deps + compiled extension + stubs. Idempotent.
[group('setup')]
dev: sync build stubs
    @echo ""
    @echo "  Ready. Try:  uv run --no-sync python -c 'import imgdl; print(imgdl.__version__)'"
    @echo "  Tests:       just test"

# Install Python dependencies into .venv (does not build the extension).
[group('setup')]
sync:
    uv sync --no-install-project

# Install cargo-nextest, cargo-llvm-cov and cargo-deny (one-time, slow).
[group('setup')]
setup-tools:
    cargo install --locked cargo-nextest cargo-llvm-cov cargo-deny

# Print every prerequisite and its version; non-zero exit if one is missing.
[group('setup')]
doctor:
    #!/usr/bin/env bash
    set -uo pipefail
    fail=0
    # $4 = "required" makes a missing tool a non-zero exit; optional tools only
    # print a hint, so `just doctor` is not noisy for someone who has not run
    # `just setup-tools` yet.
    check() {
      if command -v "$1" >/dev/null 2>&1; then
        printf '  \033[32mok\033[0m   %-14s %s\n' "$1" "$($2 2>&1 | head -1)"
      else
        if [ "${4:-}" = "MISS" ]; then
          printf '  \033[31mMISS\033[0m %-14s %s\n' "$1" "$3"; fail=1
        else
          printf '  \033[33mnote\033[0m %-14s %s\n' "$1" "$3"
        fi
      fi
      return 0
    }
    echo "required:"
    check rustup "rustup --version" "install: https://rustup.rs" MISS
    check cargo  "cargo --version"  "comes with rustup" MISS
    check uv     "uv --version"     "install: https://docs.astral.sh/uv/" MISS
    check just   "just --version"   "install: uv tool install rust-just" MISS
    echo "optional (just setup-tools):"
    check cargo-nextest  "cargo nextest --version"  "just setup-tools"
    check cargo-llvm-cov "cargo llvm-cov --version" "just setup-tools"
    check cargo-deny     "cargo deny --version"     "just setup-tools"
    echo "project:"
    printf '  toolchain    %s\n' "$(cargo --version)"
    printf '  venv python  %s\n' "$(uv run --no-sync python -V 2>/dev/null || echo '(run: just sync)')"
    printf '  imgdl        %s\n' "$(uv run --no-sync python -c 'import imgdl;print(imgdl.__version__)' 2>/dev/null || echo '(run: just build)')"
    if [ "$fail" -ne 0 ]; then echo; echo "missing required tools -- see the hints above"; fi
    exit $fail

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

# Compile the extension into .venv (debug: fast to build, slow to run).
[group('build')]
build: sync
    {{ uvr }} maturin develop --uv

# Compile the extension with release optimisations (required before benchmarking).
[group('build')]
build-release: sync
    # A debug extension is 20-50x slower, which people reliably mistake for a
    # performance regression. Never benchmark the output of `just build`.
    {{ uvr }} maturin develop --uv --release

# Build a distributable wheel into target/wheels/.
[group('build')]
wheel:
    {{ uvr }} maturin build --release --out target/wheels

# Build the source distribution (what `uv add git+` effectively consumes).
[group('build')]
sdist:
    {{ uvr }} maturin sdist --out target/wheels

# Regenerate python/imgdl/_imgdl.pyi from the #[gen_stub_*] annotations.
[group('build')]
stubs:
    #!/usr/bin/env bash
    set -euo pipefail
    # No LD_LIBRARY_PATH dance: crates/imgdl-py/build.rs bakes the libpython
    # rpath into the binary. If this ever fails to load libpython, that is the
    # file to look at.
    # `cargo run` (not ./target/.../stub_gen) is required: pyo3-stub-gen reads
    # CARGO_MANIFEST_DIR unconditionally and panics without it.
    cargo run --quiet -p imgdl-py --bin stub_gen
    # Two bits of post-processing, both deliberate:
    #
    # 1. pyo3-stub-gen writes mixed-layout stubs as <pkg>/<mod>/__init__.pyi.
    #    Normalise to <pkg>/<mod>.pyi — the conventional spot, right next to
    #    the .so, where every type checker looks first.
    if [ -f python/imgdl/_imgdl/__init__.pyi ]; then
      mv python/imgdl/_imgdl/__init__.pyi {{ stub }}
      rmdir python/imgdl/_imgdl
    fi
    # 2. It also emits a parent-package stub (python/imgdl/__init__.pyi) that
    #    declares `__all__ = []`. That stub would shadow our hand-written
    #    __init__.py and tell type checkers the package exports nothing.
    #    The .py file is the source of truth for the public API; drop the stub.
    rm -f python/imgdl/__init__.pyi
    echo "wrote {{ stub }}"

# Fail if the committed stub is stale. CI runs this; so does pre-commit.
[group('build')]
stubs-check: stubs
    #!/usr/bin/env bash
    set -euo pipefail
    if ! git diff --quiet -- {{ stub }}; then
      echo "::error::{{ stub }} is out of date. Run 'just stubs' and commit the result." >&2
      git --no-pager diff -- {{ stub }} >&2
      exit 1
    fi
    echo "stub is up to date"

# ---------------------------------------------------------------------------
# Test
# ---------------------------------------------------------------------------

# Everything CI runs, in CI order (needs `just setup-tools`). Run before pushing.
[group('test')]
ci: fmt-check lint stubs-check test cov

# Rust + Python test suites.
[group('test')]
test: test-rust test-py

# Rust tests across the whole workspace (core logic and binding conversions).
[group('test')]
test-rust:
    #!/usr/bin/env bash
    set -euo pipefail
    # nextest is nicer (per-test process isolation, better output) but it is an
    # optional tool. Falling back keeps `just test` working on a fresh clone
    # instead of failing with "no such subcommand".
    if cargo nextest --version >/dev/null 2>&1; then
      cargo nextest run --workspace --all-targets
    else
      echo "note: cargo-nextest not installed (just setup-tools), using cargo test"
      cargo test --workspace --all-targets
    fi

# Python tests against a freshly built extension.
[group('test')]
test-py: build
    {{ uvr }} pytest

# Python tests in parallel — worth it once the suite grows.
[group('test')]
test-py-fast: build
    {{ uvr }} pytest -n auto

# Only the tests that hit the network (skipped by default in CI).
[group('test')]
test-network: build
    {{ uvr }} pytest -m network

# ---------------------------------------------------------------------------
# Coverage
# ---------------------------------------------------------------------------

# Rust coverage from Rust tests -> target/lcov-rust.info
[group('cov')]
cov:
    cargo llvm-cov nextest --workspace --lcov --output-path target/lcov-rust.info

# Rust coverage attributable to the PYTHON test suite -> target/lcov-py.info
[group('cov')]
cov-py:
    #!/usr/bin/env bash
    set -euo pipefail
    # This is the interesting coverage number: it answers "which Rust paths does
    # our Python API actually exercise?", which plain `cargo llvm-cov` cannot.
    #
    # The ordering is fragile by nature. `show-env` must be sourced BEFORE
    # maturin builds, so the .so pytest imports is the instrumented one, and the
    # target dir must not be cleaned after that point or the profile data goes
    # with it. Both mistakes fail silently, as an empty report.
    source <(cargo llvm-cov show-env --export-prefix)
    cargo llvm-cov clean --workspace
    uv run --no-sync maturin develop --uv
    {{ uvr }} pytest
    cargo llvm-cov report --lcov --output-path target/lcov-py.info
    cargo llvm-cov report --summary-only

# Both coverage flavours plus Python-level line coverage.
[group('cov')]
cov-all: cov cov-py
    {{ uvr }} pytest --cov=imgdl --cov-report=xml:target/coverage-python.xml --cov-report=term

# HTML report you can actually read.
[group('cov')]
cov-html:
    cargo llvm-cov nextest --workspace --html --output-dir target/coverage-html
    @echo "open target/coverage-html/index.html"

# ---------------------------------------------------------------------------
# Lint / format
# ---------------------------------------------------------------------------

# Clippy (lints are configured in Cargo.toml, not here) + ruff + mypy.
[group('lint')]
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    {{ uvr }} ruff check .
    {{ uvr }} mypy

# Rewrite everything in canonical style.
[group('lint')]
fmt:
    cargo fmt --all
    {{ uvr }} ruff format .
    {{ uvr }} ruff check --fix .

# Check formatting without touching files.
[group('lint')]
fmt-check:
    cargo fmt --all --check
    {{ uvr }} ruff format --check .

# Licence and advisory audit of the Rust dependency tree.
[group('lint')]
audit:
    cargo deny check

# Install the git hooks (fmt + lint + stub drift on commit).
[group('lint')]
hooks:
    {{ uvr }} pre-commit install

# ---------------------------------------------------------------------------
# Benchmarks
# ---------------------------------------------------------------------------

# End-to-end throughput through the Python API (builds release first).
[group('bench')]
bench: build-release
    # build-release is not optional. A debug extension is 20-50x slower, and
    # every number measured against one is noise.
    uv run --no-sync --group bench pytest benches/ --benchmark-only

# Re-run the benchmarks and compare against the last saved run.
[group('bench')]
bench-compare: build-release
    uv run --no-sync --group bench pytest benches/ --benchmark-only \
        --benchmark-autosave --benchmark-compare --benchmark-compare-fail=mean:10%

# ---------------------------------------------------------------------------
# Release
# ---------------------------------------------------------------------------

# Bump the version everywhere it matters, then refresh lockfiles (e.g. just bump 0.3.0).
[group('release')]
bump VERSION:
    {{ uvr }} python scripts/bump_version.py {{ VERSION }}
    cargo update --workspace --quiet
    uv lock
    @echo "Now: review the diff, commit, then 'just tag'"

# Tag the current workspace version and push — this is what triggers a release.
[group('release')]
tag:
    #!/usr/bin/env bash
    set -euo pipefail
    v=$({{ uvr }} python scripts/bump_version.py --show)
    git tag -a "v${v}" -m "imgdl v${v}"
    echo "created tag v${v}; push it with: git push origin v${v}"

# Prove `uv add git+<this repo>` still works, from a clean temp project.
[group('release')]
smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    # Mirrors CI's `install-smoke` job. If this breaks, the headline install
    # path is broken for every consumer.
    repo=$(pwd)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    cd "$tmp"
    uv init --bare --name smoke >/dev/null
    uv add "git+file://${repo}"
    uv run python -c "import imgdl; print('installed imgdl', imgdl.__version__, imgdl.download_images)"
    echo "uv add git+ works"

# ---------------------------------------------------------------------------
# Housekeeping
# ---------------------------------------------------------------------------

# Remove build artefacts, caches and the venv.
[group('misc')]
clean:
    cargo clean
    rm -rf .venv .pytest_cache .ruff_cache .mypy_cache
    find . -name '__pycache__' -type d -prune -exec rm -rf {} +

# Build and open the Rust API docs.
[group('misc')]
docs:
    cargo doc --workspace --no-deps --open
