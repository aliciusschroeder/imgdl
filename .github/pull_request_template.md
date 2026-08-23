## What and why

<!-- One or two sentences. Link the issue if there is one. -->

## Checklist

- [ ] `just ci` is green locally
- [ ] Logic changes live in `imgdl-core`, not in the binding layer
- [ ] `just stubs` run and `python/imgdl/_imgdl.pyi` committed (if the Python surface changed)
- [ ] `CHANGELOG.md` updated under `<!-- next-release -->` (unless internal-only)
- [ ] Benchmark numbers included (if `transport.rs`, `pool.rs`, or `[profile.release]` changed)
