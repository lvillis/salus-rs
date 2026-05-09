use std::{ffi::OsString, process::Stdio, time::Duration};

#[cfg(unix)]
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::watch,
    task::JoinHandle,
};

use crate::{
    cli::ExecArgs,
    error::{AppError, Result},
    probe::{ProbeOptions, ProbeReport, deadline_after},
};

pub async fn run(
    options: ProbeOptions,
    args: &ExecArgs,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    if args.max_output == 0
        && (!args.stdout_contains.is_empty() || !args.stderr_contains.is_empty())
    {
        return Err(AppError::invalid_config(
            "--max-output must be greater than 0 when output assertions are used",
        ));
    }

    validate_non_empty_assertions("--stdout-contains", &args.stdout_contains)?;
    validate_non_empty_assertions("--stderr-contains", &args.stderr_contains)?;

    let success_codes: &[i32] = if args.exit_code.is_empty() {
        &[0]
    } else {
        &args.exit_code
    };

    for code in success_codes {
        if !(0..=255).contains(code) {
            return Err(AppError::invalid_config(format!(
                "invalid exit code {code}, expected 0..=255"
            )));
        }
    }

    let program = args
        .command
        .first()
        .ok_or_else(|| AppError::invalid_config("missing command to execute"))?;
    if program.is_empty() {
        return Err(AppError::invalid_config("command must not be empty"));
    }

    let command_label = os_string_lossy(program);
    let capture_stdout = !args.stdout_contains.is_empty();
    let capture_stderr = !args.stderr_contains.is_empty();

    let mut command = Command::new(program);
    command.args(args.command.iter().skip(1));
    command.kill_on_drop(true);
    configure_process_group(&mut command);
    command.stdin(Stdio::null());
    command.stdout(if capture_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stderr(if capture_stderr {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            AppError::invalid_config(format!("command {:?} was not found", program))
        }
        std::io::ErrorKind::PermissionDenied => {
            AppError::invalid_config(format!("command {:?} is not executable", program))
        }
        _ => AppError::internal(format!("failed to spawn {:?}: {error}", program)),
    })?;
    let process_group = child_process_group(&child);

    let mut stdout_task = if capture_stdout {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::internal("failed to capture child stdout"))?;
        Some(spawn_output_capture(
            stdout,
            args.max_output,
            args.stdout_contains.clone(),
        ))
    } else {
        None
    };
    let mut stderr_task = if capture_stderr {
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::internal("failed to capture child stderr"))?;
        Some(spawn_output_capture(
            stderr,
            args.max_output,
            args.stderr_contains.clone(),
        ))
    } else {
        None
    };

    let deadline = deadline_after(options.timeout)?;
    let status = match tokio::time::timeout(options.timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            terminate_child(&mut child, process_group);
            abort_output_tasks(&stdout_task, &stderr_task);
            return Err(AppError::internal(format!(
                "failed waiting for child: {error}"
            )));
        }
        Err(_) => {
            terminate_child(&mut child, process_group);
            abort_output_tasks(&stdout_task, &stderr_task);
            let _reaper = tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Err(command_timeout_error(&command_label, options.timeout));
        }
    };
    let _process_group_cleanup = ProcessGroupCleanup::new(process_group);

    let code = match status.code() {
        Some(code) => code,
        None => {
            abort_output_tasks(&stdout_task, &stderr_task);
            return Err(AppError::failure(format!(
                "command {command_label} terminated without an exit code{}",
                termination_suffix(&status)
            )));
        }
    };
    if !success_codes.contains(&code) {
        abort_output_tasks(&stdout_task, &stderr_task);
        return Err(AppError::failure(format!(
            "command {command_label} exited with {code}"
        )));
    }

    let stdout = match await_output_task(
        &mut stdout_task,
        "stdout",
        deadline,
        options.timeout,
        &command_label,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            abort_output_task(&stderr_task);
            return Err(error);
        }
    };
    let stderr = await_output_task(
        &mut stderr_task,
        "stderr",
        deadline,
        options.timeout,
        &command_label,
    )
    .await?;

    for needle in &args.stdout_contains {
        if !contains_bytes(&stdout.bytes, needle) {
            if stdout.cannot_prove_absence() {
                return Err(output_limit_error(
                    "stdout",
                    &command_label,
                    args.max_output,
                    needle,
                ));
            }
            return Err(AppError::failure(format!(
                "stdout of {command_label} does not contain required text {:?}",
                needle
            )));
        }
    }

    for needle in &args.stderr_contains {
        if !contains_bytes(&stderr.bytes, needle) {
            if stderr.cannot_prove_absence() {
                return Err(output_limit_error(
                    "stderr",
                    &command_label,
                    args.max_output,
                    needle,
                ));
            }
            return Err(AppError::failure(format!(
                "stderr of {command_label} does not contain required text {:?}",
                needle
            )));
        }
    }

    Ok(ProbeReport::new(
        "exec",
        command_label,
        Some(format!("exit_code={code}")),
        started,
        options,
    ))
}

