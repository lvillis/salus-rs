use std::io::{self, Write};

use clap::Error;

pub fn print_clap_error(error: Error, to_stdout: bool) {
    if to_stdout {
        let mut stdout = io::stdout().lock();
        let _ = write!(stdout, "{error}");
        let _ = stdout.flush();
    } else {
        let mut stderr = io::stderr().lock();
        let _ = write!(stderr, "{error}");
        let _ = stderr.flush();
    }
}

pub fn print_stderr_line(message: impl std::fmt::Display) {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "{message}");
    let _ = stderr.flush();
}

pub fn format_success(mode: &str, target: &str, duration_ms: u128, detail: Option<&str>) -> String {
    match detail {
        Some(detail) if !detail.is_empty() => {
            format!(
                "result=healthy mode={mode} target={} duration_ms={duration_ms} detail={}",
                quote_field_value(target),
                quote_field_value(detail)
            )
        }
        _ => format!(
            "result=healthy mode={mode} target={} duration_ms={duration_ms}",
            quote_field_value(target)
        ),
    }
}

fn quote_field_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => {
                escaped.push_str(&format!("\\u{{{:04X}}}", u32::from(ch)));
            }
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::format_success;

    #[test]
    fn success_output_quotes_and_escapes_fields() {
        let formatted = format_success("file", "/tmp/ready\u{1b} file", 7, Some("status=\"ok\""));

        assert_eq!(
            formatted,
            "result=healthy mode=file target=\"/tmp/ready\\u{001B} file\" duration_ms=7 detail=\"status=\\\"ok\\\"\""
        );
    }
}
