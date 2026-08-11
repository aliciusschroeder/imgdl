//! Regenerates `python/imgdl/_imgdl.pyi` from the `#[gen_stub_*]` annotations.
//!
//! Run it with `just stubs`. CI runs the same thing and fails if the committed
//! stub differs, so the type surface can never silently rot.
fn main() -> pyo3_stub_gen::Result<()> {
    let stub = _imgdl::stub_info()?;
    stub.generate()?;
    Ok(())
}
