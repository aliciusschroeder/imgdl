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
