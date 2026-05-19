use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use hyper::http::{Uri, uri::Scheme};
use hyper_rustls::{FixedServerNameResolver, HttpsConnectorBuilder};
use hyper_util::client::legacy::connect::HttpConnector;
use tonic::transport::Endpoint;
use tonic_health::pb::{
    HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient,
};
use tower_service::Service;

use crate::{
    authority::{PortPolicy, RawFormat, validate_authority},
    cli::{GrpcArgs, TlsArgs},
    diagnostic,
    error::{AppError, Result},
    probe::{ProbeOptions, ProbeReport, with_probe_timeout},
    tls::{build_http_tls_config, parse_server_name_override, validate_tls_options},
    validation::validate_non_empty_str,
};

pub async fn run(
    options: ProbeOptions,
    args: &GrpcArgs,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    validate_non_empty_str("--addr", &args.addr)?;
    if let Some(service) = args.service.as_deref() {
        validate_non_empty_str("--service", service)?;
    }
    if let Some(authority) = args.authority.as_deref() {
        validate_non_empty_str("--authority", authority)?;
    }
    validate_authority(
        &args.addr,
        "gRPC address",
        PortPolicy::Required,
        RawFormat::Debug,
    )?;
    if let Some(authority) = &args.authority {
        validate_authority(
            authority,
            "gRPC authority",
            PortPolicy::Optional,
            RawFormat::Debug,
        )?;
    }
    if !args.tls && has_tls_flags(args) {
        return Err(AppError::invalid_config("gRPC TLS flags require --tls"));
    }
    if args.tls {
        validate_tls_options(&args.tls_args)?;
    }

    let endpoint_uri = format!("http://{}", args.addr);
    let request_scheme = if args.tls { "https" } else { "http" };
    let target = grpc_target(&args.addr, args.service.as_deref());
    let endpoint = prepare_grpc_endpoint(args, &endpoint_uri, request_scheme, options.timeout)?;
    let tls_connector = if args.tls {
        Some(build_grpc_tls_connector(&args.tls_args)?)
    } else {
        None
    };

    let timeout = options.timeout;
    with_probe_timeout("gRPC", timeout, async move {
        let channel = connect_grpc_channel(endpoint, tls_connector, &args.addr).await?;

        let mut client = HealthClient::new(channel);
        let response = client
            .check(HealthCheckRequest {
                service: args.service.clone().unwrap_or_default(),
            })
            .await
            .map_err(|status| grpc_status_error(&target, &status))?;

        let status = ServingStatus::try_from(response.get_ref().status).map_err(|_| {
            AppError::failure(format!(
                "gRPC health check for {target} returned an unknown serving status"
            ))
        })?;

        if status != ServingStatus::Serving {
            return Err(AppError::failure(format!(
                "gRPC health check for {target} returned {status:?}"
            )));
        }

        Ok::<_, AppError>(ProbeReport::new(
            "grpc",
            target,
            Some("status=SERVING".to_string()),
            started,
            options,
        ))
    })
    .await
}

fn prepare_grpc_endpoint(
    args: &GrpcArgs,
    endpoint_uri: &str,
    request_scheme: &str,
    timeout: Duration,
) -> Result<Endpoint> {
    let mut endpoint = Endpoint::from_shared(endpoint_uri.to_string()).map_err(|error| {
        AppError::invalid_config(format!("invalid gRPC endpoint {endpoint_uri}: {error}"))
    })?;
    endpoint = endpoint.connect_timeout(timeout).timeout(timeout);

    if let Some(authority) = &args.authority {
        let origin = format!("{request_scheme}://{authority}")
            .parse()
            .map_err(|error| {
                AppError::invalid_config(format!("invalid gRPC authority {authority}: {error}"))
            })?;
        endpoint = endpoint.origin(origin);
    } else if args.tls {
        let origin = format!("https://{}", args.addr).parse().map_err(|error| {
            AppError::invalid_config(format!(
                "invalid gRPC TLS origin https://{}: {error}",
                args.addr
            ))
        })?;
        endpoint = endpoint.origin(origin);
    }

    Ok(endpoint)
}

