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

mod argv;
mod authority;
mod capture;
mod cli;
mod diagnostic;
mod env_expand;
mod error;
mod output;
mod probe;
mod probes;
mod text_match;
mod tls;
mod validation;

use std::ffi::OsString;

use clap::{CommandFactory, Parser, error::ErrorKind};
use cli::Cli;
use error::Result;
use output::print_clap_error;
use probe::ProbeReport;

pub async fn main_entry<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let raw_args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let args = if let Some(args) = argv::help_or_version_args(&raw_args) {
        args
    } else {
        match env_expand::expand_argv_with_partial(raw_args) {
            Ok(args) => args,
            Err(error) => {
                let (error, partial_args, raw_tail) = error.into_parts();
                let quiet_requested =
                    argv::requests_quiet(&argv::with_raw_tail(partial_args, raw_tail));
                return error.print_and_exit_code_with_quiet(quiet_requested);
            }
        }
    };
    let quiet_requested = argv::requests_quiet(&args);

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
    validation::validate_positive_duration("--timeout", cli.timeout)?;
    probe::deadline_after(cli.timeout)?;

    if let Some(max_latency) = cli.max_latency {
        validation::validate_positive_duration("--max-latency", max_latency)?;
    }

    let started = std::time::Instant::now();
    probes::run(cli, started).await
}

pub fn command() -> clap::Command {
    Cli::command()
}
