use std::time::Duration;

use crate::output::format_success;
use crate::{
    cli::Cli,
    error::{AppError, Result},
};

#[derive(Debug)]
pub struct ProbeReport {
    pub mode: &'static str,
    pub target: String,
    pub detail: Option<String>,
    pub elapsed: Duration,
    pub cli: Cli,
}

impl ProbeReport {
    pub fn new(
        mode: &'static str,
        target: String,
        detail: Option<String>,
        started: std::time::Instant,
        cli: Cli,
    ) -> Self {
        Self {
            mode,
            target,
            detail,
            elapsed: started.elapsed(),
            cli,
        }
    }

    pub fn enforce_max_latency(self) -> Result<Self> {
        if let Some(limit) = self.cli.max_latency
            && self.elapsed > limit
        {
            return Err(AppError::failure(format!(
                "{} probe to {} exceeded --max-latency {} (observed {})",
                self.mode,
                self.target,
                humantime::format_duration(limit),
                humantime::format_duration(self.elapsed)
            )));
        }

        Ok(self)
    }

    pub fn print_and_exit_code(self) -> i32 {
        let quiet = self.cli.quiet;
        self.print_and_exit_code_with_quiet(quiet)
    }

    pub fn print_and_exit_code_with_quiet(self, quiet: bool) -> i32 {
        let elapsed = self.elapsed.as_millis();

        if self.cli.verbose && !quiet {
            eprintln!(
                "{}",
                format_success(self.mode, &self.target, elapsed, self.detail.as_deref())
            );
        }

        0
    }
}
