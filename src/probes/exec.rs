use std::{ffi::OsString, process::Stdio};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

use crate::{
    cli::{Cli, ExecArgs},
    error::{AppError, Result},
    probe::ProbeReport,
};

pub async fn run(cli: Cli, args: ExecArgs, started: std::time::Instant) -> Result<ProbeReport> {
    if args.max_output == 0
        && (!args.stdout_contains.is_empty() || !args.stderr_contains.is_empty())
    {
        return Err(AppError::invalid_config(
            "--max-output must be greater than 0 when output assertions are used",
        ));
    }

    let success_codes = if args.exit_code.is_empty() {
        vec![0]
    } else {
        args.exit_code.clone()
    };

    for code in &success_codes {
        if !(0..=255).contains(code) {
            return Err(AppError::invalid_config(format!(
                "invalid exit code {code}, expected 0..=255"
            )));
        }
    }

    let program = args
        .command
        .first()
        .ok_or_else(|| AppError::invalid_config("missing command to execute"))?
        .clone();
    let command_label = os_string_lossy(&program);

    let mut command = Command::new(&program);
    command.args(args.command.iter().skip(1));
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            AppError::invalid_config(format!("command {:?} was not found", program))
        }
        std::io::ErrorKind::PermissionDenied => {
            AppError::invalid_config(format!("command {:?} is not executable", program))
        }
        _ => AppError::internal(format!("failed to spawn {:?}: {error}", program)),
    })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::internal("failed to capture child stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::internal("failed to capture child stderr"))?;

    let stdout_task = tokio::spawn(read_limited(stdout, args.max_output));
    let stderr_task = tokio::spawn(read_limited(stderr, args.max_output));

    let status = match tokio::time::timeout(cli.timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            return Err(AppError::internal(format!(
                "failed waiting for child: {error}"
            )));
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(AppError::failure(format!(
                "command {command_label} timed out after {}",
                humantime::format_duration(cli.timeout)
            )));
        }
    };

    let stdout_bytes = stdout_task
        .await
        .map_err(|error| AppError::internal(format!("stdout task failed: {error}")))??;
    let stderr_bytes = stderr_task
        .await
        .map_err(|error| AppError::internal(format!("stderr task failed: {error}")))??;

    let code = match status.code() {
        Some(code) => code,
        None => {
            return Err(AppError::failure(format!(
                "command {command_label} terminated without an exit code{}",
                termination_suffix(&status)
            )));
        }
    };
    if !success_codes.contains(&code) {
        return Err(AppError::failure(format!(
            "command {command_label} exited with {code}"
        )));
    }

    let stdout_text = String::from_utf8_lossy(&stdout_bytes);
    for needle in &args.stdout_contains {
        if !stdout_text.contains(needle) {
            return Err(AppError::failure(format!(
                "stdout of {command_label} does not contain required text {:?}",
                needle
            )));
        }
    }

    let stderr_text = String::from_utf8_lossy(&stderr_bytes);
    for needle in &args.stderr_contains {
        if !stderr_text.contains(needle) {
            return Err(AppError::failure(format!(
                "stderr of {command_label} does not contain required text {:?}",
                needle
            )));
        }
    }

    Ok(ProbeReport {
        mode: "exec",
        target: command_label,
        detail: Some(format!("exit_code={code}")),
        started,
        cli,
    })
}

async fn read_limited<R>(mut reader: R, limit: usize) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut collected = Vec::new();
    let mut buffer = [0_u8; 4096];

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
            let slice = &buffer[..read.min(remaining)];
            collected.extend_from_slice(slice);
        }
    }

    Ok(collected)
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
