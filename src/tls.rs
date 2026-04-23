use std::{fs::File, io::BufReader, path::Path, sync::Arc};

use rustls::{
    ClientConfig, RootCertStore,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
};
#[cfg(feature = "webpki")]
use webpki_roots::TLS_SERVER_ROOTS;

use crate::{
    cli::TlsArgs,
    error::{AppError, Result},
};

pub fn build_http_tls_config(tls: &TlsArgs) -> Result<ClientConfig> {
    build_client_config(tls)
}

pub fn parse_server_name_override(raw: &str) -> Result<ServerName<'static>> {
    let trimmed = raw
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(raw);

    ServerName::try_from(trimmed.to_string()).map_err(|error| {
        AppError::invalid_config(format!("invalid TLS server name override {raw:?}: {error}"))
    })
}

fn build_client_config(tls: &TlsArgs) -> Result<ClientConfig> {
    let roots = load_root_store(tls.ca.as_deref())?;
    let builder = rustls::ClientConfig::builder();

    let config = if tls.insecure_skip_verify {
        let verifier = Arc::new(InsecureVerifier);
        match load_client_identity(tls)? {
            Some((certs, key)) => builder
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_client_auth_cert(certs, key)
                .map_err(|error| {
                    AppError::invalid_config(format!("invalid client certificate or key: {error}"))
                })?,
            None => builder
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_no_client_auth(),
        }
    } else {
        match load_client_identity(tls)? {
            Some((certs, key)) => builder
                .with_root_certificates(roots)
                .with_client_auth_cert(certs, key)
                .map_err(|error| {
                    AppError::invalid_config(format!("invalid client certificate or key: {error}"))
                })?,
            None => builder.with_root_certificates(roots).with_no_client_auth(),
        }
    };

    Ok(config)
}

fn load_root_store(path: Option<&Path>) -> Result<RootCertStore> {
    let mut roots = RootCertStore::empty();

    #[cfg(feature = "webpki")]
    roots.extend(TLS_SERVER_ROOTS.iter().cloned());

    if let Some(path) = path {
        let file = File::open(path).map_err(|error| {
            AppError::invalid_config(format!(
                "failed to open CA file {}: {error}",
                path.display()
            ))
        })?;
        let mut reader = BufReader::new(file);
        let certs = rustls_pemfile::certs(&mut reader)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                AppError::invalid_config(format!(
                    "failed to parse CA file {}: {error}",
                    path.display()
                ))
            })?;

        if certs.is_empty() {
            return Err(AppError::invalid_config(format!(
                "CA file {} does not contain any certificates",
                path.display()
            )));
        }

        for cert in certs {
            roots.add(cert).map_err(|error| {
                AppError::invalid_config(format!(
                    "invalid CA certificate in {}: {error}",
                    path.display()
                ))
            })?;
        }
    }

    Ok(roots)
}

fn load_client_identity(
    tls: &TlsArgs,
) -> Result<Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>> {
    match (&tls.cert, &tls.key) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(AppError::invalid_config(
            "--cert and --key must be provided together",
        )),
        (Some(cert_path), Some(key_path)) => {
            let cert_file = File::open(cert_path).map_err(|error| {
                AppError::invalid_config(format!(
                    "failed to open client certificate {}: {error}",
                    cert_path.display()
                ))
            })?;
            let mut cert_reader = BufReader::new(cert_file);
            let certs = rustls_pemfile::certs(&mut cert_reader)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| {
                    AppError::invalid_config(format!(
                        "failed to parse client certificate {}: {error}",
                        cert_path.display()
                    ))
                })?;

            if certs.is_empty() {
                return Err(AppError::invalid_config(format!(
                    "client certificate {} does not contain any certificates",
                    cert_path.display()
                )));
            }

            let key_file = File::open(key_path).map_err(|error| {
                AppError::invalid_config(format!(
                    "failed to open client key {}: {error}",
                    key_path.display()
                ))
            })?;
            let mut key_reader = BufReader::new(key_file);
            let key = rustls_pemfile::private_key(&mut key_reader).map_err(|error| {
                AppError::invalid_config(format!(
                    "failed to parse client key {}: {error}",
                    key_path.display()
                ))
            })?;

            let key = key.ok_or_else(|| {
                AppError::invalid_config(format!(
                    "client key {} does not contain a supported private key",
                    key_path.display()
                ))
            })?;

            Ok(Some((certs, key)))
        }
    }
}

#[derive(Debug)]
struct InsecureVerifier;

impl ServerCertVerifier for InsecureVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme;

        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}