#[derive(Clone, Debug, Default)]
struct BufferedOutput {
    bytes: Vec<u8>,
    truncated: bool,
    matched: bool,
    limit_reached: bool,
    complete: bool,
}

struct OutputCapture {
    task: OutputTask,
    snapshot: watch::Receiver<BufferedOutput>,
}

type OutputTask = JoinHandle<Result<BufferedOutput>>;

fn validate_non_empty_assertions(flag: &str, values: &[String]) -> Result<()> {
    if values.iter().any(String::is_empty) {
        return Err(AppError::invalid_config(format!(
            "{flag} must not be empty"
        )));
    }

    Ok(())
}

async fn await_output_task(
    capture: &mut Option<OutputCapture>,
    stream_name: &'static str,
    deadline: tokio::time::Instant,
    timeout: Duration,
    command_label: &str,
) -> Result<BufferedOutput> {
    let Some(capture) = capture else {
        return Ok(BufferedOutput::default());
    };

    loop {
        if capture.task.is_finished() {
            return (&mut capture.task).await.map_err(|error| {
                AppError::internal(format!("{stream_name} task failed: {error}"))
            })?;
        }

        if capture.snapshot.borrow().matched {
            let snapshot = capture.snapshot.borrow().clone();
            capture.task.abort();
            return Ok(snapshot);
        }
        if capture.snapshot.borrow().truncated {
            let snapshot = capture.snapshot.borrow().clone();
            capture.task.abort();
            return Ok(snapshot);
        }
        if capture.snapshot.borrow().limit_reached {
            let snapshot = capture.snapshot.borrow().clone();
            capture.task.abort();
            return Ok(snapshot);
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            capture.task.abort();
            return Err(command_timeout_error(command_label, timeout));
        }

        let timeout_sleep = tokio::time::sleep(remaining);
        tokio::pin!(timeout_sleep);

        tokio::select! {
            join_result = &mut capture.task => {
                return join_result
                    .map_err(|error| AppError::internal(format!("{stream_name} task failed: {error}")))?;
            }
            changed = capture.snapshot.changed() => {
                if changed.is_err() {
                    continue;
                }
            }
            () = &mut timeout_sleep => {
                capture.task.abort();
                return Err(command_timeout_error(command_label, timeout));
            }
        }
    }
}

fn abort_output_tasks(stdout: &Option<OutputCapture>, stderr: &Option<OutputCapture>) {
    abort_output_task(stdout);
    abort_output_task(stderr);
}

fn abort_output_task(capture: &Option<OutputCapture>) {
    if let Some(capture) = capture {
        capture.task.abort();
    }
}

fn command_timeout_error(command_label: &str, timeout: Duration) -> AppError {
    AppError::failure(format!(
        "command {command_label} timed out after {}",
        humantime::format_duration(timeout)
    ))
}

