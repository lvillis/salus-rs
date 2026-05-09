use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};

use hyper::http::{Uri, uri::Scheme};
use hyper_rustls::{FixedServerNameResolver, HttpsConnectorBuilder};
use tonic::transport::Endpoint;
use tonic_health::pb::{
    HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient,
};
use tower_service::Service;

use crate::{
    authority::{PortPolicy, RawFormat, validate_authority},
    cli::GrpcArgs,
    diagnostic,
    error::{AppError, Result},
    probe::{ProbeOptions, ProbeReport},
    tls::{build_http_tls_config, parse_server_name_override},
};

pub async fn run(
    options: ProbeOptions,
    args: &GrpcArgs,
    started: std::time::Instant,
) -> Result<ProbeReport> {
    if !args.tls && has_tls_flags(args) {
        return Err(AppError::invalid_config("gRPC TLS flags require --tls"));
    }
    validate_authority(
        &args.addr,
        "gRPC address",
        PortPolicy::Required,
        RawFormat::Debug,
    )?;

    let endpoint_uri = format!("http://{}", args.addr);
    let request_scheme = if args.tls { "https" } else { "http" };
    let target = match &args.service {
        Some(service) if !service.is_empty() => format!("{} service={service}", args.addr),
        _ => args.addr.clone(),
    };

    let timeout = options.timeout;
    let result = tokio::time::timeout(timeout, async {
        let mut endpoint = Endpoint::from_shared(endpoint_uri.clone()).map_err(|error| {
            AppError::invalid_config(format!("invalid gRPC endpoint {endpoint_uri}: {error}"))
        })?;
        endpoint = endpoint.connect_timeout(timeout).timeout(timeout);

        if let Some(authority) = &args.authority {
            validate_authority(
                authority,
                "gRPC authority",
                PortPolicy::Optional,
                RawFormat::Debug,
            )?;
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

        let channel = if args.tls {
            let tls_config = build_http_tls_config(&args.tls_args)?;
            let builder = HttpsConnectorBuilder::new()
                .with_tls_config(tls_config)
                .https_only();
            let builder = match args.tls_args.server_name.as_deref() {
                Some(server_name) => builder.with_server_name_resolver(
                    FixedServerNameResolver::new(parse_server_name_override(server_name)?),
                ),
                None => builder,
            };
            let connector = GrpcTlsConnector::new(builder.enable_http2().build());

            endpoint
                .connect_with_connector(connector)
                .await
                .map_err(|error| {
                    AppError::failure(format!(
                        "failed to connect to gRPC target {}: {error}",
                        args.addr
                    ))
                })?
        } else {
            endpoint.connect().await.map_err(|error| {
                AppError::failure(format!(
                    "failed to connect to gRPC target {}: {error}",
                    args.addr
                ))
            })?
        };

        let mut client = HealthClient::new(channel);
        let response = client
            .check(HealthCheckRequest {
                service: args.service.clone().unwrap_or_default(),
            })
            .await
            .map_err(|status| grpc_status_error(&args.addr, &status))?;

        let status = ServingStatus::try_from(response.get_ref().status).map_err(|_| {
            AppError::failure(format!(
                "gRPC health check for {} returned an unknown serving status",
                args.addr
            ))
        })?;

        if status != ServingStatus::Serving {
            return Err(AppError::failure(format!(
                "gRPC health check for {} returned {status:?}",
                args.addr
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
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(AppError::failure(format!(
            "gRPC probe timed out after {}",
            humantime::format_duration(timeout)
        ))),
    }
}

fn grpc_status_error(addr: &str, status: &tonic::Status) -> AppError {
    AppError::failure(format!(
        "gRPC health check for {} failed: code={} message={}",
        diagnostic::value(addr),
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

    use super::grpc_status_error;

    #[test]
    fn grpc_status_error_escapes_server_message_controls() {
        let status = tonic::Status::new(Code::Unavailable, "not ready\nnext");

        let error = grpc_status_error("127.0.0.1:50051", &status);

        assert!(error.to_string().contains(r#"message="not ready\nnext""#));
    }
}
