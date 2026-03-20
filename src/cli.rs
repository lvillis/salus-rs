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
    #[command(about = "Probe an HTTP or HTTPS health endpoint")]
    Http(HttpArgs),
    #[command(about = "Probe TCP connectivity to an address")]
    Tcp(TcpArgs),
    #[command(about = "Run a gRPC health check probe")]
    Grpc(GrpcArgs),
    #[command(about = "Run a command and evaluate its exit code and output")]
    Exec(ExecArgs),
    #[command(about = "Probe file state and contents")]
    File(FileArgs),
}

#[derive(Debug, Clone, Args)]
pub struct HttpArgs {
    #[arg(long, help_heading = "Target")]
    pub url: Option<String>,
    #[arg(long = "sock", help_heading = "Target")]
    pub sock: Option<PathBuf>,
    #[arg(long, help_heading = "Target")]
    pub path: Option<String>,
    #[arg(long, default_value = "GET", help_heading = "Request")]
    pub method: String,
    #[arg(long, action = ArgAction::Append, help_heading = "Request")]
    pub header: Vec<String>,
    #[arg(long, help_heading = "Request")]
    pub host: Option<String>,
    #[arg(long, action = ArgAction::Append, help_heading = "Assertions")]
    pub status: Vec<String>,
    #[arg(long = "header-contains", action = ArgAction::Append, help_heading = "Assertions")]
    pub header_contains: Vec<String>,
    #[arg(
        long = "header-not-contains",
        action = ArgAction::Append,
        help_heading = "Assertions"
    )]
    pub header_not_contains: Vec<String>,
    #[arg(long = "contains", action = ArgAction::Append, help_heading = "Assertions")]
    pub contains: Vec<String>,
    #[arg(long = "not-contains", action = ArgAction::Append, help_heading = "Assertions")]
    pub not_contains: Vec<String>,
    #[command(flatten)]
    pub tls: TlsArgs,
    #[arg(long = "max-body", default_value_t = 65_536, help_heading = "Limits")]
    pub max_body: usize,
}

#[derive(Debug, Clone, Args)]
pub struct TcpArgs {
    #[arg(long = "addr")]
    pub addr: String,
}

#[derive(Debug, Clone, Args)]
pub struct GrpcArgs {
    #[arg(long = "addr")]
    pub addr: String,
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
    #[arg(long = "ok-code", action = ArgAction::Append)]
    pub ok_code: Vec<i32>,
    #[arg(long = "out-contains", action = ArgAction::Append)]
    pub out_contains: Vec<String>,
    #[arg(long = "err-contains", action = ArgAction::Append)]
    pub err_contains: Vec<String>,
    #[arg(long = "max-out", default_value_t = 65_536)]
    pub max_out: usize,
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
    #[arg(long = "max-read", default_value_t = 65_536)]
    pub max_read: usize,
}

#[derive(Debug, Clone, Args, Default)]
pub struct TlsArgs {
    #[arg(long = "ca", help_heading = "TLS")]
    pub ca: Option<PathBuf>,
    #[arg(long = "cert", help_heading = "TLS")]
    pub cert: Option<PathBuf>,
    #[arg(long = "key", help_heading = "TLS")]
    pub key: Option<PathBuf>,
    #[arg(long, help_heading = "TLS")]
    pub server_name: Option<String>,
    #[arg(long, help_heading = "TLS")]
    pub insecure_skip_verify: bool,
}

pub fn parse_duration(raw: &str) -> Result<Duration> {
    humantime::parse_duration(raw)
        .map_err(|_| AppError::invalid_config(format!("invalid duration: {raw}")))
}
