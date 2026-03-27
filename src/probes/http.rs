use std::path::PathBuf;

use http_body_util::{BodyExt, Empty};
use hyper::{
    HeaderMap, Method, Request, body::Bytes, client::conn::http1, header::HOST, header::HeaderName,
    header::HeaderValue, header::USER_AGENT,
};
use hyper_rustls::{FixedServerNameResolver, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use tokio::net::UnixStream;
use url::Url;

use crate::{
    cli::{Cli, HttpArgs, TlsArgs},
    error::{AppError, Result},
    probe::ProbeReport,
    tls::{build_http_tls_config, parse_server_name_override},
};

const USER_AGENT_VALUE: &str = concat!("salus/", env!("CARGO_PKG_VERSION"));

pub async fn run(cli: Cli, args: HttpArgs, started: std::time::Instant) -> Result<ProbeReport> {
    validate_http_args(&args)?;

    if let Some(sock) = args.sock.clone() {
        let path = args
            .path
            .clone()
            .ok_or_else(|| AppError::invalid_config("--path is required with --sock"))?;
        return run_unix_socket(cli, args, sock, path, started).await;
    }

    let raw_url = args
        .url
        .clone()
        .ok_or_else(|| AppError::invalid_config("--url is required when --sock is not set"))?;
    let url = Url::parse(&raw_url)
        .map_err(|error| AppError::invalid_config(format!("invalid URL {raw_url}: {error}")))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::invalid_config(format!(
            "unsupported URL scheme {}, expected http or https",
            url.scheme()
        )));
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::invalid_config(
            "URL user info is not supported for security reasons",
        ));
    }

    let method = parse_method(&args.method)?;
    let headers = parse_headers(&args.header)?;
    let header_present = parse_header_names(&args.header_present)?;
    let header_equals = parse_header_assertions(&args.header_equals)?;
    let header_contains = parse_header_assertions(&args.header_contains)?;
    let header_not_contains = parse_header_assertions(&args.header_not_contains)?;
    let host = default_host_header(&url, args.host.as_deref())?;
    let status_ranges = parse_status_ranges(&args.status)?;
    let uses_tls = url.scheme() == "https";

    if !uses_tls && has_tls_flags(&args.tls) {
        return Err(AppError::invalid_config(
            "TLS flags may only be used with https URLs",
        ));
    }

    let timeout = cli.timeout;
    let verbose_cli = cli.clone();
    let result = tokio::time::timeout(timeout, async {
        let tls_config = build_http_tls_config(&args.tls)?;
        let builder = HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http();
        let builder = match args.tls.server_name.as_deref() {
            Some(server_name) => builder.with_server_name_resolver(FixedServerNameResolver::new(
                parse_server_name_override(server_name)?,
            )),
            None => builder,
        };
        let https = builder.enable_http1().enable_http2().build();
        let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new()).build(https);

        let mut builder = Request::builder().method(method).uri(raw_url.as_str());
        builder = builder.header(HOST, host);
        builder = builder.header(USER_AGENT, USER_AGENT_VALUE);

        for (name, value) in headers {
            builder = builder.header(name, value);
        }

        let request = builder
            .body(Empty::new())
            .map_err(|error| AppError::invalid_config(format!("invalid HTTP request: {error}")))?;

        let response = client.request(request).await.map_err(|error| {
            AppError::failure(format!("HTTP request to {raw_url} failed: {error}"))
        })?;
        let status = response.status().as_u16();
        let response_headers = response.headers().clone();

        if !status_ranges.matches(status) {
            return Err(AppError::failure(format!(
                "HTTP status {} from {} is outside the allowed range",
                status, raw_url
            )));
        }

        assert_response_headers(
            &response_headers,
            &header_present,
            &header_equals,
            &header_contains,
            &header_not_contains,
            &raw_url,
        )?;

        if !has_body_assertions(&args) {
            return Ok::<_, AppError>(ProbeReport::new(
                "http",
                raw_url.clone(),
                Some(format!("status={status}")),
                started,
                verbose_cli.clone(),
            ));
        }

        let body = read_body(
            response.into_body(),
            args.max_body,
            requires_complete_body(&args),
        )
        .await?;
        assert_response_body(&body, &args, &raw_url)?;

        Ok::<_, AppError>(ProbeReport::new(
            "http",
            raw_url.clone(),
            Some(format!("status={status}")),
            started,
            verbose_cli.clone(),
        ))
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(AppError::failure(format!(
            "HTTP probe timed out after {}",
            humantime::format_duration(timeout)
        ))),
    }
}

