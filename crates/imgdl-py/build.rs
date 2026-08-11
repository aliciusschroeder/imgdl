//! Build script for the PyO3 binding crate.
//!
//! Its single job is to kill a papercut that used to require every developer
//! (and every editor config, and every CI job) to set `LD_LIBRARY_PATH` by
//! hand before running `cargo test`, `cargo llvm-cov` or `stub_gen`.
//!
//! Why it is needed: this crate is built WITHOUT `pyo3/extension-module` for
//! tests and stub generation, which means the resulting binaries link against
//! `libpython3.x.so`. Many Python installations (uv-managed, pyenv, Homebrew)
//! put that library somewhere the dynamic loader does not search by default,
//! so the link succeeds and the *run* fails with
//! "error while loading shared libraries: libpython3.12.so.1.0".
//!
//! Baking an rpath into the test/bin artefacts fixes it permanently. The cdylib
//! that maturin builds is unaffected: it is compiled with `extension-module`,
//! which does not link libpython at all.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Make `cfg(Py_3_10)`, `cfg(PyPy)` etc. available and, importantly, keep
    // this build script honest about which interpreter pyo3 actually picked.
    pyo3_build_config::use_pyo3_cfgs();

    let config = pyo3_build_config::get();

    // `lib_dir` is None for a static/embedded build or when `extension-module`
    // is on — nothing to do in those cases.
    let Some(lib_dir) = config.lib_dir.as_deref() else {
        return;
    };

    if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
        // Also resolve relative to the binary for relocatable test artefacts.
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    } else if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
    }
    // Windows resolves python3xx.dll via PATH; the venv Scripts dir is already
    // on PATH whenever `uv run` / an activated venv is in play.
}
