#![forbid(unsafe_code)]

mod cli;
mod error;
mod output;
mod probe;
mod probes;
mod tls;

use clap::{CommandFactory, Parser, error::ErrorKind};
use cli::Cli;
use error::{AppError, Result};
use output::print_clap_error;
use probe::ProbeReport;

pub async fn main_entry<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            let code = match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
                _ => 3,
            };
            print_clap_error(error, code == 0);
            return code;
        }
    };

    match run(cli.clone()).await {
        Ok(report) => report.print_and_exit_code(),
        Err(error) => error.print_and_exit_code_with_quiet(cli.quiet),
    }
}

async fn run(cli: Cli) -> Result<ProbeReport> {
    if cli.timeout.is_zero() {
        return Err(AppError::invalid_config("--timeout must be greater than 0"));
    }

    let started = std::time::Instant::now();
    probes::run(&cli, started).await
}

pub fn command() -> clap::Command {
    Cli::command()
}
