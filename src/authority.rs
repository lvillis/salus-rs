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

pub fn validate_authority(
    raw: &str,
    label: &str,
    port_policy: PortPolicy,
    raw_format: RawFormat,
) -> Result<Authority> {
    let authority = raw
        .parse::<Authority>()
        .map_err(|error| invalid_authority(label, raw, raw_format, error))?;

    if raw.contains('@') {
        return Err(invalid_authority(
            label,
            raw,
            raw_format,
            "user info is not allowed",
        ));
    }
    if authority.host().is_empty() {
        return Err(invalid_authority(
            label,
            raw,
            raw_format,
            "host must not be empty",
        ));
    }
    validate_bracketed_host(raw)
        .map_err(|reason| invalid_authority(label, raw, raw_format, reason))?;

    let explicit_port = explicit_port(raw);
    if let Some(port) = explicit_port {
        validate_port(port).map_err(|reason| invalid_authority(label, raw, raw_format, reason))?;
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

fn validate_bracketed_host(raw: &str) -> std::result::Result<(), &'static str> {
    let Some(rest) = raw.strip_prefix('[') else {
        if raw.contains('[') || raw.contains(']') {
            return Err("brackets are only allowed around IPv6 addresses");
        }
        return Ok(());
    };
    let Some(end) = rest.find(']') else {
        return Ok(());
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

fn validate_port(raw: &str) -> std::result::Result<u16, &'static str> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("port must be a valid integer");
    }

    let Ok(port) = raw.parse::<u16>() else {
        return Err("port must be between 1 and 65535");
    };
    if port == 0 {
        return Err("port must be between 1 and 65535");
    }

    Ok(port)
}

pub(crate) fn explicit_port(raw: &str) -> Option<&str> {
    if let Some(rest) = raw.strip_prefix('[') {
        let end = rest.find(']')?;
        return rest[end + 1..].strip_prefix(':');
    }

    raw.rsplit_once(':').map(|(_, port)| port)
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
    use super::{PortPolicy, RawFormat, explicit_port, validate_authority, validate_port};

    #[test]
    fn explicit_port_handles_ipv6_authorities() {
        assert_eq!(explicit_port("[::1]:8080"), Some("8080"));
        assert_eq!(explicit_port("[::1]"), None);
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
    fn port_validation_distinguishes_non_numeric_and_out_of_range_ports() {
        assert_eq!(
            validate_port("bad").unwrap_err(),
            "port must be a valid integer"
        );
        assert_eq!(
            validate_port("65536").unwrap_err(),
            "port must be between 1 and 65535"
        );
    }
}
