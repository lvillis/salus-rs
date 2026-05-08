use std::{ffi::OsString, path::PathBuf, time::Duration};

use clap::{ArgAction, Args, Parser, Subcommand};

use crate::error::{AppError, Result};

const ENV_EXPANSION_AFTER_HELP: &str = "\
Argument values support ${VAR} and ${VAR:-default} expansion before parsing.
Use $$ to keep a literal $ character in JSON-array commands.";

#[derive(Debug, Clone, Parser)]
#[command(
    name = "salus",
    version,
    about = "Container health check probe runner",
    after_help = ENV_EXPANSION_AFTER_HELP
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        default_value = "3s",
        value_name = "DURATION",
        value_parser = parse_duration,
        help = "Hard deadline for one probe"
    )]
    pub timeout: Duration,
    #[arg(
        long = "max-latency",
        global = true,
        value_name = "DURATION",
        value_parser = parse_duration,
        help = "Fail if a successful probe takes longer than this"
    )]
    pub max_latency: Option<Duration>,
    #[arg(
        long,
        global = true,
        conflicts_with = "verbose",
        help = "Suppress output"
    )]
    pub quiet: bool,
    #[arg(
        long,
        global = true,
        conflicts_with = "quiet",
        help = "Print structured success output"
    )]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    #[command(about = "Probe an HTTP or HTTPS health endpoint")]
    Http(Box<HttpArgs>),
    #[command(about = "Probe TCP connectivity to an address")]
    Tcp(TcpArgs),
    #[cfg(feature = "grpc")]
    #[command(about = "Run a gRPC health check probe")]
    Grpc(GrpcArgs),
    #[command(about = "Run a command and evaluate its exit code and output")]
    Exec(ExecArgs),
    #[command(about = "Probe file state and contents")]
    File(FileArgs),
}

#[derive(Debug, Clone, Args)]
#[command(after_help = ENV_EXPANSION_AFTER_HELP)]
pub struct HttpArgs {
    #[arg(
        long,
        value_name = "URL",
        help_heading = "Target",
        help = "HTTP or HTTPS endpoint URL to probe"
    )]
    pub url: Option<String>,
    #[arg(
        long = "sock",
        value_name = "SOCKET",
        help_heading = "Target",
        help = "Unix socket path for HTTP over UDS"
    )]
    pub sock: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        help_heading = "Target",
        help = "Request path when using --sock"
    )]
    pub path: Option<String>,
    #[arg(
        long,
        default_value = "GET",
        value_name = "METHOD",
        help_heading = "Request",
        help = "HTTP method to send"
    )]
    pub method: String,
    #[arg(
        long,
        action = ArgAction::Append,
        value_name = "NAME:VALUE",
        help_heading = "Request",
        help = "Request header to send"
    )]
    pub header: Vec<String>,
    #[arg(
        long,
        value_name = "HOST",
        help_heading = "Request",
        help = "Override the request Host header"
    )]
    pub host: Option<String>,
    #[arg(
        long,
        action = ArgAction::Append,
        value_name = "CODE|RANGE",
        help_heading = "Assertions",
        help = "Accepted status code or inclusive range"
    )]
    pub status: Vec<String>,
    #[arg(
        long = "header-present",
        action = ArgAction::Append,
        value_name = "NAME",
        help_heading = "Assertions",
        help = "Require a response header to exist"
    )]
    pub header_present: Vec<String>,
    #[arg(
        long = "header-equals",
        action = ArgAction::Append,
        value_name = "NAME:VALUE",
        help_heading = "Assertions",
        help = "Require a response header value to equal text"
    )]
    pub header_equals: Vec<String>,
    #[arg(
        long = "header-contains",
        action = ArgAction::Append,
        value_name = "NAME:TEXT",
        help_heading = "Assertions",
        help = "Require a response header value to contain text"
    )]
    pub header_contains: Vec<String>,
    #[arg(
        long = "header-not-contains",
        action = ArgAction::Append,
        value_name = "NAME:TEXT",
        help_heading = "Assertions",
        help = "Require a response header value not to contain text"
    )]
    pub header_not_contains: Vec<String>,
    #[arg(
        long = "body-equals",
        value_name = "TEXT",
        help_heading = "Assertions",
        help = "Require response body to exactly equal text"
    )]
    pub body_equals: Option<String>,
    #[arg(
        long = "contains",
        action = ArgAction::Append,
        value_name = "TEXT",
        help_heading = "Assertions",
        help = "Require response body to contain text"
    )]
    pub contains: Vec<String>,
    #[arg(
        long = "not-contains",
        action = ArgAction::Append,
        value_name = "TEXT",
        help_heading = "Assertions",
        help = "Require response body not to contain text"
    )]
    pub not_contains: Vec<String>,
    #[command(flatten)]
    pub tls: TlsArgs,
    #[arg(
        long = "max-body",
        default_value_t = 65_536,
        value_name = "BYTES",
        help_heading = "Limits",
        help = "Maximum response body bytes to read"
    )]
    pub max_body: usize,
}

