use std::{borrow::Cow, ffi::OsStr, path::Path};

pub fn value(raw: &str) -> Cow<'_, str> {
    if contains_control(raw) {
        Cow::Owned(format!("{raw:?}"))
    } else {
        Cow::Borrowed(raw)
    }
}

pub fn os_str(raw: &OsStr) -> String {
    match raw.to_str() {
        Some(value) if !contains_control(value) => value.to_string(),
        _ => format!("{raw:?}"),
    }
}

pub fn path(path: &Path) -> String {
    match path.to_str() {
        Some(value) if !contains_control(value) => value.to_string(),
        _ => format!("{path:?}"),
    }
}

fn contains_control(raw: &str) -> bool {
    raw.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::Path};

    use super::{os_str, path, value};

    #[test]
    fn value_preserves_plain_text() {
        assert_eq!(value("127.0.0.1:8080"), "127.0.0.1:8080");
    }

    #[test]
    fn value_escapes_control_characters() {
        assert_eq!(value("bad\naddr:80"), "\"bad\\naddr:80\"");
    }

    #[test]
    fn path_escapes_control_characters() {
        assert_eq!(path(Path::new("/tmp/bad\npath")), "\"/tmp/bad\\npath\"");
    }

    #[test]
    fn os_str_escapes_control_characters() {
        assert_eq!(os_str(OsStr::new("bad\ncmd")), "\"bad\\ncmd\"");
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
}
