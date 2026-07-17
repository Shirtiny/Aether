use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use rustls::pki_types::pem;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::pem::SectionKind;
use rustls::pki_types::CertificateDer;
use rustls::ClientConfig;
use rustls::RootCertStore;
use thiserror::Error;

const CODEX_CA_CERTIFICATE: &str = "CODEX_CA_CERTIFICATE";
const SSL_CERT_FILE: &str = "SSL_CERT_FILE";
type PemSection = (SectionKind, Vec<u8>);

static PINNED_CODEX_KX_GROUPS: [&dyn rustls::crypto::SupportedKxGroup; 4] = [
    rustls::crypto::aws_lc_rs::kx_group::X25519,
    rustls::crypto::aws_lc_rs::kx_group::SECP256R1,
    rustls::crypto::aws_lc_rs::kx_group::SECP384R1,
    rustls::crypto::aws_lc_rs::kx_group::X25519MLKEM768,
];

#[derive(Debug, Error)]
pub enum ConnectorBuildError {
    #[error("failed to construct the pinned Codex rustls protocol configuration: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("failed to load the pinned Codex custom CA configuration: {0}")]
    CustomCa(String),
}

pub(crate) fn build_tls_config(roots: RootCertStore) -> Result<ClientConfig, ConnectorBuildError> {
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    // The embedding gateway also uses rustls and can enable its feature-gated PQ preference.
    // Preserve the pinned Codex build's no-PQ order independently of workspace feature unification.
    provider.kx_groups.clear();
    provider
        .kx_groups
        .extend_from_slice(&PINNED_CODEX_KX_GROUPS);
    let provider = Arc::new(provider);
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();

    // Codex sends no ALPN for this WebSocket path. Leave every other ClientConfig field,
    // including session resumption, at the rustls default used by the official client.
    config.alpn_protocols.clear();
    Ok(config)
}

/// Returns the immutable process trust template. Custom CA I/O and parsing happen once; every
/// physical connection still receives fresh rustls session state from this root template.
pub(crate) fn cached_codex_roots() -> Result<Arc<RootCertStore>, ConnectorBuildError> {
    static ROOTS: OnceLock<Result<Arc<RootCertStore>, String>> = OnceLock::new();
    match ROOTS.get_or_init(|| {
        build_roots_with_env((*cached_native_roots()).clone(), &ProcessEnv)
            .map(Arc::new)
            .map_err(|error| error.to_string())
    }) {
        Ok(roots) => Ok(Arc::clone(roots)),
        Err(error) => Err(ConnectorBuildError::CustomCa(error.clone())),
    }
}

fn cached_native_roots() -> Arc<RootCertStore> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    Arc::clone(ROOTS.get_or_init(|| {
        let mut roots = RootCertStore::empty();
        let result = rustls_native_certs::load_native_certs();
        let _ = roots.add_parsable_certificates(result.certs);
        Arc::new(roots)
    }))
}

trait EnvSource {
    fn var(&self, key: &str) -> Option<String>;

    fn configured_ca(&self) -> Option<ConfiguredCa> {
        self.non_empty_path(CODEX_CA_CERTIFICATE)
            .map(|path| ConfiguredCa {
                source_env: CODEX_CA_CERTIFICATE,
                path,
            })
            .or_else(|| {
                self.non_empty_path(SSL_CERT_FILE).map(|path| ConfiguredCa {
                    source_env: SSL_CERT_FILE,
                    path,
                })
            })
    }

    fn non_empty_path(&self, key: &str) -> Option<PathBuf> {
        self.var(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
}

struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }
}

struct ConfiguredCa {
    source_env: &'static str,
    path: PathBuf,
}

fn build_roots_with_env(
    mut roots: RootCertStore,
    env_source: &dyn EnvSource,
) -> Result<RootCertStore, ConnectorBuildError> {
    let Some(configured) = env_source.configured_ca() else {
        return Ok(roots);
    };
    for (index, certificate) in configured.certificates()?.into_iter().enumerate() {
        roots.add(certificate).map_err(|error| {
            ConnectorBuildError::CustomCa(format!(
                "failed to register certificate #{} from {} selected by {}: {error}",
                index + 1,
                configured.path.display(),
                configured.source_env
            ))
        })?;
    }
    Ok(roots)
}

