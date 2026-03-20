use std::{
    convert::Infallible,
    ffi::OsString,
    fs,
    io::Cursor,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
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
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};
use tokio_rustls::TlsAcceptor;
use tokio_stream::wrappers::TcpListenerStream;
use tonic_health::ServingStatus;

const TLS_CERT_PEM: &str = include_str!("fixtures/server.pem");
const TLS_KEY_PEM: &str = include_str!("fixtures/server.rsa");
const TLS_CERT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/server.pem");

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

    let code = salus::main_entry(args(&["salus", "tcp", "--address", &addr.to_string()])).await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn timeout_must_be_greater_than_zero() {
    let code = salus::main_entry(args(&[
        "salus",
        "--timeout",
        "0s",
        "tcp",
        "--address",
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
        "--body-contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn http_probe_sends_request_header() {
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
        "--request-header",
        "x-api-key:secret",
    ]))
    .await;

    assert_eq!(code, 0);
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
        "--response-header-contains",
        "x-ready:ok",
        "--response-header-not-contains",
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
        "--response-header-contains",
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
        "--unix-socket",
        &socket_path.display().to_string(),
        "--path",
        "/healthz",
        "--body-contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);

    let _ = fs::remove_file(socket_path);
}

#[tokio::test]
async fn https_probe_succeeds_with_ca_file_and_server_name_override() {
    let addr = spawn_https_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("https://{addr}/healthz"),
        "--ca-file",
        TLS_CERT_PATH,
        "--server-name",
        "localhost",
        "--body-contains",
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
        "--body-contains",
        "ok",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn https_probe_fails_without_trust_override_for_self_signed_server() {
    let addr = spawn_https_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("https://{addr}/healthz"),
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn https_probe_fails_with_wrong_server_name() {
    let addr = spawn_https_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;

    let code = salus::main_entry(args(&[
        "salus",
        "http",
        "--url",
        &format!("https://{addr}/healthz"),
        "--ca-file",
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
        "--ca-file",
        TLS_CERT_PATH,
        "--server-name",
        "localhost",
        "--body-contains",
        "ok",
        "--response-header-contains",
        "x-salus-protocol:h2",
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
        "--max-body-bytes",
        "4",
        "--body-not-contains",
        "zzz",
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
        "--max-body-bytes",
        "0",
        "--body-contains",
        "ok",
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
        "--body-contains",
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
        "--max-read-bytes",
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
        "--max-read-bytes",
        "0",
    ]))
    .await;

    assert_eq!(code, 3);

    let _ = fs::remove_file(path);
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
        "--max-read-bytes",
        "4",
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
        "--max-output-bytes",
        "0",
        "--",
        "sh",
        "-c",
        "printf ok",
    ]))
    .await;

    assert_eq!(code, 3);
}

#[cfg(unix)]
#[tokio::test]
async fn exec_probe_fails_when_command_is_terminated_by_signal() {
    let code = salus::main_entry(args(&["salus", "exec", "--", "sh", "-c", "kill -TERM $$"])).await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn grpc_probe_succeeds() {
    let addr = spawn_grpc_server(false).await;

    let code = salus::main_entry(args(&["salus", "grpc", "--address", &addr.to_string()])).await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn grpc_probe_fails_when_service_is_not_serving() {
    let addr =
        spawn_grpc_server_with_service_status(false, Some(("db", ServingStatus::NotServing))).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--address",
        &addr.to_string(),
        "--service",
        "db",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn grpc_probe_fails_when_service_is_unregistered() {
    let addr = spawn_grpc_server(false).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--address",
        &addr.to_string(),
        "--service",
        "db",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn grpc_probe_succeeds_with_tls_ca_file_and_server_name_override() {
    let addr = spawn_grpc_server(true).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--address",
        &addr.to_string(),
        "--tls",
        "--ca-file",
        TLS_CERT_PATH,
        "--server-name",
        "localhost",
    ]))
    .await;

    assert_eq!(code, 0);
}

#[tokio::test]
async fn grpc_probe_fails_with_tls_without_trust_override_for_self_signed_server() {
    let addr = spawn_grpc_server(true).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--address",
        &addr.to_string(),
        "--tls",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn grpc_probe_fails_with_wrong_server_name() {
    let addr = spawn_grpc_server(true).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--address",
        &addr.to_string(),
        "--tls",
        "--ca-file",
        TLS_CERT_PATH,
        "--server-name",
        "example.invalid",
    ]))
    .await;

    assert_eq!(code, 1);
}

#[tokio::test]
async fn grpc_probe_succeeds_with_tls_and_insecure_skip_verify() {
    let addr = spawn_grpc_server(true).await;

    let code = salus::main_entry(args(&[
        "salus",
        "grpc",
        "--address",
        &addr.to_string(),
        "--tls",
        "--insecure-skip-verify",
    ]))
    .await;

    assert_eq!(code, 0);
}

async fn spawn_grpc_server(tls: bool) -> SocketAddr {
    spawn_grpc_server_with_service_status(tls, None).await
}

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
