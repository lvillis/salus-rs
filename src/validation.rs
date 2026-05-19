use std::{path::Path, time::Duration};

use crate::error::{AppError, Result};

pub(crate) const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn validate_capture_limit(
    flag: &str,
    limit: usize,
    assertion_label: &str,
    enabled: bool,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }

    if limit == 0 {
        return Err(AppError::invalid_config(format!(
            "{flag} must be greater than 0 when {assertion_label} are used"
        )));
    }
    if limit > MAX_CAPTURE_BYTES {
        return Err(AppError::invalid_config(format!(
            "{flag} must be at most {MAX_CAPTURE_BYTES} bytes when {assertion_label} are used"
        )));
    }

    Ok(())
}

pub(crate) fn validate_non_empty_str(flag: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(empty_value_error(flag));
    }

    Ok(())
}

pub(crate) fn validate_non_empty_path(flag: &str, value: &Path) -> Result<()> {
    if value.as_os_str().is_empty() {
        return Err(empty_value_error(flag));
    }

    Ok(())
}

pub(crate) fn validate_non_empty_values(flag: &str, values: &[String]) -> Result<()> {
    for value in values {
        validate_non_empty_str(flag, value)?;
    }

    Ok(())
}

pub(crate) fn validate_positive_duration(flag: &str, value: Duration) -> Result<()> {
    if value.is_zero() {
        return Err(AppError::invalid_config(format!(
            "{flag} must be greater than 0"
        )));
    }

    Ok(())
}

fn empty_value_error(flag: &str) -> AppError {
    AppError::invalid_config(format!("{flag} must not be empty"))
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::{
        validate_capture_limit, validate_non_empty_path, validate_non_empty_str,
        validate_non_empty_values, validate_positive_duration,
    };

    #[test]
    fn capture_limit_validation_only_applies_when_assertions_are_enabled() {
        validate_capture_limit("--max-body", 0, "body assertions", false).unwrap();

        let error = validate_capture_limit("--max-body", 0, "body assertions", true).unwrap_err();

        assert_eq!(
            error.to_string(),
            "--max-body must be greater than 0 when body assertions are used"
        );
    }

    #[test]
    fn non_empty_str_validation_reports_flag_name() {
        let error = validate_non_empty_str("--url", "").unwrap_err();

        assert_eq!(error.to_string(), "--url must not be empty");
    }

    #[test]
    fn non_empty_path_validation_reports_flag_name() {
        let error = validate_non_empty_path("--path", &PathBuf::new()).unwrap_err();

        assert_eq!(error.to_string(), "--path must not be empty");
    }

    #[test]
    fn non_empty_values_validation_reports_flag_name() {
        let error = validate_non_empty_values("--contains", &[String::new()]).unwrap_err();

        assert_eq!(error.to_string(), "--contains must not be empty");
    }

    #[test]
    fn positive_duration_validation_reports_flag_name() {
        let error = validate_positive_duration("--timeout", Duration::ZERO).unwrap_err();

        assert_eq!(error.to_string(), "--timeout must be greater than 0");
    }
}
