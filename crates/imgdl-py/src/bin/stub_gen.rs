//! Regenerates `python/imgdl/_imgdl.pyi` from the `#[gen_stub_*]` annotations.
//!
//! Run it with `just stubs`. CI runs the same thing and fails if the committed
//! stub differs, so the type surface can never silently rot.
use std::path::Path;

const NAMING_STRATEGY_TYPE: &str =
    r#"typing.Literal["content_hash", "url_based", "sequential", "file_header"]"#;

fn main() -> pyo3_stub_gen::Result<()> {
    let stub = _imgdl::stub_info()?;
    stub.generate()?;
    narrow_naming_strategy_type()?;
    Ok(())
}

fn narrow_naming_strategy_type() -> pyo3_stub_gen::Result<()> {
    let paths = generated_stub_paths();
    if paths.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not find generated imgdl._imgdl stub to post-process",
        )
        .into());
    }
    for path in paths {
        let stub = std::fs::read_to_string(&path)?;
        let stub = replace_once_or_confirm(
            stub,
            "def naming_strategy(self) -> builtins.str: ...",
            &format!("def naming_strategy(self) -> {NAMING_STRATEGY_TYPE}: ..."),
        )?;
        let stub = replace_once_or_confirm(
            stub,
            "naming_strategy: builtins.str = 'url_based'",
            &format!("naming_strategy: {NAMING_STRATEGY_TYPE} = 'url_based'"),
        )?;
        std::fs::write(path, stub)?;
    }
    Ok(())
}

fn generated_stub_paths() -> Vec<std::path::PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    [
        manifest_dir.join("../../python/imgdl/_imgdl.pyi"),
        manifest_dir.join("../../python/imgdl/_imgdl/__init__.pyi"),
    ]
    .into_iter()
    .filter(|path| path.is_file())
    .collect()
}

fn replace_once_or_confirm(
    haystack: String,
    needle: &str,
    replacement: &str,
) -> pyo3_stub_gen::Result<String> {
    let count = haystack.matches(needle).count();
    if count == 1 {
        return Ok(haystack.replacen(needle, replacement, 1));
    }
    if count == 0 && haystack.matches(replacement).count() == 1 {
        return Ok(haystack);
    }
    if count == 0 {
        return Err(std::io::Error::other(format!(
            "expected {needle:?} or {replacement:?}, found neither"
        ))
        .into());
    }
    Err(std::io::Error::other(format!(
        "expected one occurrence of {needle:?}, found {count}"
    ))
    .into())
}
