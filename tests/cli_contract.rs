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

fn run_salus_with_env(args: &[&str], envs: &[(&str, Option<&str>)]) -> Output {
    let mut command = Command::new(salus_bin());
    command.args(args);

    for (name, value) in envs {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }

    command
        .output()
        .expect("salus test binary must execute with custom environment")
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
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("Container health check probe runner"));
    assert!(stdout.contains("http  Probe an HTTP or HTTPS health endpoint"));
    assert!(stdout.contains("tcp   Probe TCP connectivity to an address"));
    assert!(stdout.contains("grpc  Run a gRPC health check probe"));
    assert!(stdout.contains("exec  Run a command and evaluate its exit code and output"));
    assert!(stdout.contains("file  Probe file state and contents"));
    assert!(output.stderr.is_empty());
}

#[test]
fn http_help_groups_options_by_concern() {
    let output = run_salus(&["http", "--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let target = stdout
        .find("Target:")
        .expect("http help must include Target");
    let request = stdout
        .find("Request:")
        .expect("http help must include Request");
    let assertions = stdout
        .find("Assertions:")
        .expect("http help must include Assertions");
    let tls = stdout.find("TLS:").expect("http help must include TLS");
    let limits = stdout
        .find("Limits:")
        .expect("http help must include Limits");

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(target < request);
    assert!(request < assertions);
    assert!(assertions < tls);
    assert!(tls < limits);
    assert!(stdout.contains("--header <HEADER>"));
    assert!(stdout.contains("--contains <CONTAINS>"));
    assert!(stdout.contains("--not-contains <NOT_CONTAINS>"));
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
fn expands_environment_variables_before_cli_parsing() {
    let path = temp_file_path("salus-cli-env-expansion");
    write_file(&path, b"ready\n");

    let output = run_salus_with_env(
        &[
            "--timeout",
            "${SALUS_TEST_TIMEOUT}",
            "file",
            "--path",
            &path.display().to_string(),
            "--contains",
            "${SALUS_TEST_NEEDLE}",
        ],
        &[
            ("SALUS_TEST_TIMEOUT", Some("1s")),
            ("SALUS_TEST_NEEDLE", Some("ready")),
        ],
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let _ = fs::remove_file(path);
}

#[test]
fn missing_environment_variable_exits_three() {
    let path = temp_file_path("salus-cli-missing-env");
    write_file(&path, b"ready\n");

    let output = run_salus_with_env(
        &[
            "file",
            "--path",
            &path.display().to_string(),
            "--contains",
            "${SALUS_TEST_MISSING_ENV}",
        ],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("environment variable SALUS_TEST_MISSING_ENV is not set")
    );

    let _ = fs::remove_file(path);
}

#[test]
fn environment_variable_defaults_are_supported() {
    let path = temp_file_path("salus-cli-env-default");
    write_file(&path, b"ready\n");

    let output = run_salus_with_env(
        &[
            "file",
            "--path",
            &path.display().to_string(),
            "--contains",
            "${SALUS_TEST_DEFAULT_NEEDLE:-ready}",
        ],
        &[("SALUS_TEST_DEFAULT_NEEDLE", None)],
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

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
