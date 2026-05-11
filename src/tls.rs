use std::{fs::File, io::BufReader, net::Ipv6Addr, path::Path, sync::Arc};

use rustls::{
    ClientConfig, RootCertStore,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
};
#[cfg(feature = "webpki")]
use webpki_roots::TLS_SERVER_ROOTS;

use crate::{
    cli::TlsArgs,
    diagnostic,
    error::{AppError, Result},
};

pub fn build_http_tls_config(tls: &TlsArgs) -> Result<ClientConfig> {
    build_client_config(tls)
}

pub fn validate_tls_options(tls: &TlsArgs) -> Result<()> {
    if let Some(server_name) = tls.server_name.as_deref() {
        parse_server_name_override(server_name)?;
    }

    validate_client_identity_args(tls)?;
    validate_trust_args(tls)?;
    validate_tls_file_args(tls)?;
    validate_tls_trust_available(tls)
}

fn validate_client_identity_args(tls: &TlsArgs) -> Result<()> {
    match (&tls.cert, &tls.key) {
        (Some(_), None) | (None, Some(_)) => Err(AppError::invalid_config(
            "--cert and --key must be provided together",
        )),
        _ => Ok(()),
    }
}

fn validate_trust_args(tls: &TlsArgs) -> Result<()> {
    if tls.insecure_skip_verify && tls.ca.is_some() {
        return Err(AppError::invalid_config(
            "--ca cannot be used with --insecure-skip-verify",
        ));
    }

    Ok(())
}

fn validate_tls_file_args(tls: &TlsArgs) -> Result<()> {
    validate_optional_path("--ca", tls.ca.as_deref())?;
    validate_optional_path("--cert", tls.cert.as_deref())?;
    validate_optional_path("--key", tls.key.as_deref())
}

fn validate_optional_path(flag: &str, path: Option<&Path>) -> Result<()> {
    if path.is_some_and(|path| path.as_os_str().is_empty()) {
        return Err(AppError::invalid_config(format!(
            "{flag} must not be empty"
        )));
    }

    Ok(())
}

fn validate_tls_trust_available(tls: &TlsArgs) -> Result<()> {
    if tls.insecure_skip_verify || tls.ca.is_some() {
        return Ok(());
    }

    validate_bundled_roots_available()
}

pub fn parse_server_name_override(raw: &str) -> Result<ServerName<'static>> {
    let name = normalize_server_name_override(raw)?;

    ServerName::try_from(name.to_string()).map_err(|error| {
        AppError::invalid_config(format!("invalid TLS server name override {raw:?}: {error}"))
    })
}

fn normalize_server_name_override(raw: &str) -> Result<&str> {
    let Some(inner) = raw.strip_prefix('[') else {
        if raw.contains(['[', ']']) {
            return Err(invalid_bracketed_server_name(raw));
        }
        return Ok(raw);
    };

    let Some(inner) = inner.strip_suffix(']') else {
        return Err(invalid_bracketed_server_name(raw));
    };
    if inner.parse::<Ipv6Addr>().is_err() {
        return Err(invalid_bracketed_server_name(raw));
    }

    Ok(inner)
}

fn invalid_bracketed_server_name(raw: &str) -> AppError {
    AppError::invalid_config(format!(
        "invalid TLS server name override {raw:?}: brackets are only allowed around IPv6 addresses"
    ))
}

#[cfg(feature = "webpki")]
fn validate_bundled_roots_available() -> Result<()> {
    Ok(())
}

