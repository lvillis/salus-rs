use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

fn salus_bin() -> &'static str {
    env!("CARGO_BIN_EXE_salus")
}

fn run_salus(args: &[&str]) -> Output {
    Command::new(salus_bin())
        .args(args)
        .output()
        .expect("salus test binary must execute")
}

fn temp_file_path(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after UNIX_EPOCH")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{nanos}.tmp"))
}

fn write_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("test file must be writable");
}

#[test]
fn help_exits_zero_and_prints_to_stdout() {
    let output = run_salus(&["--help"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Container health check probe runner")
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn invalid_duration_exits_three_and_prints_to_stderr() {
    let path = temp_file_path("salus-cli-invalid-duration");
    write_file(&path, b"ready\n");

    let output = run_salus(&[
        "--timeout",
        "nope",
        "file",
        "--path",
        &path.display().to_string(),
    ]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid duration: nope"));

    let _ = fs::remove_file(path);
}

#[test]
fn success_is_silent_by_default() {
    let path = temp_file_path("salus-cli-success-silent");
    write_file(&path, b"ready\n");

    let output = run_salus(&[
        "file",
        "--path",
        &path.display().to_string(),
        "--contains",
        "ready",
    ]);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let _ = fs::remove_file(path);
}

#[test]
fn verbose_success_prints_structured_line_to_stderr() {
    let path = temp_file_path("salus cli success verbose");
    write_file(&path, b"ready\n");

    let output = run_salus(&[
        "--verbose",
        "file",
        "--path",
        &path.display().to_string(),
        "--contains",
        "ready",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("result=healthy mode=file"));
    assert!(stderr.contains(&format!("target=\"{}\"", path.display())));
    assert!(stderr.contains("detail=\"size=6B\""));

    let _ = fs::remove_file(path);
}

#[test]
fn quiet_failure_suppresses_stderr() {
    let path = temp_file_path("salus-cli-quiet-failure");
    write_file(&path, b"ready\n");

    let output = run_salus(&[
        "--quiet",
        "file",
        "--path",
        &path.display().to_string(),
        "--contains",
        "missing",
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let _ = fs::remove_file(path);
}

#[test]
fn failure_prints_message_to_stderr_by_default() {
    let path = temp_file_path("salus-cli-failure-message");
    write_file(&path, b"ready\n");

    let output = run_salus(&[
        "file",
        "--path",
        &path.display().to_string(),
        "--contains",
        "missing",
    ]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("does not contain required text \"missing\"")
    );

    let _ = fs::remove_file(path);
}
