pub mod exec;
pub mod file;
pub mod grpc;
pub mod http;
pub mod tcp;

use crate::{
    cli::{Cli, Command},
    error::Result,
    probe::ProbeReport,
};

pub async fn run(cli: &Cli, started: std::time::Instant) -> Result<ProbeReport> {
    match &cli.command {
        Command::Http(args) => http::run(cli.clone(), args.clone(), started).await,
        Command::Tcp(args) => tcp::run(cli.clone(), args.clone(), started).await,
        Command::Grpc(args) => grpc::run(cli.clone(), args.clone(), started).await,
        Command::Exec(args) => exec::run(cli.clone(), args.clone(), started).await,
        Command::File(args) => file::run(cli.clone(), args.clone(), started).await,
    }
}