impl ConfiguredCa {
    fn certificates(&self) -> Result<Vec<CertificateDer<'static>>, ConnectorBuildError> {
        let pem_data = fs::read(&self.path).map_err(|error| {
            ConnectorBuildError::CustomCa(format!(
                "failed to read {} selected by {}: {error}",
                self.path.display(),
                self.source_env
            ))
        })?;
        let normalized = NormalizedPem::new(&pem_data);
        let mut certificates = Vec::new();
        for section in normalized.sections() {
            let (kind, der) = section.map_err(|error| self.invalid_pem(error))?;
            if kind == SectionKind::Certificate {
                let der = normalized.certificate_der(&der).ok_or_else(|| {
                    ConnectorBuildError::CustomCa(format!(
                        "invalid DER length in {} selected by {}",
                        self.path.display(),
                        self.source_env
                    ))
                })?;
                certificates.push(CertificateDer::from(der.to_vec()));
            }
        }
        if certificates.is_empty() {
            return Err(ConnectorBuildError::CustomCa(format!(
                "no certificates found in {} selected by {}",
                self.path.display(),
                self.source_env
            )));
        }
        Ok(certificates)
    }

    fn invalid_pem(&self, error: pem::Error) -> ConnectorBuildError {
        ConnectorBuildError::CustomCa(format!(
            "failed to parse {} selected by {}: {error}",
            self.path.display(),
            self.source_env
        ))
    }
}

enum NormalizedPem {
    Standard(String),
    TrustedCertificate(String),
}

impl NormalizedPem {
    fn new(pem_data: &[u8]) -> Self {
        let pem = String::from_utf8_lossy(pem_data);
        if pem.contains("TRUSTED CERTIFICATE") {
            Self::TrustedCertificate(
                pem.replace("BEGIN TRUSTED CERTIFICATE", "BEGIN CERTIFICATE")
                    .replace("END TRUSTED CERTIFICATE", "END CERTIFICATE"),
            )
        } else {
            Self::Standard(pem.into_owned())
        }
    }

    fn contents(&self) -> &str {
        match self {
            Self::Standard(contents) | Self::TrustedCertificate(contents) => contents,
        }
    }

    fn sections(&self) -> impl Iterator<Item = Result<PemSection, pem::Error>> + '_ {
        PemSection::pem_slice_iter(self.contents().as_bytes())
    }

    fn certificate_der<'a>(&self, der: &'a [u8]) -> Option<&'a [u8]> {
        match self {
            Self::Standard(_) => Some(der),
            Self::TrustedCertificate(_) => first_der_item(der),
        }
    }
}

