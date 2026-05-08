#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented,
        clippy::unreachable,
        clippy::unwrap_used
    )
)]

mod cli;
mod env_expand;
mod error;
mod output;
mod probe;
mod probes;
mod tls;

use std::ffi::{OsStr, OsString};

use clap::{CommandFactory, Parser, error::ErrorKind};
use cli::Cli;
use error::{AppError, Result};
use output::print_clap_error;
use probe::ProbeReport;

pub async fn main_entry<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let raw_args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let quiet_requested = requests_quiet(&raw_args);
    let args = if requests_help_or_version(&raw_args) {
        raw_args
    } else {
        match env_expand::expand_argv(raw_args) {
            Ok(args) => args,
            Err(error) => return error.print_and_exit_code_with_quiet(quiet_requested),
        }
    };

    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let code = match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => 3,
            };
            let success = code == 0;
            if success || !quiet_requested {
                print_clap_error(error, success);
            }
            return code;
        }
    };

    match run(&cli).await {
        Ok(report) => match report.enforce_max_latency() {
            Ok(report) => report.print_and_exit_code(),
            Err(error) => error.print_and_exit_code_with_quiet(cli.quiet),
        },
        Err(error) => error.print_and_exit_code_with_quiet(cli.quiet),
    }
}

async fn run(cli: &Cli) -> Result<ProbeReport> {
    if cli.timeout.is_zero() {
        return Err(AppError::invalid_config("--timeout must be greater than 0"));
    }
    probe::deadline_after(cli.timeout)?;

    if let Some(max_latency) = cli.max_latency
        && max_latency.is_zero()
    {
        return Err(AppError::invalid_config(
            "--max-latency must be greater than 0",
        ));
    }

    let started = std::time::Instant::now();
    probes::run(cli, started).await
}

pub fn command() -> clap::Command {
    Cli::command()
}

fn requests_help_or_version(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|arg| arg.as_os_str() != OsStr::new("--"))
        .any(|arg| matches!(arg.to_str(), Some("-h" | "--help" | "-V" | "--version")))
}

fn requests_quiet(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|arg| arg.as_os_str() != OsStr::new("--"))
        .any(|arg| matches!(arg.to_str(), Some("--quiet")))
}
