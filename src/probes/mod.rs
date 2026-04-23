pub mod exec;
pub mod file;
#[cfg(feature = "grpc")]
pub mod grpc;
pub mod http;
pub mod tcp;

use crate::{
    cli::{Cli, Command},
    error::Result,
    probe::{ProbeOptions, ProbeReport},
};

pub async fn run(cli: &Cli, started: std::time::Instant) -> Result<ProbeReport> {
    let options = ProbeOptions::from(cli);

    match &cli.command {
        Command::Http(args) => http::run(options, args, started).await,
        Command::Tcp(args) => tcp::run(options, args, started).await,
        #[cfg(feature = "grpc")]
        Command::Grpc(args) => grpc::run(options, args, started).await,
        Command::Exec(args) => exec::run(options, args, started).await,
        Command::File(args) => file::run(options, args, started).await,
    }
}