fn first_der_item(der: &[u8]) -> Option<&[u8]> {
    let &length_octet = der.get(1)?;
    if length_octet & 0x80 == 0 {
        let length = 2usize.checked_add(usize::from(length_octet))?;
        return der.get(..length);
    }
    let length_octets = usize::from(length_octet & 0x7f);
    if length_octets == 0 {
        return None;
    }
    let length_end = 2usize.checked_add(length_octets)?;
    let mut content_length = 0usize;
    for &byte in der.get(2..length_end)? {
        content_length = content_length
            .checked_mul(256)?
            .checked_add(usize::from(byte))?;
    }
    der.get(..length_end.checked_add(content_length)?)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use base64::Engine as _;
    use tempfile::TempDir;

    use super::*;

    const TEST_CERTIFICATE_DER_BASE64: &str = concat!(
        "MIIDQzCCAiugAwIBAgIUNL87ZdLGL3uDWZqqiMUxV1bU3cQwDQYJKoZIhvcNAQELBQAw",
        "FDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDcxNTE4MDgxN1oXDTM2MDcxMjE4MDgx",
        "N1owFDESMBAGA1UEAwwJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIB",
        "CgKCAQEAiZtaJCkE7SZT/vhScmyIv+GoQe7RqVx2JxKyEt5xUWpFf+lOMf/H6QAnvj67",
        "D7FfUuTyaqCEPvbC/pts5kET9BRgcy4gbeaEyz6CdVkMW2QkGtnXPtTT7POA1X5dHIAe",
        "JAh3Sn+cb7f9PYSbxPlCxTbvxLoxChV3ORlGeYntrzebcRLlOlSYF+rQs1UfL3zAZdn9",
        "0mXWYeG5qENDCyr67nQDGqPqeh1OEHESxVKRyUETL+/xmmfnn1wd9y8mx8XDGvlbG4Dm",
        "dKiUW1Y9O3q/9/YYRolwnkiVFSTrvBaSgVJUGDz+tZsZk86fQF3SYrgWBZuihnfJmasV",
        "4nNsHoyjowIDAQABo4GMMIGJMB0GA1UdDgQWBBQgJoBF30JyWs05bhczOyaahvFirjAf",
        "BgNVHSMEGDAWgBQgJoBF30JyWs05bhczOyaahvFirjAUBgNVHREEDTALgglsb2NhbGhv",
        "c3QwDAYDVR0TAQH/BAIwADAOBgNVHQ8BAf8EBAMCBaAwEwYDVR0lBAwwCgYIKwYBBQUH",
        "AwEwDQYJKoZIhvcNAQELBQADggEBADW9Y/Nq/p2c1Ye+yRxypZDhDlysZTPbBcwYGm/b",
        "vV0ZZ8AX+F6PsgBXOSBqUx/Y08Nn96nYYINzM90jo18tnme+N/kZnbDvJ5v7L32Mw17S",
        "UXO88pP+lIbWWMnqmz19l+eWvXhhc5sJacHsWHBZJIKUNMJLrGrAO3Y0gxNBvWWOP+pc",
        "GR/+cthftfXOr/Sd+G8mc8jbxwPbnDtZMZR0hd7yjhtgx+LcnYWspurMZQ/QMSHN1k2S",
        "f+qh6N7UV5/Ph9ZrYcwwjHyKY9PVStm+GzcWtPHbbloOw4GLLlJ2tde7Gj3KnMxouhmk",
        "Kp7M+iyPI3p+mY9cT8TCPj+VKic6juc="
    );

    struct MapEnv(HashMap<String, String>);

    impl EnvSource for MapEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    fn map_env(entries: &[(&str, &Path)]) -> MapEnv {
        MapEnv(
            entries
                .iter()
                .map(|(name, path)| ((*name).to_string(), path.to_string_lossy().into_owned()))
                .collect(),
        )
    }

    fn write_test_pem(directory: &TempDir, name: &str) -> PathBuf {
        let path = directory.path().join(name);
        let pem = format!(
            "-----BEGIN CERTIFICATE-----\n{TEST_CERTIFICATE_DER_BASE64}\n-----END CERTIFICATE-----\n"
        );
        fs::write(&path, pem).expect("test PEM should be written");
        path
    }

    fn test_certificate() -> CertificateDer<'static> {
        CertificateDer::from(
            base64::engine::general_purpose::STANDARD
                .decode(TEST_CERTIFICATE_DER_BASE64)
                .expect("test certificate should decode"),
        )
    }

    #[test]
    fn pinned_tls_config_has_no_alpn() {
        let config = build_tls_config(RootCertStore::empty()).expect("TLS config should build");
        assert!(config.alpn_protocols.is_empty());
    }

    #[test]
    fn codex_ca_certificate_takes_precedence_over_ssl_cert_file() {
        let directory = TempDir::new().expect("tempdir");
        let valid = write_test_pem(&directory, "codex.pem");
        let invalid = directory.path().join("invalid.pem");
        fs::write(&invalid, "not a certificate").expect("invalid PEM should be written");
        let env = map_env(&[
            (CODEX_CA_CERTIFICATE, valid.as_path()),
            (SSL_CERT_FILE, invalid.as_path()),
        ]);

        let roots = build_roots_with_env(RootCertStore::empty(), &env)
            .expect("Codex-specific CA should win");
        assert_eq!(roots.len(), 1);
    }

    #[test]
    fn invalid_selected_custom_ca_fails_closed() {
        let directory = TempDir::new().expect("tempdir");
        let invalid = directory.path().join("invalid.pem");
        fs::write(&invalid, "not a certificate").expect("invalid PEM should be written");
        let env = map_env(&[(CODEX_CA_CERTIFICATE, invalid.as_path())]);

        assert!(build_roots_with_env(RootCertStore::empty(), &env).is_err());
    }

    #[test]
    fn custom_ca_is_additive_to_native_roots() {
        let directory = TempDir::new().expect("tempdir");
        let custom = write_test_pem(&directory, "custom.pem");
        let env = map_env(&[(CODEX_CA_CERTIFICATE, custom.as_path())]);
        let mut native = RootCertStore::empty();
        native
            .add(test_certificate())
            .expect("native fixture should register");

        let roots = build_roots_with_env(native, &env).expect("custom CA should register");
        assert_eq!(roots.len(), 2);
    }
}