async fn run_unix_socket(
    cli: Cli,
    args: HttpArgs,
    sock: PathBuf,
    path: String,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    if !path.starts_with('/') {
        return Err(AppError::invalid_config(
            "HTTP UDS --path must start with /",
        ));
    }

    let method = parse_method(&args.method)?;
    let headers = parse_headers(&args.header)?;
    let header_present = parse_header_names(&args.header_present)?;
    let header_equals = parse_header_assertions(&args.header_equals)?;
    let header_contains = parse_header_assertions(&args.header_contains)?;
    let header_not_contains = parse_header_assertions(&args.header_not_contains)?;
    let host = args.host.clone().unwrap_or_else(|| "localhost".to_string());
    let status_ranges = parse_status_ranges(&args.status)?;
    let target = format!("unix:{}{}", sock.display(), path);

    let timeout = cli.timeout;
    let verbose_cli = cli.clone();
    let result = tokio::time::timeout(timeout, async {
        let stream = UnixStream::connect(&sock).await.map_err(|error| {
            AppError::failure(format!(
                "failed to connect to unix socket {}: {error}",
                sock.display()
            ))
        })?;
        let io = TokioIo::new(stream);
        let (mut sender, connection) = http1::handshake::<_, Empty<Bytes>>(io)
            .await
            .map_err(|error| AppError::failure(format!("HTTP handshake failed: {error}")))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let mut builder = Request::builder().method(method).uri(path.as_str());
        builder = builder.header(HOST, host);
        builder = builder.header(USER_AGENT, USER_AGENT_VALUE);

        for (name, value) in headers {
            builder = builder.header(name, value);
        }

        let request = builder
            .body(Empty::new())
            .map_err(|error| AppError::invalid_config(format!("invalid HTTP request: {error}")))?;

        let response = sender.send_request(request).await.map_err(|error| {
            AppError::failure(format!(
                "HTTP request over unix socket {} failed: {error}",
                sock.display()
            ))
        })?;
        let status = response.status().as_u16();
        let response_headers = response.headers().clone();

        if !status_ranges.matches(status) {
            return Err(AppError::failure(format!(
                "HTTP status {} from {} is outside the allowed range",
                status, target
            )));
        }

        assert_response_headers(
            &response_headers,
            &header_present,
            &header_equals,
            &header_contains,
            &header_not_contains,
            &target,
        )?;

        if !has_body_assertions(&args) {
            return Ok::<_, AppError>(ProbeReport::new(
                "http",
                target,
                Some(format!("status={status}")),
                started,
                verbose_cli.clone(),
            ));
        }

        let body = read_body(
            response.into_body(),
            args.max_body,
            requires_complete_body(&args),
        )
        .await?;
        assert_response_body(&body, &args, &target)?;

        Ok::<_, AppError>(ProbeReport::new(
            "http",
            target,
            Some(format!("status={status}")),
            started,
            verbose_cli.clone(),
        ))
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(AppError::failure(format!(
            "HTTP probe timed out after {}",
            humantime::format_duration(timeout)
        ))),
    }
}

fn validate_http_args(args: &HttpArgs) -> Result<()> {
    match (&args.url, &args.sock) {
        (Some(_), Some(_)) => Err(AppError::invalid_config(
            "--url and --sock cannot be used together",
        )),
        (None, None) => Err(AppError::invalid_config(
            "either --url or --sock must be provided",
        )),
        _ => Ok(()),
    }?;

    if args.sock.is_some() {
        if has_tls_flags(&args.tls) {
            return Err(AppError::invalid_config(
                "TLS flags are not supported with --sock",
            ));
        }
    } else if args.path.is_some() {
        return Err(AppError::invalid_config(
            "--path may only be used with --sock",
        ));
    }

    if args.max_body == 0 && has_body_assertions(args) {
        return Err(AppError::invalid_config(
            "--max-body must be greater than 0 when body assertions are used",
        ));
    }

    if args.method == "HEAD" && has_body_assertions(args) {
        return Err(AppError::invalid_config(
            "HTTP body assertions are not supported with HEAD requests",
        ));
    }

    Ok(())
}

fn has_tls_flags(tls: &TlsArgs) -> bool {
    tls.ca.is_some()
        || tls.cert.is_some()
        || tls.key.is_some()
        || tls.server_name.is_some()
        || tls.insecure_skip_verify
}

fn parse_method(raw: &str) -> Result<Method> {
    match raw {
        "GET" => Ok(Method::GET),
        "HEAD" => Ok(Method::HEAD),
        other => Err(AppError::invalid_config(format!(
            "unsupported HTTP method {other}, expected GET or HEAD"
        ))),
    }
}

fn parse_headers(raw_headers: &[String]) -> Result<Vec<(HeaderName, HeaderValue)>> {
    let mut headers = Vec::with_capacity(raw_headers.len());
    for raw in raw_headers {
        let (name, value) = raw.split_once(':').ok_or_else(|| {
            AppError::invalid_config(format!("invalid header {raw:?}, expected name:value"))
        })?;
        let name = HeaderName::from_bytes(name.trim().as_bytes()).map_err(|error| {
            AppError::invalid_config(format!("invalid header name {name:?}: {error}"))
        })?;
        let value = HeaderValue::from_str(value.trim()).map_err(|error| {
            AppError::invalid_config(format!("invalid header value for {name}: {error}"))
        })?;
        headers.push((name, value));
    }
    Ok(headers)
}

fn parse_header_names(raw_names: &[String]) -> Result<Vec<HeaderName>> {
    let mut names = Vec::with_capacity(raw_names.len());
    for raw in raw_names {
        let name = HeaderName::from_bytes(raw.trim().as_bytes()).map_err(|error| {
            AppError::invalid_config(format!("invalid header name {raw:?}: {error}"))
        })?;
        names.push(name);
    }
    Ok(names)
}

fn parse_header_assertions(raw_assertions: &[String]) -> Result<Vec<HeaderAssertion>> {
    let mut assertions = Vec::with_capacity(raw_assertions.len());
    for raw in raw_assertions {
        let (name, value) = raw.split_once(':').ok_or_else(|| {
            AppError::invalid_config(format!(
                "invalid header assertion {raw:?}, expected name:value"
            ))
        })?;
        let name = HeaderName::from_bytes(name.trim().as_bytes()).map_err(|error| {
            AppError::invalid_config(format!("invalid header name {name:?}: {error}"))
        })?;
        assertions.push(HeaderAssertion {
            name,
            value: value.trim().to_string(),
        });
    }
    Ok(assertions)
}

fn default_host_header(url: &Url, override_host: Option<&str>) -> Result<String> {
    if let Some(host) = override_host {
        return Ok(host.to_string());
    }

    let host = url
        .host_str()
        .ok_or_else(|| AppError::invalid_config(format!("URL {} does not contain a host", url)))?;
    let port = url.port();
    let default = match url.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };

    Ok(match port {
        Some(port) if Some(port) != default => format!("{host}:{port}"),
        _ => host.to_string(),
    })
}

