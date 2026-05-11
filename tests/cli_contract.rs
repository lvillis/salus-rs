use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
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

#[cfg(unix)]
fn run_salus_os(args: &[OsString]) -> Output {
    Command::new(salus_bin())
        .args(args)
        .output()
        .expect("salus test binary must execute with OS arguments")
}

#[cfg(unix)]
fn non_utf8_arg() -> OsString {
    use std::os::unix::ffi::OsStringExt;

    OsString::from_vec(vec![0xff])
}

#[cfg(unix)]
fn temp_non_utf8_path(prefix: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;

    let mut bytes = temp_file_path(prefix).into_os_string().into_vec();
    bytes.push(0xff);
    PathBuf::from(OsString::from_vec(bytes))
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

fn spawn_http_server(response: &'static str) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test listener must bind");
    let addr = listener
        .local_addr()
        .expect("test listener must expose a local address");

    thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("test server must accept one client");
        let mut buffer = [0_u8; 4096];
        let _ = stream.read(&mut buffer);
        stream
            .write_all(response.as_bytes())
            .expect("test server must write a response");
    });

    addr
}

#[test]
fn help_exits_zero_and_prints_to_stdout() {
    let output = run_salus(&["--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("Container health check probe runner"));
    assert!(stdout.contains("--timeout <DURATION>"));
    assert!(stdout.contains("--max-latency <DURATION>"));
    assert!(stdout.contains("Hard deadline for one probe"));
    assert!(stdout.contains("Fail if a successful probe takes longer than this"));
    assert!(
        stdout.contains(
            "Argument values support ${VAR} and ${VAR:-default} expansion before parsing."
        )
    );
    assert!(stdout.contains("Use $$ to keep a literal $ character in JSON-array commands."));
    assert!(stdout.contains("http  Probe an HTTP or HTTPS health endpoint"));
    assert!(stdout.contains("tcp   Probe TCP connectivity to an address"));
    if cfg!(feature = "grpc") {
        assert!(stdout.contains("grpc  Run a gRPC health check probe"));
    } else {
        assert!(!stdout.contains("grpc  Run a gRPC health check probe"));
    }
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
    assert!(stdout.contains("--header <NAME:VALUE>"));
    assert!(stdout.contains("Request header to send"));
    assert!(stdout.contains("--status <CODE|RANGE>"));
    assert!(stdout.contains("Accepted status code or inclusive range"));
    assert!(
        stdout.contains(
            "Argument values support ${VAR} and ${VAR:-default} expansion before parsing."
        )
    );
    assert!(stdout.contains("--header-present <NAME>"));
    assert!(stdout.contains("--header-equals <NAME:VALUE>"));
    assert!(stdout.contains("--contains <TEXT>"));
    assert!(stdout.contains("--body-equals <TEXT>"));
    assert!(stdout.contains("--not-contains <TEXT>"));
}

#[test]
fn limit_help_documents_capture_cap() {
    for (subcommand, flag) in [
        ("http", "--max-body <BYTES>"),
        ("exec", "--max-output <BYTES>"),
        ("file", "--max-read <BYTES>"),
    ] {
        let output = run_salus(&[subcommand, "--help"]);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(output.status.code(), Some(0), "{subcommand}");
        assert!(stdout.contains(flag), "{subcommand}");
        assert!(stdout.contains("up to 16 MiB"), "{subcommand}");
        assert!(output.stderr.is_empty(), "{subcommand}");
    }
}

#[test]
fn subcommand_help_documents_environment_expansion() {
    let mut subcommands = vec!["http", "tcp", "exec", "file"];
    if cfg!(feature = "grpc") {
        subcommands.push("grpc");
    }

    for subcommand in subcommands {
        let output = run_salus(&[subcommand, "--help"]);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(output.status.code(), Some(0), "{subcommand}");
        assert!(
            stdout.contains(
                "Argument values support ${VAR} and ${VAR:-default} expansion before parsing."
            ),
            "{subcommand}"
        );
        assert!(
            stdout.contains("Use $$ to keep a literal $ character in JSON-array commands."),
            "{subcommand}"
        );
        assert!(output.stderr.is_empty(), "{subcommand}");
    }
}

#[test]
fn help_does_not_require_environment_expansion() {
    let output = run_salus_with_env(
        &["http", "--url", "${SALUS_TEST_MISSING_ENV}", "--help"],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("Probe an HTTP or HTTPS health endpoint"));
    assert!(output.stderr.is_empty());
}

#[test]
fn help_does_not_parse_environment_expanded_durations() {
    let output = run_salus_with_env(
        &["--timeout", "${SALUS_TEST_MISSING_ENV}", "--help"],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("Container health check probe runner"));
    assert!(output.stderr.is_empty());
}

#[test]
fn help_subcommand_does_not_parse_environment_expanded_durations() {
    let output = run_salus_with_env(
        &["--timeout", "${SALUS_TEST_MISSING_ENV}", "help"],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("Container health check probe runner"));
    assert!(output.stderr.is_empty());
}

#[test]
fn help_subcommand_does_not_require_environment_expansion() {
    let output = run_salus_with_env(
        &["help", "http", "--url", "${SALUS_TEST_MISSING_ENV}"],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("Probe an HTTP or HTTPS health endpoint"));
    assert!(stdout.contains("--url <URL>"));
    assert!(output.stderr.is_empty());
}

#[test]
fn help_subcommand_help_flag_prints_help_command_usage() {
    for flag in ["--help", "-h"] {
        let output = run_salus(&["help", flag]);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert!(stdout.contains("Print this message or the help of the given subcommand(s)"));
        assert!(stdout.contains("Usage: salus help [COMMAND]..."));
        assert!(output.stderr.is_empty(), "{flag}");
    }
}

#[test]
fn version_does_not_parse_environment_expanded_durations() {
    let output = run_salus_with_env(
        &["--timeout", "${SALUS_TEST_MISSING_ENV}", "--version"],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.starts_with("salus "));
    assert!(output.stderr.is_empty());
}

#[test]
fn subcommand_version_does_not_skip_environment_expansion() {
    let output = run_salus_with_env(
        &["http", "--url", "${SALUS_TEST_MISSING_ENV}", "--version"],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("environment variable SALUS_TEST_MISSING_ENV is not set")
    );
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
fn environment_variable_defaults_are_used_for_empty_values() {
    let path = temp_file_path("salus-cli-env-empty-default");
    write_file(&path, b"ready\n");

    let output = run_salus_with_env(
        &[
            "file",
            "--path",
            &path.display().to_string(),
            "--contains",
            "${SALUS_TEST_DEFAULT_NEEDLE:-ready}",
        ],
        &[("SALUS_TEST_DEFAULT_NEEDLE", Some(""))],
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());

    let _ = fs::remove_file(path);
}

#[test]
fn quiet_suppresses_environment_expansion_errors() {
    let path = temp_file_path("salus-cli-quiet-env-error");
    write_file(&path, b"ready\n");

    let output = run_salus_with_env(
        &[
            "--quiet",
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
    assert!(output.stderr.is_empty());

    let _ = fs::remove_file(path);
}

#[test]
fn quiet_suppresses_cli_parse_errors() {
    let output = run_salus(&["--quiet", "--bad-flag"]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn expanded_quiet_suppresses_cli_parse_errors() {
    let output = run_salus_with_env(
        &["${SALUS_TEST_QUIET}", "--bad-flag"],
        &[("SALUS_TEST_QUIET", Some("--quiet"))],
    );

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn expanded_quiet_suppresses_later_environment_expansion_errors() {
    let output = run_salus_with_env(
        &[
            "${SALUS_TEST_QUIET}",
            "file",
            "--path",
            "${SALUS_TEST_MISSING_ENV}",
        ],
        &[
            ("SALUS_TEST_QUIET", Some("--quiet")),
            ("SALUS_TEST_MISSING_ENV", None),
        ],
    );

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn quiet_prescan_does_not_accept_subcommand_options_at_root() {
    let output = run_salus(&["--url", "http://127.0.0.1:8080", "--quiet"]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--url'"));
}

#[cfg(unix)]
#[test]
fn quiet_prescan_stops_at_non_utf8_unknown_argument() {
    let output = run_salus_os(&[non_utf8_arg(), OsString::from("--quiet")]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
}

#[cfg(unix)]
#[test]
fn quiet_prescan_stops_at_non_utf8_unknown_subcommand_argument() {
    let output = run_salus_os(&[
        OsString::from("file"),
        non_utf8_arg(),
        OsString::from("--quiet"),
    ]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
}

#[cfg(unix)]
#[test]
fn help_prescan_stops_at_non_utf8_unknown_argument() {
    let output = run_salus_os(&[non_utf8_arg(), OsString::from("--help")]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
}

#[test]
fn exec_command_arguments_do_not_enable_quiet_for_environment_errors() {
    let output = run_salus_with_env(
        &["exec", "sh", "--quiet", "${SALUS_TEST_MISSING_ENV}"],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("environment variable SALUS_TEST_MISSING_ENV is not set")
    );
}

#[test]
fn expanded_exec_command_arguments_do_not_enable_quiet_for_environment_errors() {
    let output = run_salus_with_env(
        &[
            "exec",
            "sh",
            "${SALUS_TEST_QUIET}",
            "${SALUS_TEST_MISSING_ENV}",
        ],
        &[
            ("SALUS_TEST_QUIET", Some("--quiet")),
            ("SALUS_TEST_MISSING_ENV", None),
        ],
    );

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("environment variable SALUS_TEST_MISSING_ENV is not set")
    );
}

#[test]
fn environment_variable_expansion_reaches_http_probe_arguments() {
    let addr = spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    let port = addr.port().to_string();

    let output = run_salus_with_env(
        &[
            "http",
            "--url",
            "http://127.0.0.1:${SALUS_TEST_HTTP_PORT}/healthz",
            "--contains",
            "ok",
        ],
        &[("SALUS_TEST_HTTP_PORT", Some(port.as_str()))],
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn http_complete_body_at_limit_proves_missing_contains() {
    let addr = spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\naaaa");

    let output = run_salus(&[
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--max-body",
        "4",
        "--contains",
        "ready",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("does not contain required text \"ready\""));
    assert!(!stderr.contains("truncated"));
}

#[test]
fn http_unknown_length_body_at_limit_proves_missing_contains_after_eof() {
    let addr = spawn_http_server("HTTP/1.1 200 OK\r\n\r\naaaa");

    let output = run_salus(&[
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--max-body",
        "4",
        "--contains",
        "ready",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("does not contain required text \"ready\""));
    assert!(!stderr.contains("truncated"));
    assert!(!stderr.contains("cannot prove"));
}

#[test]
fn http_truncated_body_with_mismatched_prefix_reports_exact_mismatch() {
    let addr = spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nxxxxmore");

    let output = run_salus(&[
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--max-body",
        "4",
        "--body-equals",
        "ready",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("does not equal required text \"ready\""));
    assert!(!stderr.contains("cannot prove"));
}

#[test]
fn http_truncated_body_with_forbidden_text_reports_forbidden_text() {
    let addr = spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nbad-data");

    let output = run_salus(&[
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--max-body",
        "4",
        "--not-contains",
        "bad",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("contains forbidden text \"bad\""));
    assert!(!stderr.contains("cannot prove"));
}

#[test]
fn http_truncated_body_with_negative_assertion_reports_target_and_limit() {
    let addr = spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\naaaaaaaa");

    let output = run_salus(&[
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--max-body",
        "4",
        "--not-contains",
        "missing",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains(&format!(
        "response body from http://{addr}/healthz was truncated at 4 bytes, cannot prove negative body assertions"
    )));
}

#[test]
fn environment_variable_expansion_reaches_exec_trailing_arguments() {
    let output = run_salus_with_env(
        &[
            "exec",
            "--stdout-contains",
            "${SALUS_TEST_EXEC_NEEDLE}",
            "--",
            "sh",
            "-c",
            "printf %s \"$1\"",
            "sh",
            "${SALUS_TEST_EXEC_VALUE}",
        ],
        &[
            ("SALUS_TEST_EXEC_NEEDLE", Some("ready")),
            ("SALUS_TEST_EXEC_VALUE", Some("ready")),
        ],
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn exec_command_arguments_do_not_disable_environment_expansion() {
    let output = run_salus_with_env(
        &[
            "exec",
            "--stdout-contains",
            "${SALUS_TEST_EXEC_NEEDLE}",
            "sh",
            "-c",
            "printf ready",
            "-h",
        ],
        &[("SALUS_TEST_EXEC_NEEDLE", Some("ready"))],
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn exec_help_does_not_require_environment_expansion() {
    let output = run_salus_with_env(
        &[
            "exec",
            "--stdout-contains",
            "${SALUS_TEST_MISSING_ENV}",
            "--help",
        ],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("Run a command and evaluate its exit code and output"));
    assert!(output.stderr.is_empty());
}

#[test]
fn exec_help_does_not_parse_environment_expanded_durations() {
    let output = run_salus_with_env(
        &["exec", "--timeout", "${SALUS_TEST_MISSING_ENV}", "--help"],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout.contains("Run a command and evaluate its exit code and output"));
    assert!(output.stderr.is_empty());
}

#[test]
fn exec_version_argument_does_not_skip_environment_expansion() {
    let output = run_salus_with_env(
        &[
            "exec",
            "--stdout-contains",
            "${SALUS_TEST_MISSING_ENV}",
            "--version",
        ],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("environment variable SALUS_TEST_MISSING_ENV is not set")
    );
}

#[test]
fn exec_version_argument_does_not_enable_quiet() {
    let output = run_salus_with_env(
        &["exec", "-V", "--quiet", "${SALUS_TEST_MISSING_ENV}"],
        &[("SALUS_TEST_MISSING_ENV", None)],
    );

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("environment variable SALUS_TEST_MISSING_ENV is not set")
    );
}

#[test]
fn option_value_positions_named_like_help_are_not_treated_as_help() {
    let output = run_salus(&["file", "--path", "/tmp/salus-test", "--max-age", "--help"]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("a value is required for '--max-age <DURATION>'")
    );
}

#[test]
fn option_value_positions_named_like_quiet_do_not_suppress_errors() {
    let output = run_salus(&["file", "--path", "/tmp/salus-test", "--max-age", "--quiet"]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("a value is required for '--max-age <DURATION>'")
    );
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

#[cfg(unix)]
#[test]
fn verbose_file_success_escapes_non_utf8_path() {
    let path = temp_non_utf8_path("salus-cli-non-utf8-success");
    write_file(&path, b"ready\n");
    let args = vec![
        OsString::from("--verbose"),
        OsString::from("file"),
        OsString::from("--path"),
        path.as_os_str().to_os_string(),
        OsString::from("--readable"),
    ];

    let output = run_salus_os(&args);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("\\xFF"));
    assert!(!stderr.contains('\u{FFFD}'));

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

#[test]
fn tcp_error_escapes_control_characters_in_address() {
    let output = run_salus(&["tcp", "--addr", "bad\naddr:80"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(!stderr.contains("bad\naddr:80"));
    assert!(stderr.contains("\"bad\\naddr:80\""));
}

#[cfg(all(feature = "grpc", not(feature = "webpki")))]
#[test]
fn grpc_tls_reports_invalid_address_before_missing_trust() {
    let output = run_salus(&["grpc", "--addr", "127.0.0.1:0", "--tls"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("invalid gRPC address \"127.0.0.1:0\""));
}

#[cfg(all(feature = "grpc", not(feature = "webpki")))]
#[test]
fn grpc_tls_reports_invalid_authority_before_missing_trust() {
    let output = run_salus(&[
        "grpc",
        "--addr",
        "127.0.0.1:9",
        "--tls",
        "--authority",
        "example.com:0",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("invalid gRPC authority \"example.com:0\""));
}

#[test]
fn file_error_escapes_control_characters_in_path() {
    let path = temp_file_path("salus-cli-bad\npath");
    let output = run_salus(&["file", "--path", &path.display().to_string()]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!stderr.contains(&path.display().to_string()));
    assert!(stderr.contains(&format!("{path:?}")));
}

#[cfg(unix)]
#[test]
fn http_unix_socket_error_escapes_control_characters_in_socket_path() {
    let socket = Path::new("/tmp/salus-cli-bad\nsock");
    let output = run_salus(&[
        "http",
        "--sock",
        &socket.display().to_string(),
        "--path",
        "/healthz",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!stderr.contains(&socket.display().to_string()));
    assert!(stderr.contains(&format!("{socket:?}")));
}

#[test]
fn http_method_error_escapes_control_characters() {
    let output = run_salus(&[
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--method",
        "GE\nT",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(!stderr.contains("GE\nT"));
    assert!(stderr.contains("\"GE\\nT\""));
}

#[test]
fn http_status_error_escapes_control_characters() {
    let output = run_salus(&[
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--status",
        "299-200\n",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(!stderr.contains("299-200\n"));
    assert!(stderr.contains("\"299-200\\n\""));
}

#[test]
fn http_status_error_quotes_empty_values() {
    let output = run_salus(&[
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--status",
        "",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("invalid status code \"\", expected integer"));
}

#[test]
fn http_status_error_treats_leading_hyphen_as_invalid_code() {
    let output = run_salus(&["http", "--url", "http://127.0.0.1:9/healthz", "--status=-1"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("invalid status code -1, expected integer"));
}

#[test]
fn exec_complete_output_at_limit_reports_missing_required_text() {
    let output = run_salus(&[
        "exec",
        "--stdout-contains",
        "ready",
        "--max-output",
        "4",
        "--",
        "sh",
        "-c",
        "printf aaaa",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("stdout of sh does not contain required text \"ready\""));
    assert!(!stderr.contains("cannot prove"));
}

#[test]
fn exec_inherited_pipe_at_output_limit_reports_missing_required_text_after_cleanup() {
    let output = run_salus(&[
        "exec",
        "--stdout-contains",
        "ready",
        "--max-output",
        "4",
        "--",
        "sh",
        "-c",
        "printf aaaa; sleep 1 &",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(stderr.contains("stdout of sh does not contain required text \"ready\""));
    assert!(!stderr.contains("cannot prove"));
}
