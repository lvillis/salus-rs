use std::path::Path;

use http_body_util::{BodyExt, Empty};
use hyper::{
    HeaderMap, Method, Request, Response, Uri,
    body::{Body, Bytes, Incoming},
    client::conn::http1,
    header::CONTENT_LENGTH,
    header::HOST,
    header::HeaderName,
    header::HeaderValue,
    header::TRANSFER_ENCODING,
    header::USER_AGENT,
    http::uri::Authority,
};
use hyper_rustls::{FixedServerNameResolver, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
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
    if url.fragment().is_some() {
        return Err(AppError::invalid_config(
            "URL fragments are not supported in HTTP probes",
        ));
    }
    if url.port() == Some(0) {
        return Err(AppError::invalid_config(
            "URL port must be between 1 and 65535",
        ));
    }

    let host = default_host_header(&url, args.host.as_deref())?;
    let request_uri = url.as_str().to_string();
    let uses_tls = url.scheme() == "https";

    if !uses_tls && has_tls_flags(&args.tls) {
        return Err(AppError::invalid_config(
            "TLS flags may only be used with https URLs",
        ));
    }

    let timeout = options.timeout;
    let result = tokio::time::timeout(timeout, async {
        let client = build_http_client(&args.tls)?;
        let request = build_request(&request_uri, &host, &prepared)?;
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
    validate_uds_path(path)?;

    let host = match args.host.as_deref() {
        Some(host) => explicit_host_header(host)?,
        None => "localhost".to_string(),
    };
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
        let _connection_task = AbortOnDrop::new(tokio::spawn(async move {
            let _ = connection.await;
        }));

        let request = build_request(path, &host, &prepared)?;
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

struct AbortOnDrop<T> {
    handle: JoinHandle<T>,
}

impl<T> AbortOnDrop<T> {
    fn new(handle: JoinHandle<T>) -> Self {
        Self { handle }
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.handle.abort();
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
        if args
            .sock
            .as_ref()
            .is_some_and(|sock| sock.as_os_str().is_empty())
        {
            return Err(AppError::invalid_config("--sock must not be empty"));
        }
        if args.path.is_none() {
            return Err(AppError::invalid_config("--path is required with --sock"));
        }
        if let Some(path) = args.path.as_deref() {
            validate_uds_path(path)?;
        }

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

    validate_non_empty_assertions("--contains", &args.contains)?;
    validate_non_empty_assertions("--not-contains", &args.not_contains)?;

    if parse_method(&args.method)? == Method::HEAD && has_body_assertions(args) {
        return Err(AppError::invalid_config(
            "HTTP body assertions are not supported with HEAD requests",
        ));
    }

    Ok(())
}

fn validate_uds_path(path: &str) -> Result<()> {
    if !path.starts_with('/') {
        return Err(AppError::invalid_config(
            "HTTP UDS --path must start with /",
        ));
    }
    if path.contains('#') {
        return Err(AppError::invalid_config(
            "HTTP UDS --path must not contain a URL fragment",
        ));
    }

    let uri = path
        .parse::<Uri>()
        .map_err(|error| AppError::invalid_config(format!("invalid HTTP UDS --path: {error}")))?;
    if uri.scheme().is_some() || uri.authority().is_some() {
        return Err(AppError::invalid_config(
            "HTTP UDS --path must be an origin-form path, not an absolute URI",
        ));
    }
    if uri.path().is_empty() {
        return Err(AppError::invalid_config(
            "HTTP UDS --path must contain a path",
        ));
    }

    Ok(())
}

fn validate_non_empty_assertions(flag: &str, values: &[String]) -> Result<()> {
    if values.iter().any(String::is_empty) {
        return Err(AppError::invalid_config(format!(
            "{flag} must not be empty"
        )));
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
    if !prepared.headers.iter().any(|(name, _)| name == USER_AGENT) {
        builder = builder.header(USER_AGENT, USER_AGENT_VALUE);
    }

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
        early_body_contains_assertions(args),
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
    let method = raw.trim().to_ascii_uppercase();
    match method.as_str() {
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
        if name == HOST {
            return Err(AppError::invalid_config(
                "--header cannot set Host; use --host instead",
            ));
        }
        if name == CONTENT_LENGTH || name == TRANSFER_ENCODING {
            return Err(AppError::invalid_config(format!(
                "--header cannot set HTTP framing header {name}"
            )));
        }
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

fn parse_header_assertions(
    raw_assertions: &[String],
    flag: &str,
    allow_empty_value: bool,
) -> Result<Vec<HeaderAssertion>> {
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
        let value = value.trim();
        if !allow_empty_value && value.is_empty() {
            return Err(AppError::invalid_config(format!(
                "{flag} value must not be empty"
            )));
        }
        assertions.push(HeaderAssertion {
            name,
            value: value.to_string(),
        });
    }
    Ok(assertions)
}

fn default_host_header(url: &Url, override_host: Option<&str>) -> Result<String> {
    if let Some(host) = override_host {
        return explicit_host_header(host);
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

    let header = match port {
        Some(port) if Some(port) != default => format!("{host}:{port}"),
        _ => host.to_string(),
    };
    validate_host_header(&header)?;
    Ok(header)
}

fn explicit_host_header(raw: &str) -> Result<String> {
    let host = raw.trim();
    if host.is_empty() {
        return Err(AppError::invalid_config("--host must not be empty"));
    }

    validate_host_header(host)?;
    Ok(host.to_string())
}

fn validate_host_header(host: &str) -> Result<()> {
    if host.contains('@') {
        return Err(AppError::invalid_config(format!(
            "invalid Host header {host:?}: user info is not allowed"
        )));
    }

    let authority = host.parse::<Authority>().map_err(|error| {
        AppError::invalid_config(format!("invalid Host header {host:?}: {error}"))
    })?;
    if authority.host().is_empty() {
        return Err(AppError::invalid_config(format!(
            "invalid Host header {host:?}: host must not be empty"
        )));
    }

    let port_part = authority
        .as_str()
        .strip_prefix(authority.host())
        .unwrap_or_default();
    if !port_part.is_empty() && authority.port_u16().is_none() {
        return Err(AppError::invalid_config(format!(
            "invalid Host header {host:?}: port must be a valid integer"
        )));
    }
    if authority.port_u16() == Some(0) {
        return Err(AppError::invalid_config(format!(
            "invalid Host header {host:?}: port must be between 1 and 65535"
        )));
    }

    HeaderValue::from_str(host)
        .map(|_| ())
        .map_err(|error| AppError::invalid_config(format!("invalid Host header {host:?}: {error}")))
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
            let start = parse_status_code(start.trim())?;
            let end = parse_status_code(end.trim())?;
            if start > end {
                return Err(AppError::invalid_config(format!(
                    "invalid status range {raw}, start is greater than end"
                )));
            }
            StatusRange { start, end }
        } else {
            let code = parse_status_code(raw.trim())?;
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
                equals: parse_header_assertions(&args.header_equals, "--header-equals", true)?,
                contains: parse_header_assertions(
                    &args.header_contains,
                    "--header-contains",
                    false,
                )?,
                not_contains: parse_header_assertions(
                    &args.header_not_contains,
                    "--header-not-contains",
                    false,
                )?,
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
            .any(|value| value.as_bytes() == assertion.value.as_bytes());
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
            .any(|value| contains_bytes(value.as_bytes(), &assertion.value));
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
            .any(|value| contains_bytes(value.as_bytes(), &assertion.value));
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

fn early_body_contains_assertions(args: &HttpArgs) -> &[String] {
    if args.body_equals.is_none() && args.not_contains.is_empty() {
        &args.contains
    } else {
        &[]
    }
}

fn assert_response_body(body: &BufferedBody, args: &HttpArgs, target: &str) -> Result<()> {
    if let Some(expected) = &args.body_equals {
        if body.truncated {
            return Err(AppError::failure(format!(
                "response body from {} was truncated at {} bytes, cannot prove exact body match",
                target, args.max_body
            )));
        }
        if body.bytes != expected.as_bytes() {
            return Err(AppError::failure(format!(
                "response body from {} does not equal required text {:?}",
                target, expected
            )));
        }
    }

    for needle in &args.contains {
        if !contains_bytes(&body.bytes, needle) {
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
        if contains_bytes(&body.bytes, needle) {
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
    early_contains: &[String],
) -> Result<BufferedBody> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let limit_u64 = u64::try_from(limit).unwrap_or(u64::MAX);
    let body_size_upper = body.size_hint().upper();
    let declared_body_exceeds_limit = body_size_upper.is_some_and(|upper| upper > limit_u64);
    let body_may_exceed_limit = body_size_upper.is_none_or(|upper| upper > limit_u64);

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

            if !early_contains.is_empty() && contains_all(&bytes, early_contains) {
                break;
            }

            if truncated
                || (declared_body_exceeds_limit && bytes.len() == limit)
                || (!early_contains.is_empty() && bytes.len() == limit && body_may_exceed_limit)
            {
                truncated = true;
                break;
            }
        }
    }

    Ok(BufferedBody { bytes, truncated })
}

fn contains_all(bytes: &[u8], needles: &[String]) -> bool {
    needles.iter().all(|needle| contains_bytes(bytes, needle))
}

fn contains_bytes(bytes: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    needle.is_empty() || bytes.windows(needle.len()).any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::default_host_header;
    use url::Url;

    #[test]
    fn default_host_header_preserves_ipv6_brackets() {
        let url = Url::parse("http://[::1]:8080/healthz").unwrap();

        assert_eq!(default_host_header(&url, None).unwrap(), "[::1]:8080");
    }
}
