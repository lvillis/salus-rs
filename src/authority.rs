use std::net::Ipv6Addr;

use hyper::http::uri::Authority;

use crate::{
    diagnostic,
    error::{AppError, Result},
};

#[derive(Clone, Copy)]
pub enum PortPolicy {
    Optional,
    Required,
}

#[derive(Clone, Copy)]
pub enum RawFormat {
    Display,
    Debug,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PortError {
    NotInteger,
    OutOfRange,
}

impl PortError {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::NotInteger => "port must be a valid integer",
            Self::OutOfRange => "port must be between 1 and 65535",
        }
    }
}

impl std::fmt::Display for PortError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

pub fn validate_authority(
    raw: &str,
    label: &str,
    port_policy: PortPolicy,
    raw_format: RawFormat,
) -> Result<Authority> {
    validate_authority_text(raw)
        .map_err(|reason| invalid_authority(label, raw, raw_format, reason))?;
    if raw.contains('@') {
        return Err(invalid_authority(
            label,
            raw,
            raw_format,
            "user info is not allowed",
        ));
    }
    if raw.contains(['/', '?', '#']) {
        return Err(invalid_authority(
            label,
            raw,
            raw_format,
            "path, query, and fragment are not allowed",
        ));
    }
    validate_bracketed_host(raw)
        .map_err(|reason| invalid_authority(label, raw, raw_format, reason))?;
    if is_unbracketed_ipv6_authority(raw) {
        return Err(invalid_authority(
            label,
            raw,
            raw_format,
            IPV6_BRACKETS_REQUIRED,
        ));
    }
    if raw_host_is_empty(raw) {
        return Err(invalid_authority(
            label,
            raw,
            raw_format,
            "host must not be empty",
        ));
    }

    let explicit_port = explicit_port(raw);
    if let Some(port) = explicit_port {
        validate_port(port).map_err(|reason| invalid_authority(label, raw, raw_format, reason))?;
    }

    let authority = raw
        .parse::<Authority>()
        .map_err(|error| invalid_authority(label, raw, raw_format, error))?;
    if authority.host().is_empty() {
        return Err(invalid_authority(
            label,
            raw,
            raw_format,
            "host must not be empty",
        ));
    }
    if explicit_port.is_none() && matches!(port_policy, PortPolicy::Required) {
        return Err(invalid_authority(
            label,
            raw,
            raw_format,
            "port is required",
        ));
    }

    Ok(authority)
}

fn validate_authority_text(raw: &str) -> std::result::Result<(), &'static str> {
    if raw
        .chars()
        .any(|ch| ch == '\\' || ch == '"' || ch.is_whitespace() || ch.is_control())
    {
        return Err(
            "authority must not contain whitespace, control characters, backslashes, or quotes",
        );
    }
    if !raw.is_ascii() {
        return Err(
            "authority must contain only ASCII characters; use punycode for internationalized names",
        );
    }

    Ok(())
}

pub(crate) fn raw_host_is_empty(raw: &str) -> bool {
    if let Some(rest) = raw.strip_prefix('[') {
        return rest.starts_with(']');
    }

    raw.split_once(':')
        .map_or(raw.is_empty(), |(host, _)| host.is_empty())
}

pub(crate) const IPV6_BRACKETS_REQUIRED: &str = "IPv6 addresses must be enclosed in brackets";

pub(crate) fn is_unbracketed_ipv6_authority(raw: &str) -> bool {
    if raw.starts_with('[') || !raw.contains(':') {
        return false;
    }

    raw.parse::<Ipv6Addr>().is_ok()
        || raw
            .rsplit_once(':')
            .is_some_and(|(host, _)| host.parse::<Ipv6Addr>().is_ok())
        || raw.contains("::")
}

fn validate_bracketed_host(raw: &str) -> std::result::Result<(), &'static str> {
    let Some(rest) = raw.strip_prefix('[') else {
        if raw.contains('[') || raw.contains(']') {
            return Err("brackets are only allowed around IPv6 addresses");
        }
        return Ok(());
    };
    let Some(end) = rest.find(']') else {
        return Err("bracketed host is missing closing ]");
    };

    let host = &rest[..end];
    if host.is_empty() {
        return Err("host must not be empty");
    }
    if host.parse::<Ipv6Addr>().is_err() {
        return Err("bracketed host must be a valid IPv6 address");
    }
    let suffix = &rest[end + 1..];
    if !suffix.is_empty() && !suffix.starts_with(':') {
        return Err("bracketed host must be followed by :port or the end of the authority");
    }

    Ok(())
}

pub(crate) fn validate_port(raw: &str) -> std::result::Result<u16, PortError> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PortError::NotInteger);
    }

    let Ok(port) = raw.parse::<u16>() else {
        return Err(PortError::OutOfRange);
    };
    if port == 0 {
        return Err(PortError::OutOfRange);
    }

    Ok(port)
}

