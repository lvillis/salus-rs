use std::ffi::OsString;

use crate::error::{AppError, Result};

pub fn expand_argv<I, T>(args: I) -> Result<Vec<OsString>>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut iter = args.into_iter();
    let mut expanded = Vec::new();

    if let Some(arg0) = iter.next() {
        expanded.push(arg0.into());
    }

    for arg in iter {
        expanded.push(expand_arg_os(arg.into(), &lookup_env)?);
    }

    Ok(expanded)
}

#[cfg(unix)]
fn expand_arg_os<F>(arg: OsString, lookup: &F) -> Result<OsString>
where
    F: Fn(&str) -> Result<Option<String>>,
{
    use std::{os::unix::ffi::OsStrExt, str};

    let bytes = arg.as_os_str().as_bytes();
    if !bytes.contains(&b'$') {
        return Ok(arg);
    }

    let raw = str::from_utf8(bytes).map_err(|_| {
        AppError::invalid_config("environment expansion only supports UTF-8 arguments")
    })?;
    Ok(OsString::from(expand_text(raw, lookup)?))
}

#[cfg(not(unix))]
fn expand_arg_os<F>(arg: OsString, lookup: &F) -> Result<OsString>
where
    F: Fn(&str) -> Result<Option<String>>,
{
    let preview = arg.to_string_lossy();
    if !preview.contains('$') {
        return Ok(arg);
    }

    let raw = arg.into_string().map_err(|_| {
        AppError::invalid_config("environment expansion only supports Unicode arguments")
    })?;
    Ok(OsString::from(expand_text(&raw, lookup)?))
}

fn expand_text<F>(raw: &str, lookup: &F) -> Result<String>
where
    F: Fn(&str) -> Result<Option<String>>,
{
    let mut expanded = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '$' {
            expanded.push(ch);
            continue;
        }

        match chars.peek().copied() {
            Some('$') => {
                expanded.push('$');
                chars.next();
            }
            Some('{') => {
                chars.next();
                let mut expr = String::new();
                let mut terminated = false;

                for ch in chars.by_ref() {
                    if ch == '}' {
                        terminated = true;
                        break;
                    }
                    expr.push(ch);
                }

                if !terminated {
                    return Err(AppError::invalid_config(
                        "unterminated environment expansion",
                    ));
                }

                expanded.push_str(&expand_expression(&expr, lookup)?);
            }
            _ => expanded.push('$'),
        }
    }

    Ok(expanded)
}

fn expand_expression<F>(expr: &str, lookup: &F) -> Result<String>
where
    F: Fn(&str) -> Result<Option<String>>,
{
    let (name, default) = match expr.split_once(":-") {
        Some((name, default)) => (name, Some(default)),
        None => (expr, None),
    };

    validate_env_name(name)?;

    match lookup(name)? {
        Some(value) => Ok(value),
        None => match default {
            Some(default) => Ok(default.to_string()),
            None => Err(AppError::invalid_config(format!(
                "environment variable {name} is not set"
            ))),
        },
    }
}

fn validate_env_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch.is_ascii_alphabetic() => {}
        _ => {
            return Err(AppError::invalid_config(format!(
                "invalid environment variable name {name:?}"
            )));
        }
    }

    if chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(AppError::invalid_config(format!(
            "invalid environment variable name {name:?}"
        )))
    }
}

fn lookup_env(name: &str) -> Result<Option<String>> {
    match std::env::var_os(name) {
        Some(value) => value.into_string().map(Some).map_err(|_| {
            AppError::invalid_config(format!("environment variable {name} is not valid UTF-8"))
        }),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::expand_text;
    use crate::error::Result;

    fn expand_with(raw: &str, pairs: &[(&str, &str)]) -> Result<String> {
        expand_text(raw, &|name| {
            Ok(pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string()))
        })
    }

    #[test]
    fn expands_required_variables() {
        let expanded =
            expand_with("http://127.0.0.1:${PORT}/healthz", &[("PORT", "8080")]).unwrap();

        assert_eq!(expanded, "http://127.0.0.1:8080/healthz");
    }

    #[test]
    fn expands_default_when_variable_is_missing() {
        let expanded = expand_with("${MODE:-ready}", &[]).unwrap();

        assert_eq!(expanded, "ready");
    }

    #[test]
    fn leaves_dollar_variable_syntax_unchanged() {
        let expanded = expand_with("$PORT", &[("PORT", "8080")]).unwrap();

        assert_eq!(expanded, "$PORT");
    }

    #[test]
    fn escapes_dollar_signs() {
        let expanded = expand_with("$${PORT}", &[("PORT", "8080")]).unwrap();

        assert_eq!(expanded, "${PORT}");
    }

    #[test]
    fn does_not_expand_defaults_recursively() {
        let expanded = expand_with("${MODE:-${INNER}}", &[("INNER", "ready")]).unwrap();

        assert_eq!(expanded, "${INNER}");
    }

    #[test]
    fn rejects_missing_variables_without_default() {
        let error = expand_with("${PORT}", &[]).unwrap_err();

        assert_eq!(error.to_string(), "environment variable PORT is not set");
    }

    #[test]
    fn rejects_invalid_variable_names() {
        let error = expand_with("${1PORT}", &[]).unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid environment variable name \"1PORT\""
        );
    }

    #[test]
    fn rejects_unterminated_expansions() {
        let error = expand_with("${PORT", &[("PORT", "8080")]).unwrap_err();

        assert_eq!(error.to_string(), "unterminated environment expansion");
    }
}
