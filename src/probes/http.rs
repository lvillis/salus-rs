use std::path::Path;

use http_body_util::{BodyExt, Empty};
use hyper::{
    HeaderMap, Method, Request, Response, body::Bytes, body::Incoming, client::conn::http1,
    header::HOST, header::HeaderName, header::HeaderValue, header::USER_AGENT,
};
use hyper_rustls::{FixedServerNameResolver, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use tokio::net::UnixStream;
use url::Url;

use crate::{
    cli::{HttpArgs, TlsArgs},
    error::{AppError, Result},
    probe::{ProbeOptions, ProbeReport},
    tls::{build_http_tls_config, parse_server_name_override},
};

const USER_AGENT_VALUE: &str = concat!("salus/", env!("CARGO_PKG_VERSION"));

pub async fn run(
    options: ProbeOptions,
    args: &HttpArgs,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    validate_http_args(args)?;
    let prepared = PreparedHttpArgs::from_args(args)?;

    if let Some(sock) = args.sock.as_ref() {
        let path = args
            .path
            .as_deref()
            .ok_or_else(|| AppError::invalid_config("--path is required with --sock"))?;
        return run_unix_socket(options, args, prepared, sock, path, started).await;
    }

    let raw_url = args
        .url
        .as_deref()
        .ok_or_else(|| AppError::invalid_config("--url is required when --sock is not set"))?;
    let url = Url::parse(raw_url)
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

    let host = default_host_header(&url, args.host.as_deref())?;
    let uses_tls = url.scheme() == "https";

    if !uses_tls && has_tls_flags(&args.tls) {
        return Err(AppError::invalid_config(
            "TLS flags may only be used with https URLs",
        ));
    }

    let timeout = options.timeout;
    let result = tokio::time::timeout(timeout, async {
        let client = build_http_client(&args.tls)?;
        let request = build_request(raw_url, &host, &prepared)?;
        let response = client.request(request).await.map_err(|error| {
            AppError::failure(format!("HTTP request to {raw_url} failed: {error}"))
        })?;
        evaluate_response(response, raw_url, args, &prepared, started, options).await
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
    options: ProbeOptions,
    args: &HttpArgs,
    prepared: PreparedHttpArgs,
    sock: &Path,
    path: &str,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    if !path.starts_with('/') {
        return Err(AppError::invalid_config(
            "HTTP UDS --path must start with /",
        ));
    }

    let host = args.host.as_deref().unwrap_or("localhost");
    let target = format!("unix:{}{}", sock.display(), path);

    let timeout = options.timeout;
    let result = tokio::time::timeout(timeout, async {
        let stream = UnixStream::connect(sock).await.map_err(|error| {
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

        let request = build_request(path, host, &prepared)?;
        let response = sender.send_request(request).await.map_err(|error| {
            AppError::failure(format!(
                "HTTP request over unix socket {} failed: {error}",
                sock.display()
            ))
        })?;
        evaluate_response(response, &target, args, &prepared, started, options).await
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

fn build_http_client(
    tls: &TlsArgs,
) -> Result<
    Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Empty<Bytes>,
    >,
> {
    let tls_config = build_http_tls_config(tls)?;
    let builder = HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http();
    let builder = match tls.server_name.as_deref() {
        Some(server_name) => builder.with_server_name_resolver(FixedServerNameResolver::new(
            parse_server_name_override(server_name)?,
        )),
        None => builder,
    };
    let https = builder.enable_http1().enable_http2().build();
    Ok(Client::builder(TokioExecutor::new()).build(https))
}

fn build_request(
    uri: &str,
    host: &str,
    prepared: &PreparedHttpArgs,
) -> Result<Request<Empty<Bytes>>> {
    let mut builder = Request::builder().method(&prepared.method).uri(uri);
    builder = builder.header(HOST, host);
    builder = builder.header(USER_AGENT, USER_AGENT_VALUE);

    for (name, value) in &prepared.headers {
        builder = builder.header(name, value);
    }

    builder
        .body(Empty::new())
        .map_err(|error| AppError::invalid_config(format!("invalid HTTP request: {error}")))
}

async fn evaluate_response(
    response: Response<Incoming>,
    target: &str,
    args: &HttpArgs,
    prepared: &PreparedHttpArgs,
    started: std::time::Instant,
    options: ProbeOptions,
) -> Result<ProbeReport> {
    let status = response.status().as_u16();
    let response_headers = response.headers().clone();

    if !prepared.status_ranges.matches(status) {
        return Err(AppError::failure(format!(
            "HTTP status {} from {} is outside the allowed range",
            status, target
        )));
    }

    assert_response_headers(&response_headers, &prepared.header_assertions, target)?;

    if !has_body_assertions(args) {
        return Ok(ProbeReport::new(
            "http",
            target.to_string(),
            Some(format!("status={status}")),
            started,
            options,
        ));
    }

    let body = read_body(
        response.into_body(),
        args.max_body,
        requires_complete_body(args),
    )
    .await?;
    assert_response_body(&body, args, target)?;

    Ok(ProbeReport::new(
        "http",
        target.to_string(),
        Some(format!("status={status}")),
        started,
        options,
    ))
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

struct HeaderAssertions {
    present: Vec<HeaderName>,
    equals: Vec<HeaderAssertion>,
    contains: Vec<HeaderAssertion>,
    not_contains: Vec<HeaderAssertion>,
}

struct PreparedHttpArgs {
    method: Method,
    headers: Vec<(HeaderName, HeaderValue)>,
    header_assertions: HeaderAssertions,
    status_ranges: StatusRanges,
}

impl PreparedHttpArgs {
    fn from_args(args: &HttpArgs) -> Result<Self> {
        Ok(Self {
            method: parse_method(&args.method)?,
            headers: parse_headers(&args.header)?,
            header_assertions: HeaderAssertions {
                present: parse_header_names(&args.header_present)?,
                equals: parse_header_assertions(&args.header_equals)?,
                contains: parse_header_assertions(&args.header_contains)?,
                not_contains: parse_header_assertions(&args.header_not_contains)?,
            },
            status_ranges: parse_status_ranges(&args.status)?,
        })
    }
}

fn assert_response_headers(
    headers: &HeaderMap,
    assertions: &HeaderAssertions,
    target: &str,
) -> Result<()> {
    for name in &assertions.present {
        if headers.get(name).is_none() {
            return Err(AppError::failure(format!(
                "response header {} is missing from {}",
                name, target
            )));
        }
    }

    for assertion in &assertions.equals {
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

    for assertion in &assertions.contains {
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

    for assertion in &assertions.not_contains {
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
