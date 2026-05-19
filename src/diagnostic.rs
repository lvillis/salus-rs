use std::{borrow::Cow, ffi::OsStr, path::Path};

pub fn value(raw: &str) -> Cow<'_, str> {
    if raw.is_empty()
        || contains_whitespace(raw)
        || contains_control(raw)
        || contains_quoted_char(raw)
    {
        Cow::Owned(format!("{raw:?}"))
    } else {
        Cow::Borrowed(raw)
    }
}

pub fn os_str(raw: &OsStr) -> String {
    match raw.to_str() {
        Some(raw) => value(raw).into_owned(),
        _ => format!("{raw:?}"),
    }
}

pub fn path(path: &Path) -> String {
    match path.to_str() {
        Some(raw) => value(raw).into_owned(),
        _ => format!("{path:?}"),
    }
}

pub fn path_field(path: &Path) -> String {
    match path.to_str() {
        Some(raw) => raw.to_string(),
        _ => format!("{path:?}"),
    }
}

fn contains_control(raw: &str) -> bool {
    raw.chars().any(char::is_control)
}

fn contains_whitespace(raw: &str) -> bool {
    raw.chars().any(char::is_whitespace)
}

fn contains_quoted_char(raw: &str) -> bool {
    raw.contains(['"', '\\'])
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::Path};

    use super::{os_str, path, path_field, value};

    #[test]
    fn value_preserves_plain_text() {
        assert_eq!(value("127.0.0.1:8080"), "127.0.0.1:8080");
    }

    #[test]
    fn value_escapes_control_characters() {
        assert_eq!(value("bad\naddr:80"), "\"bad\\naddr:80\"");
    }

    #[test]
    fn value_quotes_empty_text() {
        assert_eq!(value(""), "\"\"");
    }

    #[test]
    fn value_quotes_surrounding_whitespace() {
        assert_eq!(value(" bad "), "\" bad \"");
    }

    #[test]
    fn value_quotes_embedded_whitespace() {
        assert_eq!(value("bad value"), "\"bad value\"");
    }

    #[test]
    fn value_quotes_embedded_quote_and_backslash() {
        assert_eq!(value("bad\"value"), "\"bad\\\"value\"");
        assert_eq!(value("bad\\value"), "\"bad\\\\value\"");
    }

    #[test]
    fn path_escapes_control_characters() {
        assert_eq!(path(Path::new("/tmp/bad\npath")), "\"/tmp/bad\\npath\"");
    }

    #[test]
    fn os_str_escapes_control_characters() {
        assert_eq!(os_str(OsStr::new("bad\ncmd")), "\"bad\\ncmd\"");
    }

    #[test]
    fn os_str_quotes_surrounding_whitespace() {
        assert_eq!(os_str(OsStr::new(" cmd ")), "\" cmd \"");
    }

    #[test]
    fn path_quotes_surrounding_whitespace() {
        assert_eq!(path(Path::new("/tmp/ready ")), "\"/tmp/ready \"");
    }

    #[test]
    fn path_field_preserves_utf8_without_prequoting() {
        assert_eq!(path_field(Path::new("/tmp/ready path")), "/tmp/ready path");
    }

    #[cfg(unix)]
    #[test]
    fn os_str_escapes_non_utf8_bytes() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let raw = OsString::from_vec(vec![b'c', b'm', b'd', 0xff]);

        assert_eq!(os_str(&raw), "\"cmd\\xFF\"");
    }

    #[cfg(unix)]
    #[test]
    fn path_escapes_non_utf8_bytes() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

        let raw = PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]));

        assert_eq!(path(&raw), "\"/tmp/\\xFF\"");
    }

    #[cfg(unix)]
    #[test]
    fn path_field_escapes_non_utf8_bytes() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};

        let raw = PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]));

        assert_eq!(path_field(&raw), "\"/tmp/\\xFF\"");
    }
}
