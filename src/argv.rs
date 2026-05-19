use std::ffi::{OsStr, OsString};

use clap::{Arg, ArgAction, Command as ClapCommand, CommandFactory};

use crate::cli::Cli;

pub(crate) fn with_raw_tail(
    mut partial_args: Vec<OsString>,
    raw_tail: Vec<OsString>,
) -> Vec<OsString> {
    partial_args.extend(raw_tail);
    partial_args
}

pub(crate) fn help_or_version_args(args: &[OsString]) -> Option<Vec<OsString>> {
    let program = args.first()?;
    let root = Cli::command();
    Scanner::new(args, &root).help_or_version_args(program)
}

pub(crate) fn requests_quiet(args: &[OsString]) -> bool {
    let root = Cli::command();
    Scanner::new(args, &root).requests_quiet()
}

struct Scanner<'a> {
    args: &'a [OsString],
    root: &'a ClapCommand,
    command: Option<&'a ClapCommand>,
    index: usize,
}

impl<'a> Scanner<'a> {
    fn new(args: &'a [OsString], root: &'a ClapCommand) -> Self {
        Self {
            args,
            root,
            command: None,
            index: 1,
        }
    }

    fn help_or_version_args(mut self, program: &OsString) -> Option<Vec<OsString>> {
        while let Some(arg) = self.current_arg() {
            if arg.as_os_str() == OsStr::new("--") {
                return None;
            }

            let arg = arg.to_str()?;
            if self.skip_option_value(arg)? {
                continue;
            }

            if is_help(arg) {
                return Some(control_args(program, self.command_name(), arg));
            }
            if self.command.is_none() && is_version(arg) {
                return Some(control_args(program, None, arg));
            }
            if self.command.is_none() && arg == "help" {
                return Some(help_subcommand_args(program, &self.args[self.index + 1..]));
            }

            if self.consume_flag(arg) {
                continue;
            }

            if self.command.is_none()
                && let Some(command) = self.root.find_subcommand(arg)
            {
                if command_accepts_trailing_args(command) {
                    return self.trailing_help_or_version_args(program, command);
                }
                self.command = Some(command);
                self.index += 1;
                continue;
            }

            return None;
        }

        None
    }

    fn trailing_help_or_version_args(
        mut self,
        program: &OsString,
        command: &'a ClapCommand,
    ) -> Option<Vec<OsString>> {
        self.command = Some(command);
        self.index += 1;

        while let Some(arg) = self.current_arg() {
            if arg.as_os_str() == OsStr::new("--") {
                return None;
            }

            let arg = arg.to_str()?;
            if self.skip_option_value(arg)? {
                continue;
            }

            if is_help(arg) {
                return Some(control_args(program, Some(command.get_name()), arg));
            }

            if self.consume_flag(arg) {
                continue;
            }

            return None;
        }

        None
    }

    fn requests_quiet(mut self) -> bool {
        let mut output = OutputRequest::default();

        while let Some(arg) = self.current_arg() {
            if arg.as_os_str() == OsStr::new("--") {
                return output.quiet();
            }

            let Some(arg) = arg.to_str() else {
                return false;
            };
            match self.skip_option_value_for_quiet(arg, &output) {
                QuietSkip::Consumed => continue,
                QuietSkip::Stop(quiet) => return quiet,
                QuietSkip::NotOptionValue => {}
            }

            if output.observe_flag(arg) {
                self.index += 1;
                continue;
            }

            if self.consume_flag(arg) {
                continue;
            }

            if self.command.is_none()
                && let Some(command) = self.root.find_subcommand(arg)
            {
                if command_accepts_trailing_args(command) {
                    self.command = Some(command);
                    self.index += 1;
                    return self.trailing_requests_quiet(output);
                }
                self.command = Some(command);
                self.index += 1;
                continue;
            }

            return output.quiet();
        }

        output.quiet()
    }

    fn trailing_requests_quiet(mut self, mut output: OutputRequest) -> bool {
        while let Some(arg) = self.current_arg() {
            if arg.as_os_str() == OsStr::new("--") {
                return output.quiet();
            }

            let Some(arg) = arg.to_str() else {
                return false;
            };
            match self.skip_option_value_for_quiet(arg, &output) {
                QuietSkip::Consumed => continue,
                QuietSkip::Stop(quiet) => return quiet,
                QuietSkip::NotOptionValue => {}
            }

            if output.observe_flag(arg) {
                self.index += 1;
                continue;
            }

            if self.consume_flag(arg) {
                continue;
            }

            if is_help(arg) {
                self.index += 1;
                continue;
            }

            return output.quiet();
        }

        output.quiet()
    }