#[derive(Debug, Clone, Args)]
pub struct TcpArgs {
    #[arg(
        long = "addr",
        value_name = "HOST:PORT",
        help = "TCP address to connect to"
    )]
    pub addr: String,
}

#[cfg(feature = "grpc")]
#[derive(Debug, Clone, Args)]
pub struct GrpcArgs {
    #[arg(long = "addr", value_name = "HOST:PORT", help = "gRPC server address")]
    pub addr: String,
    #[arg(long, value_name = "SERVICE", help = "gRPC health service name")]
    pub service: Option<String>,
    #[arg(long, help = "Use TLS for the gRPC connection")]
    pub tls: bool,
    #[arg(
        long,
        value_name = "AUTHORITY",
        help = "HTTP/2 authority value to send"
    )]
    pub authority: Option<String>,
    #[command(flatten)]
    pub tls_args: TlsArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ExecArgs {
    #[arg(
        long = "exit-code",
        action = ArgAction::Append,
        value_name = "CODE",
        help_heading = "Assertions",
        help = "Accepted process exit code"
    )]
    pub exit_code: Vec<i32>,
    #[arg(
        long = "stdout-contains",
        action = ArgAction::Append,
        value_name = "TEXT",
        help_heading = "Assertions",
        help = "Require stdout to contain text"
    )]
    pub stdout_contains: Vec<String>,
    #[arg(
        long = "stderr-contains",
        action = ArgAction::Append,
        value_name = "TEXT",
        help_heading = "Assertions",
        help = "Require stderr to contain text"
    )]
    pub stderr_contains: Vec<String>,
    #[arg(
        long = "max-output",
        default_value_t = 65_536,
        value_name = "BYTES",
        help_heading = "Limits",
        help = "Maximum stdout and stderr bytes to keep"
    )]
    pub max_output: usize,
    #[arg(
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "COMMAND"
    )]
    pub command: Vec<OsString>,
}

#[derive(Debug, Clone, Args)]
pub struct FileArgs {
    #[arg(long, value_name = "PATH", help = "File path to inspect")]
    pub path: PathBuf,
    #[arg(long, help = "Require the file to be readable")]
    pub readable: bool,
    #[arg(long, help = "Require the file to be non-empty")]
    pub non_empty: bool,
    #[arg(
        long = "contains",
        action = ArgAction::Append,
        value_name = "TEXT",
        help = "Require file contents to contain text"
    )]
    pub contains: Vec<String>,
    #[arg(
        long,
        value_name = "BYTES",
        help = "Require file size to be at least this many bytes"
    )]
    pub min_size: Option<u64>,
    #[arg(
        long,
        value_name = "BYTES",
        help = "Require file size to be at most this many bytes"
    )]
    pub max_size: Option<u64>,
    #[arg(
        long,
        value_name = "DURATION",
        value_parser = parse_duration,
        help = "Require file modification age to be no greater than this"
    )]
    pub max_age: Option<Duration>,
    #[arg(
        long = "max-read",
        default_value_t = 65_536,
        value_name = "BYTES",
        help = "Maximum file bytes to read for content assertions"
    )]
    pub max_read: usize,
}

#[derive(Debug, Clone, Args, Default)]
pub struct TlsArgs {
    #[arg(
        long = "ca",
        value_name = "PATH",
        help_heading = "TLS",
        help = "Additional PEM CA certificate bundle to trust"
    )]
    pub ca: Option<PathBuf>,
    #[arg(
        long = "cert",
        value_name = "PATH",
        help_heading = "TLS",
        help = "Client certificate PEM file for mTLS"
    )]
    pub cert: Option<PathBuf>,
    #[arg(
        long = "key",
        value_name = "PATH",
        help_heading = "TLS",
        help = "Client private key PEM file for mTLS"
    )]
    pub key: Option<PathBuf>,
    #[arg(
        long,
        value_name = "NAME",
        help_heading = "TLS",
        help = "TLS server name override for SNI and certificate verification"
    )]
    pub server_name: Option<String>,
    #[arg(
        long,
        help_heading = "TLS",
        help = "Skip TLS certificate and hostname verification"
    )]
    pub insecure_skip_verify: bool,
}

pub fn parse_duration(raw: &str) -> Result<Duration> {
    humantime::parse_duration(raw.trim())
        .map_err(|_| AppError::invalid_config(format!("invalid duration: {raw}")))
}