fn output_limit_error(
    stream_name: &str,
    command_label: &str,
    max_output: usize,
    needle: &str,
) -> AppError {
    AppError::failure(format!(
        "{stream_name} of {command_label} reached --max-output {max_output} bytes, cannot prove required text {needle:?}"
    ))
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn child_process_group(child: &tokio::process::Child) -> Option<Pid> {
    child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
}

#[cfg(not(unix))]
fn child_process_group(_child: &tokio::process::Child) -> Option<()> {
    None
}

#[cfg(unix)]
fn terminate_child(child: &mut tokio::process::Child, process_group: Option<Pid>) {
    terminate_process_group(process_group);
    let _ = child.start_kill();
}

#[cfg(not(unix))]
fn terminate_child(child: &mut tokio::process::Child, _process_group: Option<()>) {
    let _ = child.start_kill();
}

#[cfg(unix)]
fn terminate_process_group(process_group: Option<Pid>) {
    if let Some(process_group) = process_group {
        let _ = kill_process_group(process_group, Signal::KILL);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_process_group: Option<()>) {}

#[cfg(unix)]
struct ProcessGroupCleanup {
    process_group: Option<Pid>,
}

#[cfg(unix)]
impl ProcessGroupCleanup {
    fn new(process_group: Option<Pid>) -> Self {
        Self { process_group }
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        terminate_process_group(self.process_group.take());
    }
}

#[cfg(not(unix))]
struct ProcessGroupCleanup;

#[cfg(not(unix))]
impl ProcessGroupCleanup {
    fn new(_process_group: Option<()>) -> Self {
        Self
    }
}

fn spawn_output_capture<R>(reader: R, limit: usize, required_texts: Vec<String>) -> OutputCapture
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (snapshot_tx, snapshot) = watch::channel(BufferedOutput::default());
    let task = tokio::spawn(read_limited_with_snapshots(
        reader,
        limit,
        required_texts,
        snapshot_tx,
    ));

    OutputCapture { task, snapshot }
}

#[cfg(test)]
async fn read_limited<R>(
    reader: R,
    limit: usize,
    required_texts: Vec<String>,
) -> Result<BufferedOutput>
where
    R: AsyncRead + Unpin,
{
    let (snapshot_tx, _snapshot) = watch::channel(BufferedOutput::default());
    read_limited_with_snapshots(reader, limit, required_texts, snapshot_tx).await
}

async fn read_limited_with_snapshots<R>(
    mut reader: R,
    limit: usize,
    required_texts: Vec<String>,
    snapshots: watch::Sender<BufferedOutput>,
) -> Result<BufferedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut collected = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut truncated = false;
    let mut matched = false;
    let mut limit_reached = false;

    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| AppError::internal(format!("failed reading child output: {error}")))?;
        if read == 0 {
            break;
        }

        if collected.len() < limit {
            let remaining = limit - collected.len();
            if read > remaining {
                truncated = true;
            }
            let slice = &buffer[..read.min(remaining)];
            collected.extend_from_slice(slice);
        } else if read > 0 {
            truncated = true;
        }

        matched = !required_texts.is_empty() && contains_all(&collected, &required_texts);
        limit_reached = !required_texts.is_empty() && collected.len() == limit;
        let _ = snapshots.send(BufferedOutput {
            bytes: collected.clone(),
            truncated,
            matched,
            limit_reached,
            complete: false,
        });
    }

    let output = BufferedOutput {
        bytes: collected,
        truncated,
        matched,
        limit_reached,
        complete: true,
    };
    let _ = snapshots.send(output.clone());
    Ok(output)
}

impl BufferedOutput {
    fn cannot_prove_absence(&self) -> bool {
        self.truncated || (self.limit_reached && !self.complete)
    }
}

fn contains_all(bytes: &[u8], required_texts: &[String]) -> bool {
    required_texts
        .iter()
        .all(|required| contains_bytes(bytes, required))
}

fn contains_bytes(bytes: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    needle.is_empty() || bytes.windows(needle.len()).any(|window| window == needle)
}

fn os_string_lossy(value: &OsString) -> String {
    value.to_string_lossy().into_owned()
}

#[cfg(unix)]
fn termination_suffix(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;

    match status.signal() {
        Some(signal) => format!(" (signal {signal})"),
        None => String::new(),
    }
}

#[cfg(not(unix))]
fn termination_suffix(_status: &std::process::ExitStatus) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{read_limited, spawn_output_capture};
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn read_limited_marks_truncated_output() {
        let output = read_limited(&b"abcdef"[..], 3, Vec::new()).await.unwrap();

        assert_eq!(output.bytes, b"abc");
        assert!(output.truncated);
        assert!(output.complete);
    }

    #[tokio::test]
    async fn read_limited_leaves_short_output_untruncated() {
        let output = read_limited(&b"abc"[..], 3, Vec::new()).await.unwrap();

        assert_eq!(output.bytes, b"abc");
        assert!(!output.truncated);
        assert!(output.complete);
    }

    #[tokio::test]
    async fn complete_output_at_limit_proves_absence() {
        let output = read_limited(&b"abcd"[..], 4, vec!["ready".to_string()])
            .await
            .unwrap();

        assert_eq!(output.bytes, b"abcd");
        assert!(output.limit_reached);
        assert!(output.complete);
        assert!(!output.cannot_prove_absence());
    }

    #[tokio::test]
    async fn output_capture_reports_match_before_eof() {
        let (mut writer, reader) = tokio::io::duplex(16);
        let mut capture = spawn_output_capture(reader, 65_536, vec!["ok".to_string()]);
        writer.write_all(b"ok").await.unwrap();

        tokio::time::timeout(Duration::from_millis(100), capture.snapshot.changed())
            .await
            .unwrap()
            .unwrap();
        let output = capture.snapshot.borrow().clone();
        capture.task.abort();

        assert_eq!(output.bytes, b"ok");
        assert!(!output.truncated);
        assert!(output.matched);
        assert!(!output.complete);
    }
}