type GrpcHttpsConnector = hyper_rustls::HttpsConnector<HttpConnector>;

fn build_grpc_tls_connector(tls: &TlsArgs) -> Result<GrpcTlsConnector<GrpcHttpsConnector>> {
    let tls_config = build_http_tls_config(tls)?;
    let builder = HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_only();
    let builder = match tls.server_name.as_deref() {
        Some(server_name) => builder.with_server_name_resolver(FixedServerNameResolver::new(
            parse_server_name_override(server_name)?,
        )),
        None => builder,
    };

    Ok(GrpcTlsConnector::new(builder.enable_http2().build()))
}

async fn connect_grpc_channel(
    endpoint: Endpoint,
    tls_connector: Option<GrpcTlsConnector<GrpcHttpsConnector>>,
    addr: &str,
) -> Result<tonic::transport::Channel> {
    match tls_connector {
        Some(connector) => endpoint
            .connect_with_connector(connector)
            .await
            .map_err(|error| {
                AppError::failure(format!("failed to connect to gRPC target {addr}: {error}"))
            }),
        None => endpoint.connect().await.map_err(|error| {
            AppError::failure(format!("failed to connect to gRPC target {addr}: {error}"))
        }),
    }
}

fn grpc_target(addr: &str, service: Option<&str>) -> String {
    match service {
        Some(service) if !service.is_empty() => {
            format!("{addr} service={}", diagnostic::value(service))
        }
        _ => addr.to_string(),
    }
}

fn grpc_status_error(target: &str, status: &tonic::Status) -> AppError {
    AppError::failure(format!(
        "gRPC health check for {target} failed: code={:?} message={}",
        status.code(),
        diagnostic::value(status.message())
    ))
}

type ConnectorFuture<T> = Pin<Box<dyn Future<Output = std::result::Result<T, io::Error>> + Send>>;

fn has_tls_flags(args: &GrpcArgs) -> bool {
    args.tls_args.ca.is_some()
        || args.tls_args.cert.is_some()
        || args.tls_args.key.is_some()
        || args.tls_args.server_name.is_some()
        || args.tls_args.insecure_skip_verify
}

#[derive(Clone)]
struct GrpcTlsConnector<C> {
    inner: C,
}

impl<C> GrpcTlsConnector<C> {
    fn new(inner: C) -> Self {
        Self { inner }
    }
}

impl<C> Service<Uri> for GrpcTlsConnector<C>
where
    C: Service<Uri> + Send + 'static,
    C::Response: Send + 'static,
    C::Future: Send + 'static,
    C::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Response = C::Response;
    type Error = io::Error;
    type Future = ConnectorFuture<Self::Response>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(io::Error::other)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let mut parts = uri.into_parts();
        parts.scheme = Some(Scheme::HTTPS);
        let uri = match Uri::from_parts(parts) {
            Ok(uri) => uri,
            Err(error) => {
                return Box::pin(std::future::ready(Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("failed to construct HTTPS gRPC URI: {error}"),
                ))));
            }
        };
        let future = self.inner.call(uri);
        Box::pin(async move { future.await.map_err(io::Error::other) })
    }
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::{grpc_status_error, grpc_target};

    #[test]
    fn grpc_status_error_escapes_server_message_controls() {
        let status = tonic::Status::new(Code::Unavailable, "not ready\nnext");

        let error = grpc_status_error("127.0.0.1:50051", &status);

        assert!(error.to_string().contains("code=Unavailable"));
        assert!(error.to_string().contains(r#"message="not ready\nnext""#));
    }

    #[test]
    fn grpc_target_includes_service_name() {
        assert_eq!(
            grpc_target("127.0.0.1:50051", Some("db")),
            "127.0.0.1:50051 service=db"
        );
    }

    #[test]
    fn grpc_target_escapes_service_name_controls() {
        assert_eq!(
            grpc_target("127.0.0.1:50051", Some("db\nnext")),
            "127.0.0.1:50051 service=\"db\\nnext\""
        );
    }

    #[test]
    fn grpc_target_omits_missing_service_name() {
        assert_eq!(grpc_target("127.0.0.1:50051", None), "127.0.0.1:50051");
    }
}