fn parse_status_ranges(raw_ranges: &[String]) -> Result<StatusRanges> {
    if raw_ranges.is_empty() {
        return Ok(StatusRanges(vec![StatusRange {
            start: 200,
            end: 299,
        }]));
    }

    let mut ranges = Vec::with_capacity(raw_ranges.len());
    for raw in raw_ranges {
        let range = if let Some((start, end)) = raw.split_once('-') {
            let start = parse_status_code(start)?;
            let end = parse_status_code(end)?;
            if start > end {
                return Err(AppError::invalid_config(format!(
                    "invalid status range {raw}, start is greater than end"
                )));
            }
            StatusRange { start, end }
        } else {
            let code = parse_status_code(raw)?;
            StatusRange {
                start: code,
                end: code,
            }
        };
        ranges.push(range);
    }
    Ok(StatusRanges(ranges))
}

fn parse_status_code(raw: &str) -> Result<u16> {
    let code = raw.parse::<u16>().map_err(|_| {
        AppError::invalid_config(format!("invalid status code {raw}, expected integer"))
    })?;
    if !(100..=599).contains(&code) {
        return Err(AppError::invalid_config(format!(
            "invalid status code {code}, expected 100..=599"
        )));
    }
    Ok(code)
}

struct StatusRanges(Vec<StatusRange>);

impl StatusRanges {
    fn matches(&self, code: u16) -> bool {
        self.0.iter().any(|range| range.contains(code))
    }
}

struct StatusRange {
    start: u16,
    end: u16,
}