    fn current_arg(&self) -> Option<&'a OsString> {
        self.args.get(self.index)
    }

    fn command_name(&self) -> Option<&str> {
        self.command.map(ClapCommand::get_name)
    }

    fn consume_flag(&mut self, arg: &str) -> bool {
        let Some(spec) = self.option_spec(arg) else {
            return false;
        };
        if spec.takes_value {
            return false;
        }
        if spec.has_inline_value {
            return false;
        }

        self.index += 1;
        true
    }

    fn skip_option_value(&mut self, arg: &str) -> Option<bool> {
        let Some(spec) = self.option_spec(arg) else {
            return Some(false);
        };
        if !spec.takes_value {
            return Some(false);
        }

        self.index = self.next_index_after_option_value(spec)?;
        Some(true)
    }

    fn skip_option_value_for_quiet(&mut self, arg: &str, output: &OutputRequest) -> QuietSkip {
        let Some(spec) = self.option_spec(arg) else {
            return QuietSkip::NotOptionValue;
        };
        if !spec.takes_value {
            return QuietSkip::NotOptionValue;
        }

        let Some(next_index) = self.next_index_after_option_value(spec) else {
            return QuietSkip::Stop(output.quiet());
        };
        self.index = next_index;
        QuietSkip::Consumed
    }

    fn next_index_after_option_value(&self, spec: OptionSpec) -> Option<usize> {
        if spec.has_inline_value {
            return Some(self.index + 1);
        }

        let value = self.args.get(self.index + 1)?;
        if value_can_follow_option(value, spec) {
            Some(self.index + 2)
        } else {
            None
        }
    }

    fn option_spec(&self, arg: &str) -> Option<OptionSpec> {
        let (long, has_inline_value) = split_long_option(arg)?;
        let arg = self
            .command
            .and_then(|command| find_long_option(command, long))
            .or_else(|| find_long_option(self.root, long))?;

        Some(OptionSpec::from_arg(arg, has_inline_value))
    }
}

#[derive(Clone, Copy)]
struct OptionSpec {
    takes_value: bool,
    has_inline_value: bool,
    allow_hyphen_value: bool,
    allow_negative_number: bool,
}

enum QuietSkip {
    Consumed,
    Stop(bool),
    NotOptionValue,
}

impl OptionSpec {
    fn from_arg(arg: &Arg, has_inline_value: bool) -> Self {
        Self {
            takes_value: action_takes_value(arg.get_action()),
            has_inline_value,
            allow_hyphen_value: arg.is_allow_hyphen_values_set(),
            allow_negative_number: arg.is_allow_negative_numbers_set(),
        }
    }
}

#[derive(Default)]
struct OutputRequest {
    quiet: bool,
    verbose: bool,
}

impl OutputRequest {
    fn observe_flag(&mut self, arg: &str) -> bool {
        match arg {
            "--quiet" => {
                self.quiet = true;
                true
            }
            "--verbose" => {
                self.verbose = true;
                true
            }
            _ => false,
        }
    }

    fn quiet(&self) -> bool {
        self.quiet && !self.verbose
    }
}

fn split_long_option(arg: &str) -> Option<(&str, bool)> {
    let arg = arg.strip_prefix("--")?;
    let (name, has_inline_value) = match arg.split_once('=') {
        Some((name, _)) => (name, true),
        None => (arg, false),
    };
    if name.is_empty() {
        return None;
    }

    Some((name, has_inline_value))
}

fn find_long_option<'a>(command: &'a ClapCommand, long: &str) -> Option<&'a Arg> {
    command
        .get_arguments()
        .find(|arg| arg.get_long() == Some(long))
}

fn action_takes_value(action: &ArgAction) -> bool {
    matches!(action, ArgAction::Set | ArgAction::Append)
}

fn value_can_follow_option(value: &OsString, spec: OptionSpec) -> bool {
    let Some(value) = value.to_str() else {
        return true;
    };

    if value == "--" {
        return false;
    }
    !value.starts_with('-')
        || spec.allow_hyphen_value
        || (spec.allow_negative_number && is_negative_number_like(value))
}

fn is_negative_number_like(value: &str) -> bool {
    let Some(number) = value.strip_prefix('-') else {
        return false;
    };

    let mut seen_dot = false;
    let mut exponent = None;

    for (index, byte) in number.bytes().enumerate() {
        match byte {
            b'0'..=b'9' => {}
            b'.' if !seen_dot && exponent.is_none() && index > 0 => {
                seen_dot = true;
            }
            b'e' | b'E' if exponent.is_none() && index > 0 => {
                exponent = Some(index);
            }
            _ => return false,
        }
    }

    exponent.is_none_or(|index| index != number.len().saturating_sub(1))
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "-h" | "--help")
}

fn is_version(arg: &str) -> bool {
    matches!(arg, "-V" | "--version")
}

fn control_args(program: &OsString, command: Option<&str>, flag: &str) -> Vec<OsString> {
    let mut args = Vec::with_capacity(3);
    args.push(program.clone());
    if let Some(command) = command {
        args.push(OsString::from(command));
    }
    args.push(OsString::from(flag));
    args
}

fn help_subcommand_args(program: &OsString, args: &[OsString]) -> Vec<OsString> {
    let mut control_args = Vec::with_capacity(3);
    control_args.push(program.clone());
    control_args.push(OsString::from("help"));
    if let Some(command) = args.first() {
        if command.to_str().is_some_and(is_help) {
            control_args.push(OsString::from("help"));
        } else {
            control_args.push(command.clone());
        }
    }
    control_args
}

fn command_accepts_trailing_args(command: &ClapCommand) -> bool {
    command.get_positionals().any(Arg::is_trailing_var_arg_set)
}
