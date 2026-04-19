use std::time::Duration;

use crate::output::format_success;
use crate::{
    cli::Cli,
    error::{AppError, Result},
};

#[derive(Debug, Clone, Copy)]
pub struct ProbeOptions {
    pub timeout: Duration,
    pub quiet: bool,
    pub verbose: bool,
    pub max_latency: Option<Duration>,
}

impl From<&Cli> for ProbeOptions {
    fn from(cli: &Cli) -> Self {
        Self {
            timeout: cli.timeout,
            quiet: cli.quiet,
            verbose: cli.verbose,
            max_latency: cli.max_latency,
        }
    }
}

#[derive(Debug)]
pub struct ProbeReport {
    pub mode: &'static str,
    pub target: String,
    pub detail: Option<String>,
    pub elapsed: Duration,
    pub options: ProbeOptions,
}

impl ProbeReport {
    pub fn new(
        mode: &'static str,
        target: String,
        detail: Option<String>,
        started: std::time::Instant,
        options: ProbeOptions,
    ) -> Self {
        Self {
            mode,
            target,
            detail,
            elapsed: started.elapsed(),
            options,
        }
    }

    pub fn enforce_max_latency(self) -> Result<Self> {
        if let Some(limit) = self.options.max_latency
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
        let quiet = self.options.quiet;
        self.print_and_exit_code_with_quiet(quiet)
    }

    pub fn print_and_exit_code_with_quiet(self, quiet: bool) -> i32 {
        let elapsed = self.elapsed.as_millis();

        if self.options.verbose && !quiet {
            eprintln!(
                "{}",
                format_success(self.mode, &self.target, elapsed, self.detail.as_deref())
            );
        }

        0
    }
}
