use std::{
    convert::Infallible,
    ffi::OsString,
    fs,
    io::Cursor,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use http_body_util::Full;
use hyper::{
    Request, Response,
    body::{Bytes, Incoming},
    server::conn::http2,
    service::service_fn,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ServerConfig;
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(feature = "grpc")]
use tokio::sync::oneshot;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;
#[cfg(feature = "grpc")]
use tokio_stream::wrappers::TcpListenerStream;
#[cfg(feature = "grpc")]
use tonic_health::ServingStatus;

const TLS_CERT_PEM: &str = include_str!("fixtures/server.pem");
const TLS_KEY_PEM: &str = include_str!("fixtures/server.rsa");
const TLS_CERT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/server.pem");
const EXCESSIVE_CAPTURE_LIMIT: &str = "16777217";
#[cfg(feature = "grpc")]
// gRPC frame for grpc.health.v1.HealthCheckResponse { status: SERVING }.
const GRPC_HEALTH_SERVING_FRAME: &[u8] = &[0, 0, 0, 0, 2, 8, 1];

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[tokio::test]
async fn tcp_probe_succeeds_on_open_port() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let _ = listener.accept().await;
    });

    let code = salus::main_entry(args(&["salus", "tcp", "--addr", &addr.to_string()])).await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn tcp_probe_rejects_empty_address() {
    let code = salus::main_entry(args(&["salus", "tcp", "--addr", ""])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn tcp_probe_rejects_address_without_hostname() {
    let code = salus::main_entry(args(&["salus", "tcp", "--addr", ":80"])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn tcp_probe_rejects_empty_bracketed_host() {
    let code = salus::main_entry(args(&["salus", "tcp", "--addr", "[]:80"])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn tcp_probe_rejects_embedded_bracket_host() {
    let code = salus::main_entry(args(&["salus", "tcp", "--addr", "example[::1]:80"])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn tcp_probe_rejects_zero_port() {
    let code = salus::main_entry(args(&["salus", "tcp", "--addr", "127.0.0.1:0"])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn tcp_probe_rejects_out_of_range_port() {
    let code = salus::main_entry(args(&["salus", "tcp", "--addr", "127.0.0.1:65536"])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn timeout_must_be_greater_than_zero() {
    let code = salus::main_entry(args(&[
        "salus",
        "--timeout",
        "0s",
        "tcp",
        "--addr",
        "127.0.0.1:80",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn duration_arguments_allow_surrounding_whitespace() {
    let path = temp_file_path("salus-duration-trim");
    fs::write(&path, "ok").unwrap();

    let code = salus::main_entry(args(&[
        "salus",
        "--timeout",
        " 1s ",
        "file",
        "--path",
        path.to_str().unwrap(),
        "--readable",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn max_latency_must_be_greater_than_zero() {
    let code = salus::main_entry(args(&[
        "salus",
        "--max-latency",
        "0s",
        "tcp",
        "--addr",
        "127.0.0.1:80",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_succeeds_on_plain_http() {
    let addr = spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_accepts_lowercase_method_and_spaced_status_range() {
    let addr = spawn_http_server("HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--method",
        "get",
        "--status",
        " 200 - 299 ",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_fails_when_probe_exceeds_max_latency() {
    let addr = spawn_http_server_with_delay(
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
        Duration::from_millis(75),
    )
    .await;

    let code = salus::main_entry(args(&[
        "salus",
        "--timeout",
        "1s",
        "--max-latency",
        "20ms",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn http_probe_stops_reading_body_at_max_body() {
    let addr = spawn_http_open_body_server("HTTP/1.1 200 OK\r\n\r\nok!").await;

    let code = salus::main_entry(args(&[
        "salus",
        "--timeout",
        "200ms",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--max-body",
        "2",
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_succeeds_after_required_body_before_stream_closes() {
    let addr = spawn_http_open_body_server("HTTP/1.1 200 OK\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "--timeout",
        "200ms",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_exact_body_mismatch_does_not_wait_for_open_stream() {
    let addr = spawn_http_open_body_server("HTTP/1.1 200 OK\r\n\r\nno").await;

    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "http",
            "--url",
            &format!("http://{addr}/healthz"),
            "--body-equals",
            "ready",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[tokio::test]
async fn http_probe_exact_body_length_mismatch_does_not_wait_for_open_stream() {
    let addr = spawn_http_open_body_server("HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\n").await;

    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "http",
            "--url",
            &format!("http://{addr}/healthz"),
            "--body-equals",
            "",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[tokio::test]
async fn http_probe_forbidden_body_text_does_not_wait_for_open_stream() {
    let addr = spawn_http_open_body_server("HTTP/1.1 200 OK\r\n\r\nbad").await;

    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "http",
            "--url",
            &format!("http://{addr}/healthz"),
            "--not-contains",
            "bad",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[tokio::test]
async fn http_probe_sends_header() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer).await.unwrap();
        let request = String::from_utf8_lossy(&buffer[..read]).to_ascii_lowercase();
        let response = if request.contains("\r\nx-api-key: secret\r\n")
            || request.contains("\r\nx-api-key:secret\r\n")
        {
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
        } else {
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 9\r\n\r\nforbidden"
        };
        let _ = stream.write_all(response.as_bytes()).await;
    });

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--header",
        "x-api-key:secret",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_uses_custom_user_agent_without_default() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer).await.unwrap();
        let request = String::from_utf8_lossy(&buffer[..read]).to_ascii_lowercase();
        let has_custom_user_agent = request.contains("\r\nuser-agent: custom-probe\r\n")
            || request.contains("\r\nuser-agent:custom-probe\r\n");
        let response = if has_custom_user_agent && !request.contains("salus/") {
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
        } else {
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 9\r\n\r\nforbidden"
        };
        let _ = stream.write_all(response.as_bytes()).await;
    });

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--header",
        "User-Agent:custom-probe",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_rejects_host_request_header() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--header",
        "Host:example.test",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_framing_request_header() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--header",
        "Content-Length:1",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_hop_by_hop_request_header() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--header",
        "Connection: close",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_proxy_connection_request_header() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--header",
        "Proxy-Connection: keep-alive",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_empty_host_override() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        "",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_host_override_without_hostname() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        ":8080",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_empty_bracketed_host_override() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        "[]:80",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_bracketed_host_override_with_suffix() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        "[::1]example:80",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_embedded_bracket_host_override() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        "example[::1]:80",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_invalid_host_override() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        "example.com/path",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_invalid_host_override_port() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        "example.com:bad",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_zero_host_override_port() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        "example.com:0",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_out_of_range_host_override_port() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        "example.com:65536",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_invalid_ipv6_host_override_port() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz",
        "--host",
        "[::1]:bad",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_fragment() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/healthz#ready",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_with_whitespace() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        " http://127.0.0.1:9/healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_with_embedded_newline() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/health\nz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_with_control_character() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9/health\u{1}z",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_with_backslash() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:9\\healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_with_invalid_percent_encoding() {
    let code = salus::main_entry(args(&["salus", "http", "--url", "http://127.0.0.1:9/%zz"])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_empty_url() {
    let code = salus::main_entry(args(&["salus", "http", "--url", ""])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_with_empty_authority() {
    let code = salus::main_entry(args(&["salus", "http", "--url", "http:///healthz"])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_with_empty_userinfo() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://@127.0.0.1:9/healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_with_empty_user_and_password() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://:@127.0.0.1:9/healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_without_authority_delimiter() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http:127.0.0.1:9/healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_url_with_single_slash_authority_delimiter() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http:/127.0.0.1:9/healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_zero_port() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:0/healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_empty_url_port() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:/healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_empty_ipv6_url_port() {
    let code = salus::main_entry(args(&["salus", "http", "--url", "http://[::1]:/healthz"])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_succeeds_with_exact_header_and_body_assertions() {
    let addr =
        spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Ready: ok\r\n\r\nready").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--header-present",
        "x-ready",
        "--header-equals",
        "x-ready:ok",
        "--body-equals",
        "ready",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_fails_when_required_response_header_is_missing() {
    let addr = spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--header-present",
        "x-ready",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn http_probe_fails_when_response_header_exact_match_is_wrong() {
    let addr =
        spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Ready: warming\r\n\r\nready")
            .await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--header-equals",
        "x-ready:ok",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn http_probe_fails_on_redirect_by_default() {
    let addr = spawn_http_server("HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn http_probe_succeeds_when_status_override_allows_redirect() {
    let addr = spawn_http_server("HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--status",
        "302",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_succeeds_with_response_header_assertions() {
    let addr =
        spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Ready: ok\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--header-contains",
        "x-ready:ok",
        "--header-not-contains",
        "x-ready:error",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_fails_when_required_response_header_text_is_missing() {
    let addr =
        spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nX-Ready: warming\r\n\r\nok")
            .await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--header-contains",
        "x-ready:ok",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[cfg(unix)]
#[tokio::test]
async fn http_probe_succeeds_over_unix_socket() {
    let socket_path = temp_file_path("salus-http-uds");
    let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    spawn_http_uds_server(&socket_path, response).await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--sock",
        &socket_path.display().to_string(),
        "--path",
        "/healthz",
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);

    let _ = fs::remove_file(socket_path);
}

#[cfg(unix)]
#[tokio::test]
async fn http_probe_rejects_unix_socket_without_path() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--sock",
        "/tmp/salus-missing-path.sock",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(unix)]
#[tokio::test]
async fn http_probe_rejects_empty_unix_socket_path() {
    let code =
        salus::main_entry(args(&["salus", "http", "--sock", "", "--path", "/healthz"])).await;

    assert_eq!(code, 3);
}

#[cfg(unix)]
#[tokio::test]
async fn http_probe_rejects_unix_socket_path_without_leading_slash() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--sock",
        "/tmp/salus-invalid-path.sock",
        "--path",
        "healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(unix)]
#[tokio::test]
async fn http_probe_rejects_unix_socket_path_fragment() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--sock",
        "/tmp/salus-fragment-path.sock",
        "--path",
        "/healthz#ready",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(unix)]
#[tokio::test]
async fn http_probe_rejects_invalid_unix_socket_uri_path_before_connecting() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--sock",
        "/tmp/salus-missing-invalid-path.sock",
        "--path",
        "/health check",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(unix)]
#[tokio::test]
async fn http_probe_rejects_unix_socket_path_with_backslash_before_connecting() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--sock",
        "/tmp/salus-missing-backslash-path.sock",
        "--path",
        "/health\\z",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(unix)]
#[tokio::test]
async fn http_probe_rejects_unix_socket_path_with_invalid_percent_encoding_before_connecting() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--sock",
        "/tmp/salus-missing-percent-path.sock",
        "--path",
        "/%zz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn https_probe_succeeds_with_ca_file_and_server_name_override() {
    let addr = spawn_https_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("https://{addr}/healthz"),
        "--ca",
        TLS_CERT_PATH,
        "--server-name",
        "localhost",
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn https_probe_succeeds_with_insecure_skip_verify() {
    let addr = spawn_https_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("https://{addr}/healthz"),
        "--insecure-skip-verify",
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn https_probe_rejects_conflicting_trust_controls() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "https://127.0.0.1:9/healthz",
        "--ca",
        TLS_CERT_PATH,
        "--insecure-skip-verify",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn https_probe_rejects_empty_ca_file() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "https://127.0.0.1:9/healthz",
        "--ca",
        "",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn https_probe_rejects_non_regular_ca_file() {
    let path = temp_file_path("salus-tls-ca-dir");
    fs::create_dir(&path).unwrap();

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "https://127.0.0.1:9/healthz",
        "--ca",
        &path.display().to_string(),
    ]))
    .await;

    assert_eq!(code, 3);

    let _ = fs::remove_dir(path);
}

#[tokio::test]
async fn https_probe_fails_without_trust_override() {
    let addr = spawn_https_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("https://{addr}/healthz"),
    ]))
    .await;

    if cfg!(feature = "webpki") {
        assert_eq!(code, 1);
    } else {
        assert_eq!(code, 3);
    }
}

#[tokio::test]
async fn https_probe_fails_with_wrong_server_name() {
    let addr = spawn_https_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("https://{addr}/healthz"),
        "--ca",
        TLS_CERT_PATH,
        "--server-name",
        "example.invalid",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn https_probe_succeeds_over_http2() {
    let addr = spawn_https_http2_server().await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("https://{addr}/healthz"),
        "--ca",
        TLS_CERT_PATH,
        "--server-name",
        "localhost",
        "--contains",
        "ok",
        "--header-contains",
        "x-salus-protocol:h2",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn https_http2_probe_uses_host_override_as_authority() {
    let addr = spawn_https_http2_authority_server("example.test").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("https://{addr}/healthz"),
        "--ca",
        TLS_CERT_PATH,
        "--server-name",
        "localhost",
        "--host",
        "example.test",
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_fails_when_negative_body_assertion_truncates() {
    let body = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let addr = spawn_http_server(&response).await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--max-body",
        "4",
        "--not-contains",
        "zzz",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn http_probe_uses_declared_body_length_to_fail_at_max_body() {
    let addr =
        spawn_http_open_body_server("HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\naaaa").await;

    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "http",
            "--url",
            &format!("http://{addr}/healthz"),
            "--max-body",
            "4",
            "--not-contains",
            "zzz",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[tokio::test]
async fn http_probe_unknown_length_body_at_max_body_does_not_wait_for_open_stream() {
    let addr = spawn_http_open_body_server("HTTP/1.1 200 OK\r\n\r\naaaa").await;

    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "http",
            "--url",
            &format!("http://{addr}/healthz"),
            "--max-body",
            "4",
            "--contains",
            "ready",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[tokio::test]
async fn http_probe_accepts_unknown_length_body_exactly_at_max_body_when_complete() {
    let addr = spawn_http_server("HTTP/1.1 200 OK\r\n\r\naaaa").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--max-body",
        "4",
        "--body-equals",
        "aaaa",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_does_not_match_replacement_character_for_invalid_body_bytes() {
    let addr =
        spawn_http_server_bytes(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n\xff".to_vec()).await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--contains",
        "\u{FFFD}",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn http_probe_fails_when_exact_body_assertion_truncates() {
    let body = "ready-after-prefix";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let addr = spawn_http_server(&response).await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("http://{addr}/healthz"),
        "--max-body",
        "5",
        "--body-equals",
        "ready",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn http_probe_rejects_zero_body_limit_with_body_assertion() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:1/healthz",
        "--max-body",
        "0",
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_excessive_body_limit_with_body_assertion() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:1/healthz",
        "--max-body",
        EXCESSIVE_CAPTURE_LIMIT,
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_empty_body_contains_assertion() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:1/healthz",
        "--contains",
        "",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_empty_header_contains_assertion() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:1/healthz",
        "--header-contains",
        "x-ready:",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_body_assertions_for_head_requests() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:1/healthz",
        "--method",
        "HEAD",
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn http_probe_rejects_body_assertions_for_lowercase_head_requests() {
    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        "http://127.0.0.1:1/healthz",
        "--method",
        "head",
        "--contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn file_probe_succeeds() {
    let path = temp_file_path("salus-file");
    fs::write(&path, b"ready\n").unwrap();

    let code = salus::main_entry(args(&[
        "salus",
        "file",
        "--path",
        &path.display().to_string(),
        "--readable",
        "--contains",
        "ready",
    ]))
    .await;

    assert_eq!(code, 0);

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn file_probe_succeeds_with_readable_only_and_zero_read_limit() {
    let path = temp_file_path("salus-file-readable");
    fs::write(&path, b"ready\n").unwrap();

    let code = salus::main_entry(args(&[
        "salus",
        "file",
        "--path",
        &path.display().to_string(),
        "--readable",
        "--max-read",
        "0",
    ]))
    .await;

    assert_eq!(code, 0);

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn file_probe_rejects_zero_read_limit_when_content_must_be_read() {
    let path = temp_file_path("salus-file-invalid");
    fs::write(&path, b"ready\n").unwrap();

    let code = salus::main_entry(args(&[
        "salus",
        "file",
        "--path",
        &path.display().to_string(),
        "--contains",
        "ready",
        "--max-read",
        "0",
    ]))
    .await;

    assert_eq!(code, 3);

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn file_probe_rejects_excessive_read_limit_when_content_must_be_read() {
    let code = salus::main_entry(args(&[
        "salus",
        "file",
        "--path",
        "/does/not/matter",
        "--contains",
        "ready",
        "--max-read",
        EXCESSIVE_CAPTURE_LIMIT,
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn file_probe_rejects_zero_max_age() {
    let path = temp_file_path("salus-file-zero-max-age");
    fs::write(&path, b"ready\n").unwrap();

    let code = salus::main_entry(args(&[
        "salus",
        "file",
        "--path",
        &path.display().to_string(),
        "--max-age",
        "0s",
    ]))
    .await;

    assert_eq!(code, 3);

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn file_probe_rejects_empty_contains_assertion() {
    let code = salus::main_entry(args(&[
        "salus",
        "file",
        "--path",
        "/does/not/matter",
        "--contains",
        "",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn file_probe_rejects_empty_path() {
    let code = salus::main_entry(args(&["salus", "file", "--path", ""])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn file_probe_fails_when_required_text_may_be_beyond_read_limit() {
    let path = temp_file_path("salus-file-truncated");
    fs::write(&path, b"aaaaaready\n").unwrap();

    let code = salus::main_entry(args(&[
        "salus",
        "file",
        "--path",
        &path.display().to_string(),
        "--contains",
        "ready",
        "--max-read",
        "4",
    ]))
    .await;

    assert_eq!(code, 1);

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn file_probe_fails_when_modified_time_is_in_the_future() {
    let path = temp_file_path("salus-file-future-mtime");
    fs::write(&path, b"ready\n").unwrap();
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(60)))
        .unwrap();

    let code = salus::main_entry(args(&[
        "salus",
        "file",
        "--path",
        &path.display().to_string(),
        "--max-age",
        "5s",
    ]))
    .await;

    assert_eq!(code, 1);

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn file_probe_does_not_match_replacement_character_for_invalid_bytes() {
    let path = temp_file_path("salus-file-invalid-utf8");
    fs::write(&path, b"\xff").unwrap();

    let code = salus::main_entry(args(&[
        "salus",
        "file",
        "--path",
        &path.display().to_string(),
        "--contains",
        "\u{FFFD}",
    ]))
    .await;

    assert_eq!(code, 1);

    let _ = fs::remove_file(path);
}

#[tokio::test]
async fn exec_probe_succeeds() {
    let code = salus::main_entry(args(&[
        "salus",
        "exec",
        "--stdout-contains",
        "ok",
        "--",
        "sh",
        "-c",
        "printf ok",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn exec_probe_rejects_zero_output_limit_with_output_assertion() {
    let code = salus::main_entry(args(&[
        "salus",
        "exec",
        "--stdout-contains",
        "ok",
        "--max-output",
        "0",
        "--",
        "sh",
        "-c",
        "printf ok",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn exec_probe_rejects_excessive_output_limit_with_output_assertion() {
    let code = salus::main_entry(args(&[
        "salus",
        "exec",
        "--stdout-contains",
        "ok",
        "--max-output",
        EXCESSIVE_CAPTURE_LIMIT,
        "--",
        "sh",
        "-c",
        "printf ok",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn exec_probe_rejects_empty_stdout_contains_assertion() {
    let code = salus::main_entry(args(&[
        "salus",
        "exec",
        "--stdout-contains",
        "",
        "--",
        "sh",
        "-c",
        "printf ok",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn exec_probe_rejects_empty_command() {
    let code = salus::main_entry(args(&["salus", "exec", "--", ""])).await;

    assert_eq!(code, 3);
}

#[tokio::test]
async fn exec_probe_fails_when_required_stdout_may_be_beyond_output_limit() {
    let code = salus::main_entry(args(&[
        "salus",
        "exec",
        "--stdout-contains",
        "ready",
        "--max-output",
        "4",
        "--",
        "sh",
        "-c",
        "printf aaaaready",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn exec_probe_does_not_match_replacement_character_for_invalid_stdout_bytes() {
    let code = salus::main_entry(args(&[
        "salus",
        "exec",
        "--stdout-contains",
        "\u{FFFD}",
        "--",
        "sh",
        "-c",
        "printf '\\377'",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn exec_probe_truncated_output_does_not_wait_for_inherited_pipe() {
    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "exec",
            "--stdout-contains",
            "ready",
            "--max-output",
            "4",
            "--",
            "sh",
            "-c",
            "printf aaaaa; sleep 1 &",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[tokio::test]
async fn exec_probe_full_output_limit_does_not_wait_for_inherited_pipe() {
    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "exec",
            "--stdout-contains",
            "ready",
            "--max-output",
            "4",
            "--",
            "sh",
            "-c",
            "printf aaaa; sleep 1 &",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[tokio::test]
async fn exec_probe_missing_required_output_does_not_wait_for_inherited_pipe() {
    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "exec",
            "--stdout-contains",
            "ready",
            "--",
            "sh",
            "-c",
            "printf no; sleep 1 &",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[cfg(unix)]
#[tokio::test]
async fn exec_probe_missing_required_output_does_not_wait_for_daemonized_pipe() {
    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "exec",
            "--stdout-contains",
            "ready",
            "--",
            "sh",
            "-c",
            "printf no; setsid sh -c 'sleep 1' &",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[cfg(unix)]
#[tokio::test]
async fn exec_probe_missing_required_output_does_not_wait_for_active_daemonized_pipe() {
    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "exec",
            "--stdout-contains",
            "ready",
            "--",
            "sh",
            "-c",
            "printf no; setsid sh -c 'i=0; while [ \"$i\" -lt 100 ]; do printf x; i=$((i + 1)); sleep 0.02; done' &",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[tokio::test]
async fn exec_probe_timeout_does_not_wait_for_inherited_output_pipes() {
    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "100ms",
            "exec",
            "--",
            "sh",
            "-c",
            "sleep 1 & sleep 1",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[cfg(unix)]
#[tokio::test]
async fn exec_probe_timeout_kills_background_process_group_members() {
    let marker = temp_file_path("salus-exec-timeout-background");

    let code = salus::main_entry(args(&[
        "salus",
        "--timeout",
        "100ms",
        "exec",
        "--",
        "sh",
        "-c",
        "marker=$1; (sleep 0.4; printf survived > \"$marker\") & sleep 5",
        "sh",
        &marker.display().to_string(),
    ]))
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;

    assert_eq!(code, 1);
    assert!(!marker.exists());

    let _ = fs::remove_file(marker);
}

#[cfg(unix)]
#[tokio::test]
async fn exec_probe_success_kills_background_process_group_members() {
    let marker = temp_file_path("salus-exec-success-background");

    let code = salus::main_entry(args(&[
        "salus",
        "--timeout",
        "2s",
        "exec",
        "--stdout-contains",
        "ok",
        "--",
        "sh",
        "-c",
        "marker=$1; (sleep 0.4; printf survived > \"$marker\") & printf ok",
        "sh",
        &marker.display().to_string(),
    ]))
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;

    assert_eq!(code, 0);
    assert!(!marker.exists());

    let _ = fs::remove_file(marker);
}

#[cfg(unix)]
#[tokio::test]
async fn exec_probe_failure_kills_background_process_group_members() {
    let marker = temp_file_path("salus-exec-failure-background");

    let code = salus::main_entry(args(&[
        "salus",
        "--timeout",
        "2s",
        "exec",
        "--",
        "sh",
        "-c",
        "marker=$1; (sleep 0.4; printf survived > \"$marker\") & exit 42",
        "sh",
        &marker.display().to_string(),
    ]))
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;

    assert_eq!(code, 1);
    assert!(!marker.exists());

    let _ = fs::remove_file(marker);
}

#[tokio::test]
async fn exec_probe_succeeds_after_required_output_before_inherited_pipe_closes() {
    let result = tokio::time::timeout(
        Duration::from_millis(750),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "100ms",
            "exec",
            "--stdout-contains",
            "ok",
            "--",
            "sh",
            "-c",
            "printf ok; sleep 1 &",
        ])),
    )
    .await;

    assert_eq!(result, Ok(0));
}

#[tokio::test]
async fn exec_probe_does_not_close_output_pipe_after_required_text() {
    let code = salus::main_entry(args(&[
        "salus",
        "exec",
        "--stdout-contains",
        "ok",
        "--max-output",
        "2",
        "--",
        "sh",
        "-c",
        "printf ok; head -c 65536 /dev/zero >/dev/stdout",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn exec_probe_reports_exit_code_before_waiting_for_output_pipes() {
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        salus::main_entry(args(&[
            "salus",
            "--timeout",
            "5s",
            "exec",
            "--stdout-contains",
            "ok",
            "--",
            "sh",
            "-c",
            "sleep 1 & exit 42",
        ])),
    )
    .await;

    assert_eq!(result, Ok(1));
}

#[cfg(unix)]
#[tokio::test]
async fn exec_probe_fails_when_command_is_terminated_by_signal() {
    let code = salus::main_entry(args(&["salus", "exec", "--", "sh", "-c", "kill -TERM $$"])).await;

    assert_eq!(code, 1);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_succeeds() {
    let addr = spawn_grpc_server(false).await;

    let code = salus::main_entry(args(&["salus", "grpc", "--addr", &addr.to_string()])).await;

    assert_eq!(code, 0);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_uses_authority_override() {
    let addr = spawn_grpc_authority_server("example.test").await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        &addr.to_string(),
        "--authority",
        "example.test",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_fails_when_service_is_not_serving() {
    let addr =
        spawn_grpc_server_with_service_status(false, Some(("db", ServingStatus::NotServing))).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        &addr.to_string(),
        "--service",
        "db",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_fails_when_service_is_unregistered() {
    let addr = spawn_grpc_server(false).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        &addr.to_string(),
        "--service",
        "db",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_invalid_authority() {
    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        "127.0.0.1:9",
        "--authority",
        "example.com:bad",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_empty_authority() {
    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        "127.0.0.1:9",
        "--authority",
        "",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_authority_without_hostname() {
    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        "127.0.0.1:9",
        "--authority",
        ":50051",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_empty_bracketed_authority_host() {
    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        "127.0.0.1:9",
        "--authority",
        "[]:50051",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_zero_authority_port() {
    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        "127.0.0.1:9",
        "--authority",
        "example.com:0",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_out_of_range_authority_port() {
    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        "127.0.0.1:9",
        "--authority",
        "example.com:65536",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_invalid_address() {
    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        "127.0.0.1:50051/healthz",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_empty_address() {
    let code = salus::main_entry(args(&["salus", "grpc", "--addr", ""])).await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_address_without_hostname() {
    let code = salus::main_entry(args(&["salus", "grpc", "--addr", ":50051"])).await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_embedded_bracket_address_host() {
    let code = salus::main_entry(args(&["salus", "grpc", "--addr", "example[::1]:50051"])).await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_zero_port() {
    let code = salus::main_entry(args(&["salus", "grpc", "--addr", "127.0.0.1:0"])).await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_address_userinfo() {
    let code = salus::main_entry(args(&["salus", "grpc", "--addr", "user@127.0.0.1:50051"])).await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_rejects_address_without_port() {
    let code = salus::main_entry(args(&["salus", "grpc", "--addr", "localhost"])).await;

    assert_eq!(code, 3);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_succeeds_with_tls_ca_file_and_server_name_override() {
    let addr = spawn_grpc_server(true).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        &addr.to_string(),
        "--tls",
        "--ca",
        TLS_CERT_PATH,
        "--server-name",
        "localhost",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_tls_probe_uses_authority_override() {
    let addr = spawn_grpc_tls_authority_server("example.test").await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        &addr.to_string(),
        "--tls",
        "--ca",
        TLS_CERT_PATH,
        "--server-name",
        "localhost",
        "--authority",
        "example.test",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_fails_with_tls_without_trust_override() {
    let addr = spawn_grpc_server(true).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        &addr.to_string(),
        "--tls",
    ]))
    .await;

    if cfg!(feature = "webpki") {
        assert_eq!(code, 1);
    } else {
        assert_eq!(code, 3);
    }
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_fails_with_wrong_server_name() {
    let addr = spawn_grpc_server(true).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        &addr.to_string(),
        "--tls",
        "--ca",
        TLS_CERT_PATH,
        "--server-name",
        "example.invalid",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[cfg(feature = "grpc")]
#[tokio::test]
async fn grpc_probe_succeeds_with_tls_and_insecure_skip_verify() {
    let addr = spawn_grpc_server(true).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--addr",
        &addr.to_string(),
        "--tls",
        "--insecure-skip-verify",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[cfg(feature = "grpc")]
async fn spawn_grpc_server(tls: bool) -> SocketAddr {
    spawn_grpc_server_with_service_status(tls, None).await
}

#[cfg(feature = "grpc")]
async fn spawn_grpc_authority_server(expected_authority: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = TokioIo::new(stream);
        let service = service_fn(move |request: Request<Incoming>| async move {
            let authority = request.uri().authority().map(|value| value.as_str());
            let status = if authority == Some(expected_authority) {
                200
            } else {
                421
            };

            Ok::<_, Infallible>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/grpc")
                    .header("grpc-status", "0")
                    .body(Full::new(Bytes::from_static(GRPC_HEALTH_SERVING_FRAME)))
                    .unwrap(),
            )
        });

        http2::Builder::new(TokioExecutor::new())
            .serve_connection(io, service)
            .await
            .unwrap();
    });

    addr
}

#[cfg(feature = "grpc")]
async fn spawn_grpc_tls_authority_server(expected_authority: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(tls_http2_server_config()));

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = acceptor.accept(stream).await.unwrap();
        let io = TokioIo::new(stream);
        let service = service_fn(move |request: Request<Incoming>| async move {
            let authority = request.uri().authority().map(|value| value.as_str());
            let status = if authority == Some(expected_authority) {
                200
            } else {
                421
            };

            Ok::<_, Infallible>(
                Response::builder()
                    .status(status)
                    .header("content-type", "application/grpc")
                    .header("grpc-status", "0")
                    .body(Full::new(Bytes::from_static(GRPC_HEALTH_SERVING_FRAME)))
                    .unwrap(),
            )
        });

        http2::Builder::new(TokioExecutor::new())
            .serve_connection(io, service)
            .await
            .unwrap();
    });

    addr
}

#[cfg(feature = "grpc")]
async fn spawn_grpc_server_with_service_status(
    tls: bool,
    service_status: Option<(&str, ServingStatus)>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (reporter, service) = tonic_health::server::health_reporter();

    if let Some((service_name, status)) = service_status {
        reporter
            .set_service_status(service_name.to_string(), status)
            .await;
    }

    let (ready_tx, ready_rx) = oneshot::channel();

    tokio::spawn(async move {
        let builder = tonic::transport::Server::builder();
        let mut builder = if tls {
            builder
                .tls_config(tonic::transport::ServerTlsConfig::new().identity(
                    tonic::transport::Identity::from_pem(TLS_CERT_PEM, TLS_KEY_PEM),
                ))
                .unwrap()
        } else {
            builder
        };

        let _ = ready_tx.send(());
        builder
            .add_service(service)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    ready_rx.await.unwrap();
    addr
}

async fn spawn_http_server(response: &str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let response = response.to_string();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 4096];
        let _ = stream.read(&mut buffer).await;
        let _ = stream.write_all(response.as_bytes()).await;
    });

    addr
}

async fn spawn_http_server_bytes(response: Vec<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 4096];
        let _ = stream.read(&mut buffer).await;
        let _ = stream.write_all(&response).await;
    });

    addr
}

async fn spawn_http_server_with_delay(response: &str, delay: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let response = response.to_string();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 4096];
        let _ = stream.read(&mut buffer).await;
        tokio::time::sleep(delay).await;
        let _ = stream.write_all(response.as_bytes()).await;
    });

    addr
}

async fn spawn_http_open_body_server(response_prefix: &str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let response_prefix = response_prefix.to_string();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 4096];
        let _ = stream.read(&mut buffer).await;
        let _ = stream.write_all(response_prefix.as_bytes()).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    addr
}

async fn spawn_https_server(response: &str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let response = response.to_string();
    let acceptor = TlsAcceptor::from(Arc::new(tls_server_config()));

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = acceptor.accept(stream).await.unwrap();
        let mut buffer = [0_u8; 4096];
        let _ = stream.read(&mut buffer).await;
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    });

    addr
}

async fn spawn_https_http2_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(tls_http2_server_config()));

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = acceptor.accept(stream).await.unwrap();
        let io = TokioIo::new(stream);
        let service = service_fn(|_request: Request<Incoming>| async move {
            Ok::<_, Infallible>(
                Response::builder()
                    .status(200)
                    .header("x-salus-protocol", "h2")
                    .body(Full::new(Bytes::from_static(b"ok")))
                    .unwrap(),
            )
        });

        http2::Builder::new(TokioExecutor::new())
            .serve_connection(io, service)
            .await
            .unwrap();
    });

    addr
}

async fn spawn_https_http2_authority_server(expected_authority: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(tls_http2_server_config()));

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = acceptor.accept(stream).await.unwrap();
        let io = TokioIo::new(stream);
        let service = service_fn(move |request: Request<Incoming>| async move {
            let authority = request.uri().authority().map(|value| value.as_str());
            let status = if authority == Some(expected_authority) {
                200
            } else {
                421
            };

            Ok::<_, Infallible>(
                Response::builder()
                    .status(status)
                    .body(Full::new(Bytes::from_static(b"ok")))
                    .unwrap(),
            )
        });

        http2::Builder::new(TokioExecutor::new())
            .serve_connection(io, service)
            .await
            .unwrap();
    });

    addr
}

#[cfg(unix)]
async fn spawn_http_uds_server(socket_path: &PathBuf, response: &str) {
    let _ = fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path).unwrap();
    let response = response.to_string();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 4096];
        let _ = stream.read(&mut buffer).await;
        let _ = stream.write_all(response.as_bytes()).await;
    });
}

fn temp_file_path(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{nanos}.tmp"))
}

fn tls_server_config() -> ServerConfig {
    let mut cert_reader = Cursor::new(TLS_CERT_PEM.as_bytes());
    let certs = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();

    let mut key_reader = Cursor::new(TLS_KEY_PEM.as_bytes());
    let key = rustls_pemfile::private_key(&mut key_reader)
        .unwrap()
        .expect("TLS test key must exist");

    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("TLS test certificate must be valid")
}

fn tls_http2_server_config() -> ServerConfig {
    let mut config = tls_server_config();
    config.alpn_protocols = vec![b"h2".to_vec()];
    config
}
