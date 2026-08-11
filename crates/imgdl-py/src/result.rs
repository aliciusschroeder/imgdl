use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3_stub_gen::derive::*;

/// Result of downloading a single URL.
#[gen_stub_pyclass]
#[pyclass(frozen)]
#[derive(Debug)]
pub struct DownloadResult {
    #[pyo3(get)]
    pub url: String,
    #[pyo3(get)]
    pub success: bool,
    #[pyo3(get)]
    pub path: Option<String>,
    #[pyo3(get)]
    pub error: Option<String>,
    #[pyo3(get)]
    pub size_bytes: Option<u64>,
    #[pyo3(get)]
    pub elapsed_ms: Option<f64>,
    #[pyo3(get)]
    pub content_hash: Option<String>,
    #[pyo3(get)]
    pub retries_attempted: Option<u32>,
}

#[gen_stub_pymethods]
#[pymethods]
impl DownloadResult {
    fn __repr__(&self) -> String {
        if self.success {
            format!(
                "DownloadResult(url='{}', success=True, path='{}')",
                self.url,
                self.path.as_deref().unwrap_or("")
            )
        } else {
            format!(
                "DownloadResult(url='{}', success=False, error='{}')",
                self.url,
                self.error.as_deref().unwrap_or("")
            )
        }
    }

    #[gen_stub(override_return_type(type_repr = "dict[str, typing.Any]", imports = ("typing")))]
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("url", &self.url)?;
        dict.set_item("success", self.success)?;
        dict.set_item("path", &self.path)?;
        dict.set_item("error", &self.error)?;
        dict.set_item("size_bytes", self.size_bytes)?;
        dict.set_item("elapsed_ms", self.elapsed_ms)?;
        dict.set_item("content_hash", &self.content_hash)?;
        dict.set_item("retries_attempted", self.retries_attempted)?;
        Ok(dict)
    }
}

