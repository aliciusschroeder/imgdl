# Contributing

## Setup

```bash
just doctor     # tells you which of rustup / uv / just you are missing
just            # venv + deps + compiled extension + stubs
just hooks      # optional: pre-commit runs fmt/clippy/stub-drift on commit
```

`just ci` runs exactly what CI runs, in the same order. If it is green locally
it should be green on GitHub; if it is not, that is a bug in the workflow and
worth reporting.

## Where code goes

**`crates/imgdl-core`** — all logic. Pure Rust, no `pyo3` dependency, no Python
in the test loop. New behaviour goes here, with tests here.

**`crates/imgdl-py`** — conversions and error mapping only.

**`python/imgdl`** — the public surface. `__init__.py` re-exports explicitly
(never `import *`), which is what gives users a real `__all__`, working
autocomplete, and a place to put pure-Python conveniences.

## Adding something to the Python API

1. Implement and test it in `imgdl-core`.
2. Add a thin wrapper in `crates/imgdl-py/src/`, annotated with
   `#[gen_stub_pyfunction]` / `#[gen_stub_pymethods]`.
3. Register it in the `#[pymodule]` in `crates/imgdl-py/src/lib.rs`.
4. Re-export it from `python/imgdl/__init__.py` and add it to `__all__`.
5. `just stubs` — regenerates `python/imgdl/_imgdl.pyi`. **Commit the result.**
6. Add a pytest in `tests/`.

Step 5 is enforced: CI regenerates the stub and fails if the committed file
differs. The stub is committed rather than generated at build time so that
editors, `mypy` and downstream users get types without needing a Rust build.

## Conventions

- **Lints** live in `[workspace.lints]` in `Cargo.toml`, not in CI flags, so
  `cargo clippy` means the same thing everywhere. `-D warnings` in CI.
- **Commit messages** follow [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `perf:`, `chore:`). Release notes are generated from them.
- **Formatting** is `cargo fmt` + `ruff format`. `just fmt` does both.

## Testing

```bash
just test           # Rust + Python
just test-rust      # cargo nextest, whole workspace
just test-py        # rebuilds the extension, then pytest
just test-network   # the tests marked `network` (skipped in CI by default)
```

Tests that reach the public internet must be marked `@pytest.mark.network`.
Everything else uses `wiremock` (Rust) or a local server (Python) so the suite
is deterministic offline.

## Coverage

```bash
just cov        # Rust coverage from Rust tests
just cov-py     # Rust coverage attributable to the PYTHON test suite
just cov-html   # browsable report
```

`cov-py` is the interesting one — it answers "which Rust paths does our Python
API actually exercise?", which plain `cargo llvm-cov` cannot. It works by
sourcing `cargo llvm-cov show-env` *before* maturin builds, so the `.so` pytest
imports is instrumented. The ordering is fragile by nature: sourcing after the
build, or cleaning the target dir in between, silently produces empty data.

## Benchmarks

```bash
just bench           # release build + end-to-end throughput
just bench-compare   # same, but fails if the mean regressed >10% vs the last save
```

## Releasing

```bash
just bump 0.3.0     # edits the ONE place the version lives; opens a CHANGELOG section
# review the diff, edit CHANGELOG.md
git commit -am "chore: release v0.3.0"
just tag
git push --follow-tags
```

The tag triggers `release.yml`: it verifies the tag matches the workspace
version, builds the wheel matrix and an sdist, verifies the sdist builds from
scratch, attaches everything to a GitHub Release, and publishes to PyPI via
trusted publishing (OIDC — there is no API token to leak or rotate).

## Misc notes

- **`pyo3/extension-module` must stay off by default.** With it always on, the
  crate does not link libpython and every `cargo test` / `cargo llvm-cov`
  invocation dies with undefined Python symbols. maturin enables it via
  `tool.maturin.features`; nothing else should.
- **`crate-type` must keep `rlib`.** Dropping it silently makes `cargo nextest`
  and doctests unable to see the binding crate at all — the tests do not fail,
  they disappear.
- **`[tool.uv] cache-keys` covers `**/*.rs`.** Without it, `uv sync` serves a
  stale compiled extension after you edit Rust, and you debug a fix that is
  already applied.
- **`python-source` is mandatory** for this layout. Without it maturin produces
  a pure-extension package and `_imgdl.pyi` / `py.typed` never ship, so users
  silently lose types.
- **Both lockfiles are committed.** `Cargo.lock` is what makes `uv add git+`
  reproducible for consumers; `uv.lock` is what makes the dev environment
  reproducible for us.
- Don't forget to add `--no-sync` when using `uv run ...` or you'll face a 
  full fat-LTO *release* rebuild
