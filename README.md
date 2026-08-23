# imgdl

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![CI](https://github.com/aliciusschroeder/imgdl/actions/workflows/ci.yml/badge.svg)](https://github.com/aliciusschroeder/imgdl/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/aliciusschroeder/imgdl/branch/main/graph/badge.svg)](https://codecov.io/gh/aliciusschroeder/imgdl)


Download a lot of small images from a handful of hosts, fast.

`imgdl` is a Rust HTTP client (Tokio + Hyper) exposed to Python through PyO3.
It multiplexes over HTTP/2, pools connections per host, caches DNS in-process,
and writes straight from the socket into pre-allocated buffers — so a batch of
ten thousand thumbnails costs you one GIL release, not ten thousand `bytes`
objects.

```python
import imgdl

results = imgdl.download_images(
    urls,  # list[str]
    output_dir="./images",
    config=imgdl.Config(max_concurrent=256, max_retries=3),
)

ok = [r for r in results if r.success]
print(f"{len(ok)}/{len(results)} in {max(r.elapsed_ms or 0 for r in results):.0f} ms")
```

---

## Install

**With a Rust toolchain** (internal services, CI, anywhere `rustup` is present; uv clones, builds, and caches the wheel):

```bash
uv add git+https://github.com/aliciusschroeder/imgdl              # latest main
uv add git+https://github.com/aliciusschroeder/imgdl --tag v0.2.0 # pinned
```

**Without a Rust toolchain** — prebuilt `abi3` wheels are attached to every
[GitHub Release](https://github.com/aliciusschroeder/imgdl/releases):

```bash
uv add https://github.com/aliciusschroeder/imgdl/releases/download/v0.2.0/imgdl-0.2.0-cp310-abi3-manylinux_2_34_x86_64.whl
```

One wheel covers CPython 3.10 through whatever ships next: the extension is
built against the stable ABI (`abi3-py310`).

| Platform             | glibc | musl |
| -------------------- | ----- | ---- |
| Linux x86_64         | ✅     | ✅    |
| Linux aarch64        | ✅     | ✅    |
| macOS arm64 / x86_64 | ✅     | —    |
| Windows x86_64       | ✅     | —    |

Try it without installing anything permanent:

```bash
uvx --from git+https://github.com/aliciusschroeder/imgdl imgdl https://example.com/a.jpg -o ./out
```

## Develop

Prerequisites: [rustup](https://rustup.rs),
[uv](https://docs.astral.sh/uv/), and `just` (`uv tool install rust-just`).
Then:

```bash
git clone https://github.com/aliciusschroeder/imgdl && cd imgdl
just
```

That is the whole setup. `just` creates the venv, installs dependencies,
compiles the extension into it, and regenerates the type stubs. The pinned
toolchain in `rust-toolchain.toml` installs itself on first `cargo` invocation.

```bash
just doctor      # check prerequisites, print versions
just test        # cargo nextest + pytest
just ci          # everything CI runs, in CI order
just --list      # all recipes, grouped
```

## Layout

```
imgdl/
├── Cargo.toml              workspace + the single version number
├── pyproject.toml          the Python package (at the root, on purpose)
├── rust-toolchain.toml     pinned compiler, installs itself
├── justfile                every task a developer or CI runs
├── crates/
│   ├── imgdl-core/         all the logic — pure Rust, no pyo3, no Python in the test loop
│   └── imgdl-py/           thin PyO3 layer: type conversion and error mapping only
│       └── build.rs        bakes the libpython rpath into test binaries
├── python/imgdl/           public API, type stub, py.typed, CLI
├── tests/                  pytest, against the built extension
├── benches/                criterion + end-to-end throughput
└── .github/
    ├── actions/setup/      one composite step every job reuses
    └── workflows/          ci.yml, release.yml
```

## Configuration

Every field of `imgdl.Config` has a default; pass only what you want to change.

| Field                     | Default       | Notes                                               |
| ------------------------- | ------------- | --------------------------------------------------- |
| `max_concurrent`          | 200           | total in-flight requests                            |
| `max_concurrent_per_host` | 100           | per-host cap, applied on top                        |
| `connections_per_host`    | 1             | HTTP/2 multiplexes; raise only for HTTP/1.1 origins |
| `buffer_size`             | 524288        | read buffer, bytes                                  |
| `max_retries`             | 3             | exponential backoff from `retry_base_delay_ms`      |
| `retry_base_delay_ms`     | 100           |                                                     |
| `connect_timeout_secs`    | 10.0          |                                                     |
| `request_timeout_secs`    | 30.0          |                                                     |
| `batch_timeout_secs`      | `None`        | overall deadline for the batch                      |
| `dns_cache_ttl_secs`      | 300           | in-process cache                                    |
| `naming_strategy`         | `"url_based"` | also `sequential`, `content_hash`, `header_based`   |
| `write_metadata`          | `False`       | per-file JSON: headers, hash, timings               |
| `write_summary`           | `False`       | one `summary.json` for the batch                    |
| `runtime_threads`         | 0             | 0 = one worker per core                             |

`download_images` returns one `DownloadResult` per input URL, **in input
order**, with `.success`, `.path`, `.error`, `.size_bytes`, `.elapsed_ms`,
`.content_hash` and `.retries_attempted`.

## Notes and caveats

- **`http://` URLs do not work.** The transport always negotiates TLS.
- **gVisor.** The decision to avoid `io_uring` and other post-4.4 kernel features
  has been made on purpose to optimize for running on sandboxes like modal.