#[cfg(not(feature = "webpki"))]
fn validate_bundled_roots_available() -> Result<()> {
    Err(AppError::invalid_config(
        "TLS trust store is empty because bundled roots are disabled; provide --ca or --insecure-skip-verify",
    ))
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
        let file = open_regular_pem_file(path, "CA file")?;
        let mut reader = BufReader::new(file);
        let certs = rustls_pemfile::certs(&mut reader)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                AppError::invalid_config(format!(
                    "failed to parse CA file {}: {error}",
                    diagnostic::path(path)
                ))
            })?;

        if certs.is_empty() {
            return Err(AppError::invalid_config(format!(
                "CA file {} does not contain any certificates",
                diagnostic::path(path)
            )));
        }

        for cert in certs {
            roots.add(cert).map_err(|error| {
                AppError::invalid_config(format!(
                    "invalid CA certificate in {}: {error}",
                    diagnostic::path(path)
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
            let cert_file = open_regular_pem_file(cert_path, "client certificate")?;
            let mut cert_reader = BufReader::new(cert_file);
            let certs = rustls_pemfile::certs(&mut cert_reader)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|error| {
                    AppError::invalid_config(format!(
                        "failed to parse client certificate {}: {error}",
                        diagnostic::path(cert_path)
                    ))
                })?;

            if certs.is_empty() {
                return Err(AppError::invalid_config(format!(
                    "client certificate {} does not contain any certificates",
                    diagnostic::path(cert_path)
                )));
            }

            let key_file = open_regular_pem_file(key_path, "client key")?;
            let mut key_reader = BufReader::new(key_file);
            let key = rustls_pemfile::private_key(&mut key_reader).map_err(|error| {
                AppError::invalid_config(format!(
                    "failed to parse client key {}: {error}",
                    diagnostic::path(key_path)
                ))
            })?;

            let key = key.ok_or_else(|| {
                AppError::invalid_config(format!(
                    "client key {} does not contain a supported private key",
                    diagnostic::path(key_path)
                ))
            })?;

            Ok(Some((certs, key)))
        }
    }
}

fn open_regular_pem_file(path: &Path, label: &str) -> Result<File> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        AppError::invalid_config(format!(
            "failed to inspect {label} {}: {error}",
            diagnostic::path(path)
        ))
    })?;
    if !metadata.is_file() {
        return Err(AppError::invalid_config(format!(
            "{label} {} is not a regular file",
            diagnostic::path(path)
        )));
    }

    File::open(path).map_err(|error| {
        AppError::invalid_config(format!(
            "failed to open {label} {}: {error}",
            diagnostic::path(path)
        ))
    })
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::cli::TlsArgs;

    use super::{
        open_regular_pem_file, parse_server_name_override, validate_tls_options,
        validate_tls_trust_available,
    };

    fn temp_dir_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }

    fn create_temp_dir(prefix: &str) -> PathBuf {
        let path = temp_dir_path(prefix);
        fs::create_dir(&path).unwrap();
        path
    }

    fn remove_temp_dir(path: &Path) {
        let _ = fs::remove_dir(path);
    }

    #[test]
    fn server_name_override_accepts_bracketed_ipv6_literal() {
        let server_name = parse_server_name_override("[::1]").unwrap();

        assert_eq!(server_name.to_str(), "::1");
    }

    #[test]
    fn server_name_override_rejects_bracketed_dns_name() {
        let error = parse_server_name_override("[localhost]").unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TLS server name override \"[localhost]\": brackets are only allowed around IPv6 addresses"
        );
    }

    #[test]
    fn server_name_override_rejects_unmatched_bracket() {
        let error = parse_server_name_override("localhost]").unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TLS server name override \"localhost]\": brackets are only allowed around IPv6 addresses"
        );
    }

    #[test]
    fn tls_options_validate_server_name_before_trust_store() {
        let tls = TlsArgs {
            server_name: Some("[localhost]".to_string()),
            ..TlsArgs::default()
        };

        let error = validate_tls_options(&tls).unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid TLS server name override \"[localhost]\": brackets are only allowed around IPv6 addresses"
        );
    }

    #[test]
    fn tls_options_validate_client_identity_pair_before_trust_store() {
        let tls = TlsArgs {
            cert: Some(PathBuf::from("client.pem")),
            ..TlsArgs::default()
        };

        let error = validate_tls_options(&tls).unwrap_err();

        assert_eq!(
            error.to_string(),
            "--cert and --key must be provided together"
        );
    }

    #[test]
    fn tls_options_reject_conflicting_trust_controls_before_trust_store() {
        let tls = TlsArgs {
            ca: Some(PathBuf::from("ca.pem")),
            insecure_skip_verify: true,
            ..TlsArgs::default()
        };

        let error = validate_tls_options(&tls).unwrap_err();

        assert_eq!(
            error.to_string(),
            "--ca cannot be used with --insecure-skip-verify"
        );
    }

    #[test]
    fn tls_options_reject_empty_file_paths() {
        let tls = TlsArgs {
            ca: Some(PathBuf::new()),
            ..TlsArgs::default()
        };

        let error = validate_tls_options(&tls).unwrap_err();

        assert_eq!(error.to_string(), "--ca must not be empty");
    }

    #[test]
    fn tls_pem_files_must_be_regular_files() {
        let path = create_temp_dir("salus-tls-non-regular");

        let error = open_regular_pem_file(&path, "CA file").unwrap_err();

        assert!(error.to_string().contains("is not a regular file"));

        remove_temp_dir(&path);
    }

    #[test]
    fn tls_trust_is_available_with_custom_ca() {
        let tls = TlsArgs {
            ca: Some(PathBuf::from("ca.pem")),
            ..TlsArgs::default()
        };

        validate_tls_trust_available(&tls).unwrap();
    }

    #[test]
    fn tls_trust_is_available_when_verification_is_disabled() {
        let tls = TlsArgs {
            insecure_skip_verify: true,
            ..TlsArgs::default()
        };

        validate_tls_trust_available(&tls).unwrap();
    }

    #[cfg(feature = "webpki")]
    #[test]
    fn tls_trust_is_available_with_bundled_roots() {
        validate_tls_trust_available(&TlsArgs::default()).unwrap();
    }

    #[cfg(not(feature = "webpki"))]
    #[test]
    fn tls_trust_requires_ca_without_bundled_roots() {
        let error = validate_tls_trust_available(&TlsArgs::default()).unwrap_err();

        assert_eq!(
            error.to_string(),
            "TLS trust store is empty because bundled roots are disabled; provide --ca or --insecure-skip-verify"
        );
    }
}
