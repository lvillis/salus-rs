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

mod authority;
mod cli;
mod diagnostic;
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
    let raw_quiet_requested = requests_quiet(&raw_args);
    let args = if let Some(args) = help_or_version_args(&raw_args) {
        args
    } else {
        match env_expand::expand_argv(raw_args) {
            Ok(args) => args,
            Err(error) => return error.print_and_exit_code_with_quiet(raw_quiet_requested),
        }
    };
    let quiet_requested = raw_quiet_requested || requests_quiet(&args);

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

fn help_or_version_args(args: &[OsString]) -> Option<Vec<OsString>> {
    let program = args.first()?;
    let mut command = None;
    let mut index = 1;
    while let Some(arg) = args.get(index) {
        if arg.as_os_str() == OsStr::new("--") {
            return None;
        }

        let arg = arg.to_str()?;

        if option_takes_value(arg, command) {
            index += usize::from(!arg.contains('=')) + 1;
            continue;
        }

        if is_help(arg) {
            return Some(control_args(program, command, arg));
        }
        if command.is_none() && is_version(arg) {
            return Some(control_args(program, None, arg));
        }

        if is_bool_option(arg, command) {
            index += 1;
            continue;
        }

        if command.is_none()
            && let Some(subcommand) = subcommand_name(arg)
        {
            if subcommand == "exec" {
                return exec_help_or_version_args(program, &args[index + 1..]);
            }
            command = Some(subcommand);
            index += 1;
            continue;
        }

        return None;
    }

    None
}

fn requests_quiet(args: &[OsString]) -> bool {
    let mut command = None;
    let mut index = 1;
    while let Some(arg) = args.get(index) {
        if arg.as_os_str() == OsStr::new("--") {
            return false;
        }

        let Some(arg) = arg.to_str() else {
            return false;
        };

        if option_takes_value(arg, command) {
            index += usize::from(!arg.contains('=')) + 1;
            continue;
        }

        if arg == "--quiet" {
            return true;
        }

        if is_bool_option(arg, command) {
            index += 1;
            continue;
        }

        if command.is_none()
            && let Some(subcommand) = subcommand_name(arg)
        {
            if subcommand == "exec" {
                return exec_requests_quiet(&args[index + 1..]);
            }
            command = Some(subcommand);
            index += 1;
            continue;
        }

        return false;
    }

    false
}

fn exec_help_or_version_args(program: &OsString, args: &[OsString]) -> Option<Vec<OsString>> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if arg.as_os_str() == OsStr::new("--") {
            return None;
        }

        let arg = arg.to_str()?;

        if option_takes_value(arg, Some("exec")) {
            index += usize::from(!arg.contains('=')) + 1;
            continue;
        }

        if is_help(arg) {
            return Some(control_args(program, Some("exec"), arg));
        }

        if is_bool_option(arg, Some("exec")) {
            index += 1;
            continue;
        }

        return None;
    }

    None
}

fn exec_requests_quiet(args: &[OsString]) -> bool {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if arg.as_os_str() == OsStr::new("--") {
            return false;
        }

        let Some(arg) = arg.to_str() else {
            return false;
        };

        if option_takes_value(arg, Some("exec")) {
            index += usize::from(!arg.contains('=')) + 1;
            continue;
        }

        if arg == "--quiet" {
            return true;
        }

        if is_bool_option(arg, Some("exec")) || is_help(arg) {
            index += 1;
            continue;
        }

        return false;
    }

    false
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "-h" | "--help")
}

fn is_version(arg: &str) -> bool {
    matches!(arg, "-V" | "--version")
}

fn control_args(program: &OsString, command: Option<&'static str>, flag: &str) -> Vec<OsString> {
    let mut args = Vec::with_capacity(3);
    args.push(program.clone());
    if let Some(command) = command {
        args.push(OsString::from(command));
    }
    args.push(OsString::from(flag));
    args
}

fn subcommand_name(arg: &str) -> Option<&'static str> {
    match arg {
        "http" => Some("http"),
        "tcp" => Some("tcp"),
        #[cfg(feature = "grpc")]
        "grpc" => Some("grpc"),
        "exec" => Some("exec"),
        "file" => Some("file"),
        _ => None,
    }
}

fn option_takes_value(arg: &str, command: Option<&str>) -> bool {
    let name = arg.split_once('=').map_or(arg, |(name, _)| name);
    if matches!(name, "--timeout" | "--max-latency") {
        return true;
    }

    match command {
        Some("http") => matches!(
            name,
            "--url"
                | "--sock"
                | "--path"
                | "--method"
                | "--header"
                | "--host"
                | "--status"
                | "--header-present"
                | "--header-equals"
                | "--header-contains"
                | "--header-not-contains"
                | "--body-equals"
                | "--contains"
                | "--not-contains"
                | "--ca"
                | "--cert"
                | "--key"
                | "--server-name"
                | "--max-body"
        ),
        Some("tcp") => matches!(name, "--addr"),
        Some("grpc") => matches!(
            name,
            "--addr" | "--service" | "--authority" | "--ca" | "--cert" | "--key" | "--server-name"
        ),
        Some("exec") => matches!(
            name,
            "--exit-code" | "--stdout-contains" | "--stderr-contains" | "--max-output"
        ),
        Some("file") => matches!(
            name,
            "--path" | "--contains" | "--min-size" | "--max-size" | "--max-age" | "--max-read"
        ),
        _ => false,
    }
}

fn is_bool_option(arg: &str, command: Option<&str>) -> bool {
    if arg.contains('=') {
        return false;
    }

    if matches!(arg, "--quiet" | "--verbose") {
        return true;
    }

    match command {
        Some("http") => matches!(arg, "--insecure-skip-verify"),
        Some("grpc") => matches!(arg, "--tls" | "--insecure-skip-verify"),
        Some("file") => matches!(arg, "--readable" | "--non-empty"),
        _ => false,
    }
}
