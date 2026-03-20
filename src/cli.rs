use std::{ffi::OsString, path::PathBuf, time::Duration};

use clap::{ArgAction, Args, Parser, Subcommand};

use crate::error::{AppError, Result};

#[derive(Debug, Clone, Parser)]
#[command(name = "salus", version, about = "Container health check probe runner")]
pub struct Cli {
    #[arg(long, global = true, default_value = "3s", value_parser = parse_duration)]
    pub timeout: Duration,
    #[arg(long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
    #[arg(long, global = true, conflicts_with = "quiet")]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    Http(HttpArgs),
    Tcp(TcpArgs),
    Grpc(GrpcArgs),
    Exec(ExecArgs),
    File(FileArgs),
}

#[derive(Debug, Clone, Args)]
pub struct HttpArgs {
    #[arg(long)]
    pub url: Option<String>,
    #[arg(long)]
    pub unix_socket: Option<PathBuf>,
    #[arg(long)]
    pub path: Option<String>,
    #[arg(long, action = ArgAction::Append)]
    pub status: Vec<String>,
    #[arg(long, default_value = "GET")]
    pub method: String,
    #[arg(long = "request-header", action = ArgAction::Append)]
    pub header: Vec<String>,
    #[arg(long)]
    pub host_header: Option<String>,
    #[arg(long = "response-header-contains", action = ArgAction::Append)]
    pub response_header_contains: Vec<String>,
    #[arg(long = "response-header-not-contains", action = ArgAction::Append)]
    pub response_header_not_contains: Vec<String>,
    #[arg(long = "body-contains", action = ArgAction::Append)]
    pub body_contains: Vec<String>,
    #[arg(long = "body-not-contains", action = ArgAction::Append)]
    pub body_not_contains: Vec<String>,
    #[arg(long, default_value_t = 65_536)]
    pub max_body_bytes: usize,
    #[command(flatten)]
    pub tls: TlsArgs,
}

#[derive(Debug, Clone, Args)]
pub struct TcpArgs {
    #[arg(long)]
    pub address: String,
}

#[derive(Debug, Clone, Args)]
pub struct GrpcArgs {
    #[arg(long)]
    pub address: String,
    #[arg(long)]
    pub service: Option<String>,
    #[arg(long)]
    pub tls: bool,
    #[arg(long)]
    pub authority: Option<String>,
    #[command(flatten)]
    pub tls_args: TlsArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ExecArgs {
    #[arg(long = "success-exit-code", action = ArgAction::Append)]
    pub success_exit_code: Vec<i32>,
    #[arg(long = "stdout-contains", action = ArgAction::Append)]
    pub stdout_contains: Vec<String>,
    #[arg(long = "stderr-contains", action = ArgAction::Append)]
    pub stderr_contains: Vec<String>,
    #[arg(long, default_value_t = 65_536)]
    pub max_output_bytes: usize,
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<OsString>,
}

#[derive(Debug, Clone, Args)]
pub struct FileArgs {
    #[arg(long)]
    pub path: PathBuf,
    #[arg(long)]
    pub readable: bool,
    #[arg(long)]
    pub non_empty: bool,
    #[arg(long = "contains", action = ArgAction::Append)]
    pub contains: Vec<String>,
    #[arg(long)]
    pub min_size: Option<u64>,
    #[arg(long)]
    pub max_size: Option<u64>,
    #[arg(long, value_parser = parse_duration)]
    pub max_age: Option<Duration>,
    #[arg(long, default_value_t = 65_536)]
    pub max_read_bytes: usize,
}

#[derive(Debug, Clone, Args, Default)]
pub struct TlsArgs {
    #[arg(long)]
    pub ca_file: Option<PathBuf>,
    #[arg(long)]
    pub client_cert: Option<PathBuf>,
    #[arg(long)]
    pub client_key: Option<PathBuf>,
    #[arg(long)]
    pub server_name: Option<String>,
    #[arg(long)]
    pub insecure_skip_verify: bool,
}

pub fn parse_duration(raw: &str) -> Result<Duration> {
    humantime::parse_duration(raw)
        .map_err(|_| AppError::invalid_config(format!("invalid duration: {raw}")))
}
