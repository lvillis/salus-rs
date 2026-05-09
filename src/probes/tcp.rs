use std::time::Duration;

use hyper::http::uri::Authority;
use tokio::net::{TcpStream, lookup_host};

use crate::{
    cli::TcpArgs,
    error::{AppError, Result},
    probe::{ProbeOptions, ProbeReport, deadline_after},
};

pub async fn run(
    options: ProbeOptions,
    args: &TcpArgs,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    let timeout = options.timeout;
    if args.addr.is_empty() {
        return Err(AppError::invalid_config("--addr must not be empty"));
    }
    validate_tcp_addr(&args.addr)?;

    let result = tokio::time::timeout(timeout, async {
        let addrs = lookup_host(&args.addr)
            .await
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::InvalidInput => {
                    AppError::invalid_config(format!("invalid TCP address {}: {error}", args.addr))
                }
                _ => AppError::failure(format!("failed to resolve {}: {error}", args.addr)),
            })?
            .collect::<Vec<_>>();

        if addrs.is_empty() {
            return Err(AppError::failure(format!(
                "no TCP addresses resolved for {}",
                args.addr
            )));
        }

        let deadline = deadline_after(timeout)?;
        let mut last_error = None;

        for (index, addr) in addrs.iter().copied().enumerate() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            let attempts_left = addrs.len().saturating_sub(index);
            let per_attempt = per_attempt_timeout(remaining, attempts_left);

            match tokio::time::timeout(per_attempt, TcpStream::connect(addr)).await {
                Ok(Ok(_stream)) => {
                    return Ok::<_, AppError>(ProbeReport::new(
                        "tcp",
                        args.addr.clone(),
                        Some("connected".to_string()),
                        started,
                        options,
                    ));
                }
                Ok(Err(error)) => {
                    last_error = Some(error.to_string());
                }
                Err(_) => {
                    last_error = Some(format!("connection to {addr} timed out"));
                }
            }
        }

        Err(AppError::failure(match last_error {
            Some(error) => format!("failed to connect to {}: {error}", args.addr),
            None => format!("timed out connecting to {}", args.addr),
        }))
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(AppError::failure(format!(
            "TCP probe timed out after {}",
            humantime::format_duration(timeout)
        ))),
    }
}

fn validate_tcp_addr(raw: &str) -> Result<()> {
    let authority = raw
        .parse::<Authority>()
        .map_err(|error| AppError::invalid_config(format!("invalid TCP address {raw}: {error}")))?;
    if raw.contains('@') {
        return Err(AppError::invalid_config(format!(
            "invalid TCP address {raw}: user info is not allowed"
        )));
    }
    if authority.host().is_empty() {
        return Err(AppError::invalid_config(format!(
            "invalid TCP address {raw}: host must not be empty"
        )));
    }

    let port_part = authority
        .as_str()
        .strip_prefix(authority.host())
        .unwrap_or_default();
    if port_part.is_empty() {
        return Err(AppError::invalid_config(format!(
            "invalid TCP address {raw}: port is required"
        )));
    }
    let Some(port) = authority.port_u16() else {
        return Err(AppError::invalid_config(format!(
            "invalid TCP address {raw}: port must be a valid integer"
        )));
    };
    if port == 0 {
        return Err(AppError::invalid_config(format!(
            "invalid TCP address {raw}: port must be between 1 and 65535"
        )));
    }

    Ok(())
}

fn per_attempt_timeout(remaining: Duration, attempts_left: usize) -> Duration {
    let attempts_left = attempts_left.max(1) as u32;
    remaining
        .checked_div(attempts_left)
        .unwrap_or(Duration::from_millis(1))
        .max(Duration::from_millis(1))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::per_attempt_timeout;

    #[test]
    fn per_attempt_timeout_divides_remaining_budget() {
        assert_eq!(
            per_attempt_timeout(Duration::from_secs(3), 3),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn per_attempt_timeout_returns_full_budget_for_last_address() {
        assert_eq!(
            per_attempt_timeout(Duration::from_millis(750), 1),
            Duration::from_millis(750)
        );
    }

    #[test]
    fn per_attempt_timeout_keeps_a_minimum_of_one_millisecond() {
        assert_eq!(
            per_attempt_timeout(Duration::from_nanos(1), 8),
            Duration::from_millis(1)
        );
    }
}