pub(crate) fn explicit_port(raw: &str) -> Option<&str> {
    if let Some(rest) = raw.strip_prefix('[') {
        let end = rest.find(']')?;
        return rest[end + 1..].strip_prefix(':');
    }

    raw.split_once(':').map(|(_, port)| port)
}

fn invalid_authority(
    label: &str,
    raw: &str,
    raw_format: RawFormat,
    reason: impl std::fmt::Display,
) -> AppError {
    let raw = match raw_format {
        RawFormat::Display => diagnostic::value(raw).into_owned(),
        RawFormat::Debug => format!("{raw:?}"),
    };
    AppError::invalid_config(format!("invalid {label} {raw}: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::{
        PortError, PortPolicy, RawFormat, explicit_port, validate_authority, validate_port,
    };

    #[test]
    fn explicit_port_handles_ipv6_authorities() {
        assert_eq!(explicit_port("[::1]:8080"), Some("8080"));
        assert_eq!(explicit_port("[::1]"), None);
        assert_eq!(explicit_port("[::1]:80:90"), Some("80:90"));
    }

    #[test]
    fn required_port_rejects_missing_port() {
        let error = validate_authority(
            "localhost",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address localhost: port is required"
        );
    }

    #[test]
    fn optional_port_rejects_zero_port() {
        let error = validate_authority(
            "example.com:0",
            "gRPC authority",
            PortPolicy::Optional,
            RawFormat::Debug,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid gRPC authority \"example.com:0\": port must be between 1 and 65535"
        );
    }

    #[test]
    fn bracketed_authority_rejects_empty_host() {
        let error = validate_authority(
            "[]:80",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address []:80: host must not be empty"
        );
    }

    #[test]
    fn bracketed_authority_requires_ipv6_host() {
        let error = validate_authority(
            "[example.com]:80",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address [example.com]:80: bracketed host must be a valid IPv6 address"
        );
    }

    #[test]
    fn authority_rejects_brackets_inside_reg_name() {
        let error = validate_authority(
            "example[::1]:80",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address example[::1]:80: brackets are only allowed around IPv6 addresses"
        );
    }

    #[test]
    fn bracketed_authority_rejects_unexpected_suffix() {
        let error = validate_authority(
            "[::1]example:80",
            "Host header",
            PortPolicy::Optional,
            RawFormat::Debug,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid Host header \"[::1]example:80\": bracketed host must be followed by :port or the end of the authority"
        );
    }

    #[test]
    fn authority_rejects_malformed_unbracketed_port_before_generic_parse_error() {
        let error = validate_authority(
            "example.com:80:90",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address example.com:80:90: port must be a valid integer"
        );
    }

    #[test]
    fn authority_rejects_malformed_bracketed_port_before_generic_parse_error() {
        let error = validate_authority(
            "[::1]:80:90",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address [::1]:80:90: port must be a valid integer"
        );
    }

    #[test]
    fn authority_rejects_path_before_port_shape() {
        let error = validate_authority(
            "127.0.0.1:50051/healthz",
            "gRPC address",
            PortPolicy::Required,
            RawFormat::Debug,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid gRPC address \"127.0.0.1:50051/healthz\": path, query, and fragment are not allowed"
        );
    }

    #[test]
    fn authority_rejects_whitespace_before_port_shape() {
        let error = validate_authority(
            "127.0.0.1:80 ",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address \"127.0.0.1:80 \": authority must not contain whitespace, control characters, backslashes, or quotes"
        );
    }

    #[test]
    fn authority_rejects_non_ascii_before_generic_parse_error() {
        let error = validate_authority(
            "\u{00E9}xample.test:80",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("authority must contain only ASCII characters")
        );
    }

    #[test]
    fn authority_rejects_unbracketed_ipv6_before_port_shape() {
        let error = validate_authority(
            "2001:db8::1:80",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address 2001:db8::1:80: IPv6 addresses must be enclosed in brackets"
        );
    }

    #[test]
    fn authority_rejects_malformed_unbracketed_ipv6_before_port_shape() {
        let error = validate_authority(
            "2001:db8::zz:80",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address 2001:db8::zz:80: IPv6 addresses must be enclosed in brackets"
        );
    }

    #[test]
    fn bracketed_authority_rejects_missing_closing_bracket() {
        let error = validate_authority(
            "[::1:80",
            "TCP address",
            PortPolicy::Required,
            RawFormat::Display,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TCP address [::1:80: bracketed host is missing closing ]"
        );
    }

    #[test]
    fn port_validation_distinguishes_non_numeric_and_out_of_range_ports() {
        assert_eq!(validate_port("bad").unwrap_err(), PortError::NotInteger);
        assert_eq!(validate_port("65536").unwrap_err(), PortError::OutOfRange);
    }
}
