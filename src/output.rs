use clap::Error;

pub fn print_clap_error(error: Error, to_stdout: bool) {
    if to_stdout {
        print!("{error}");
    } else {
        eprint!("{error}");
    }
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
        let formatted = format_success("file", "/tmp/ready file", 7, Some("status=\"ok\""));

        assert_eq!(
            formatted,
            "result=healthy mode=file target=\"/tmp/ready file\" duration_ms=7 detail=\"status=\\\"ok\\\"\""
        );
    }
}
