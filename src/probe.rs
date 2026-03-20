use crate::cli::Cli;
use crate::output::format_success;

#[derive(Debug)]
pub struct ProbeReport {
    pub mode: &'static str,
    pub target: String,
    pub detail: Option<String>,
    pub started: std::time::Instant,
    pub cli: Cli,
}

impl ProbeReport {
    pub fn print_and_exit_code(self) -> i32 {
        let quiet = self.cli.quiet;
        self.print_and_exit_code_with_quiet(quiet)
    }

    pub fn print_and_exit_code_with_quiet(self, quiet: bool) -> i32 {
        let elapsed = self.started.elapsed().as_millis();

        if self.cli.verbose && !quiet {
            eprintln!(
                "{}",
                format_success(self.mode, &self.target, elapsed, self.detail.as_deref())
            );
        }

        0
    }
}