impl StatusRange {
    fn contains(&self, code: u16) -> bool {
        self.start <= code && code <= self.end
    }
}

struct HeaderAssertion {
    name: HeaderName,
    value: String,
}

fn assert_response_headers(
    headers: &HeaderMap,
    present_assertions: &[HeaderName],
    equals_assertions: &[HeaderAssertion],
    contains_assertions: &[HeaderAssertion],
    not_contains_assertions: &[HeaderAssertion],
    target: &str,
) -> Result<()> {
    for name in present_assertions {
        if headers.get(name).is_none() {
            return Err(AppError::failure(format!(
                "response header {} is missing from {}",
                name, target
            )));
        }
    }

    for assertion in equals_assertions {
        let matches = headers
            .get_all(&assertion.name)
            .iter()
            .any(|value| String::from_utf8_lossy(value.as_bytes()) == assertion.value);
        if !matches {
            return Err(AppError::failure(format!(
                "response header {} from {} does not equal required value {:?}",
                assertion.name, target, assertion.value
            )));
        }
    }

    for assertion in contains_assertions {
        let matches = headers
            .get_all(&assertion.name)
            .iter()
            .any(|value| String::from_utf8_lossy(value.as_bytes()).contains(&assertion.value));
        if !matches {
            return Err(AppError::failure(format!(
                "response header {} from {} does not contain required text {:?}",
                assertion.name, target, assertion.value
            )));
        }
    }

    for assertion in not_contains_assertions {
        let matches = headers
            .get_all(&assertion.name)
            .iter()
            .any(|value| String::from_utf8_lossy(value.as_bytes()).contains(&assertion.value));
        if matches {
            return Err(AppError::failure(format!(
                "response header {} from {} contains forbidden text {:?}",
                assertion.name, target, assertion.value
            )));
        }
    }

    Ok(())
}

struct BufferedBody {
    bytes: Vec<u8>,
    truncated: bool,
}

fn has_body_assertions(args: &HttpArgs) -> bool {
    args.body_equals.is_some() || !args.contains.is_empty() || !args.not_contains.is_empty()
}

fn requires_complete_body(args: &HttpArgs) -> bool {
    args.body_equals.is_some() || !args.not_contains.is_empty()
}

fn assert_response_body(body: &BufferedBody, args: &HttpArgs, target: &str) -> Result<()> {
    let body_text = String::from_utf8_lossy(&body.bytes);

    if let Some(expected) = &args.body_equals {
        if body.truncated {
            return Err(AppError::failure(format!(
                "response body from {} was truncated at {} bytes, cannot prove exact body match",
                target, args.max_body
            )));
        }
        if body_text != expected.as_str() {
            return Err(AppError::failure(format!(
                "response body from {} does not equal required text {:?}",
                target, expected
            )));
        }
    }

    for needle in &args.contains {
        if !body_text.contains(needle) {
            if body.truncated {
                return Err(AppError::failure(format!(
                    "response body from {} was truncated at {} bytes, cannot prove required text {:?}",
                    target, args.max_body, needle
                )));
            }
            return Err(AppError::failure(format!(
                "response body from {} does not contain required text {:?}",
                target, needle
            )));
        }
    }

    if body.truncated && !args.not_contains.is_empty() {
        return Err(AppError::failure(
            "response body was truncated, cannot prove negative body assertions",
        ));
    }

    for needle in &args.not_contains {
        if body_text.contains(needle) {
            return Err(AppError::failure(format!(
                "response body from {} contains forbidden text {:?}",
                target, needle
            )));
        }
    }

    Ok(())
}

async fn read_body(
    mut body: hyper::body::Incoming,
    limit: usize,
    stop_on_limit: bool,
) -> Result<BufferedBody> {
    let mut bytes = Vec::new();
    let mut truncated = false;

    while let Some(frame) = body.frame().await {
        let frame = frame
            .map_err(|error| AppError::failure(format!("failed reading HTTP body: {error}")))?;
        if let Some(data) = frame.data_ref() {
            if bytes.len() < limit {
                let remaining = limit - bytes.len();
                if data.len() > remaining {
                    truncated = true;
                }
                let slice = &data[..data.len().min(remaining)];
                bytes.extend_from_slice(slice);
            } else if !data.is_empty() {
                truncated = true;
            }

            if truncated && stop_on_limit {
                break;
            }
        }
    }

    Ok(BufferedBody { bytes, truncated })
}
