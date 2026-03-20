use std::time::Duration;

use tokio::net::{TcpStream, lookup_host};

use crate::{
    cli::{Cli, TcpArgs},
    error::{AppError, Result},
    probe::ProbeReport,
};

pub async fn run(cli: Cli, args: TcpArgs, started: std::time::Instant) -> Result<ProbeReport> {
    let timeout = cli.timeout;
    let target = args.addr.clone();

    let result = tokio::time::timeout(timeout, async move {
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

        let deadline = tokio::time::Instant::now() + timeout;
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
                    return Ok::<_, AppError>(ProbeReport {
                        mode: "tcp",
                        target: target.clone(),
                        detail: Some("connected".to_string()),
                        started,
                        cli,
                    });
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