/// Convert a core DownloadResult to a Python DownloadResult.
pub(crate) fn core_result_to_python(result: imgdl_core::DownloadResult) -> DownloadResult {
    match result.outcome {
        imgdl_core::DownloadOutcome::Success {
            path,
            size_bytes,
            content_hash,
            elapsed,
        } => DownloadResult {
            url: result.url,
            success: true,
            path: Some(path.display().to_string()),
            error: None,
            size_bytes: Some(size_bytes),
            elapsed_ms: Some(elapsed.as_secs_f64() * 1000.0),
            content_hash,
            retries_attempted: Some(0),
        },
        imgdl_core::DownloadOutcome::Failure {
            error,
            elapsed,
            retries_attempted,
        } => DownloadResult {
            url: result.url,
            success: false,
            path: None,
            error: Some(error.to_string()),
            size_bytes: None,
            elapsed_ms: Some(elapsed.as_secs_f64() * 1000.0),
            content_hash: None,
            retries_attempted: Some(retries_attempted),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn make_success_core_result() -> imgdl_core::DownloadResult {
        imgdl_core::DownloadResult {
            url: "https://example.com/image.jpg".to_string(),
            outcome: imgdl_core::DownloadOutcome::Success {
                path: PathBuf::from("/tmp/image.jpg"),
                size_bytes: 12345,
                content_hash: Some("abc123def456".to_string()),
                elapsed: Duration::from_millis(150),
            },
        }
    }

    fn make_failure_core_result() -> imgdl_core::DownloadResult {
        imgdl_core::DownloadResult {
            url: "https://example.com/missing.jpg".to_string(),
            outcome: imgdl_core::DownloadOutcome::Failure {
                error: imgdl_core::DownloadError::HttpStatus {
                    code: 404,
                    message: "Not Found".to_string(),
                    retry_after: None,
                },
                elapsed: Duration::from_millis(50),
                retries_attempted: 2,
            },
        }
    }

    #[test]
    fn test_convert_success_outcome() {
        let result = core_result_to_python(make_success_core_result());
        assert!(result.success);
        assert_eq!(result.url, "https://example.com/image.jpg");
        assert_eq!(result.path, Some("/tmp/image.jpg".to_string()));
        assert_eq!(result.error, None);
        assert_eq!(result.size_bytes, Some(12345));
        assert_eq!(result.content_hash, Some("abc123def456".to_string()));
        assert_eq!(result.retries_attempted, Some(0));
        assert!(result.elapsed_ms.unwrap() > 0.0);
    }

    #[test]
    fn test_convert_failure_outcome() {
        let result = core_result_to_python(make_failure_core_result());
        assert!(!result.success);
        assert_eq!(result.url, "https://example.com/missing.jpg");
        assert_eq!(result.path, None);
        assert!(result.error.as_ref().unwrap().contains("404"));
        assert_eq!(result.size_bytes, None);
        assert_eq!(result.content_hash, None);
        assert_eq!(result.retries_attempted, Some(2));
        assert!(result.elapsed_ms.unwrap() > 0.0);
    }

    #[test]
    fn test_repr_success() {
        let result = core_result_to_python(make_success_core_result());
        let repr = result.__repr__();
        assert!(repr.contains("success=True"));
        assert!(repr.contains("path='/tmp/image.jpg'"));
        assert!(repr.contains("https://example.com/image.jpg"));
    }

    #[test]
    fn test_repr_failure() {
        let result = core_result_to_python(make_failure_core_result());
        let repr = result.__repr__();
        assert!(repr.contains("success=False"));
        assert!(repr.contains("error="));
        assert!(repr.contains("https://example.com/missing.jpg"));
    }

    #[test]
    fn test_to_dict_has_all_keys() {
        Python::attach(|py| {
            let result = core_result_to_python(make_success_core_result());
            let dict = result.to_dict(py).unwrap();
            let keys: Vec<String> = dict
                .keys()
                .iter()
                .map(|k: Bound<'_, pyo3::PyAny>| k.extract::<String>().unwrap())
                .collect();
            assert_eq!(keys.len(), 8);
            for expected_key in &[
                "url",
                "success",
                "path",
                "error",
                "size_bytes",
                "elapsed_ms",
                "content_hash",
                "retries_attempted",
            ] {
                assert!(
                    keys.contains(&expected_key.to_string()),
                    "Missing key: {expected_key}"
                );
            }
        });
    }

    #[test]
    fn test_to_dict_success_types() {
        Python::attach(|py| {
            let result = core_result_to_python(make_success_core_result());
            let dict = result.to_dict(py).unwrap();

            assert!(dict
                .get_item("url")
                .unwrap()
                .unwrap()
                .extract::<String>()
                .is_ok());
            assert!(dict
                .get_item("success")
                .unwrap()
                .unwrap()
                .extract::<bool>()
                .is_ok());
            assert!(dict
                .get_item("path")
                .unwrap()
                .unwrap()
                .extract::<String>()
                .is_ok());
            assert!(dict.get_item("error").unwrap().unwrap().is_none());
            assert!(dict
                .get_item("size_bytes")
                .unwrap()
                .unwrap()
                .extract::<u64>()
                .is_ok());
            assert!(dict
                .get_item("elapsed_ms")
                .unwrap()
                .unwrap()
                .extract::<f64>()
                .is_ok());
            assert!(dict
                .get_item("content_hash")
                .unwrap()
                .unwrap()
                .extract::<String>()
                .is_ok());
            assert!(dict
                .get_item("retries_attempted")
                .unwrap()
                .unwrap()
                .extract::<u32>()
                .is_ok());
        });
    }

    #[test]
    fn test_to_dict_failure_nones() {
        Python::attach(|py| {
            let result = core_result_to_python(make_failure_core_result());
            let dict = result.to_dict(py).unwrap();

            assert!(dict.get_item("path").unwrap().unwrap().is_none());
            assert!(dict.get_item("size_bytes").unwrap().unwrap().is_none());
            assert!(dict.get_item("content_hash").unwrap().unwrap().is_none());
            assert!(dict
                .get_item("error")
                .unwrap()
                .unwrap()
                .extract::<String>()
                .is_ok());
        });
    }
}
