use std::sync::OnceLock;

use serde::Deserialize;
use tungstenite::extensions::compression::deflate::DeflateConfig;
use tungstenite::extensions::ExtensionsConfig;
use tungstenite::protocol::WebSocketConfig;

pub const CODEX_WS_PROFILE_SCHEMA_VERSION: u64 = 3;
pub const CODEX_WS_PROFILE_ID: &str =
    "codex-ws-0.144.1-linux-x64-rustls023-aws-lc-caenv1-wbufret256k1";
pub const CODEX_WS_CONTINUATION_MODE: &str = "connection_local";
pub const CODEX_SOURCE_REVISION: &str = "1f0566d3f59298d1bb88820a0d35294f1eeb07ea";
pub const TOKIO_TUNGSTENITE_REVISION: &str = "0e5b2d73aa18dd9f0a50ee9ff199d5aef7594186";
pub const TUNGSTENITE_REVISION: &str = "4fffad30fe373adbdcffab9545e9e9bf4f2fc19f";
pub const TUNGSTENITE_PATCH_ID: &str = "aether-tungstenite-0.27-out-buffer-retention-v1";
pub const MAX_FRAME_SIZE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_MESSAGE_SIZE_BYTES: usize = 64 * 1024 * 1024;
pub const WRITE_BUFFER_SIZE_BYTES: usize = 128 * 1024;
pub const MAX_WRITE_BUFFER_SIZE_BYTES: usize = 17 * 1024 * 1024;
pub const MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES: usize = 256 * 1024;

pub const CODEX_WS_PROFILE_MANIFEST_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/profiles/codex-ws-0.144.1-linux-x64-rustls023-aws-lc-caenv1-wbufret256k1.json"
));

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CodexWsProfileManifest {
    pub schema_version: u64,
    pub profile_id: String,
    pub continuation_mode: String,
    pub source: CodexWsSourceManifest,
    pub dependencies: CodexWsDependencyManifest,
    pub tls: CodexWsTlsManifest,
    pub websocket: CodexWsProtocolManifest,
    pub proxy_routes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CodexWsSourceManifest {
    pub codex_version: String,
    pub codex_revision: String,
    pub connector_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CodexWsDependencyManifest {
    pub tokio_tungstenite_revision: String,
    pub tungstenite_revision: String,
    pub tungstenite_patch_id: String,
    pub rustls_version: String,
    pub rustls_webpki_version: String,
    pub rustls_native_certs_version: String,
    pub rustls_pki_types_version: String,
    pub tokio_rustls_version: String,
    pub aws_lc_rs_version: String,
    pub aws_lc_sys_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CodexWsTlsManifest {
    pub crypto_provider: String,
    pub root_store: String,
    pub client_config_scope: String,
    pub alpn_protocols: Vec<String>,
    pub session_resumption: String,
    pub custom_ca_env_precedence: Vec<String>,
    pub invalid_custom_ca_policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CodexWsProtocolManifest {
    pub permessage_deflate: bool,
    pub max_frame_size_bytes: usize,
    pub max_message_size_bytes: usize,
    pub write_buffer_size_bytes: usize,
    pub max_write_buffer_size_bytes: usize,
    pub max_retained_write_buffer_capacity_bytes: usize,
    pub tcp_nodelay: bool,
}

pub fn codex_ws_profile_manifest() -> &'static CodexWsProfileManifest {
    static MANIFEST: OnceLock<CodexWsProfileManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        serde_json::from_str(CODEX_WS_PROFILE_MANIFEST_JSON)
            .expect("embedded Codex WebSocket profile manifest must be valid")
    })
}

pub fn codex_websocket_config() -> WebSocketConfig {
    static TEMPLATE: OnceLock<WebSocketConfig> = OnceLock::new();
    *TEMPLATE.get_or_init(|| {
        let mut extensions = ExtensionsConfig::default();
        extensions.permessage_deflate = Some(DeflateConfig::default());

        let mut config = WebSocketConfig::default();
        config.extensions = extensions;
        config.write_buffer_size = WRITE_BUFFER_SIZE_BYTES;
        config.max_write_buffer_size = MAX_WRITE_BUFFER_SIZE_BYTES;
        config.max_retained_write_buffer_capacity = MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES;
        config.max_frame_size = Some(MAX_FRAME_SIZE_BYTES);
        config.max_message_size = Some(MAX_MESSAGE_SIZE_BYTES);
        config
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONNECTOR_CARGO_TOML: &str =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    const WORKSPACE_CARGO_LOCK: &str =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"));
    const WORKSPACE_CARGO_TOML: &str =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"));
    const VENDORED_TUNGSTENITE_CARGO_TOML: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/tungstenite/Cargo.toml"
    ));
    const VENDORED_TUNGSTENITE_028_CARGO_TOML: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/tungstenite-0.28.0/Cargo.toml"
    ));
    const VENDORED_AXUM_CARGO_TOML: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/axum-0.8.8/Cargo.toml"
    ));

    fn locked_package<'a>(
        packages: &'a [toml::Value],
        name: &str,
        version: &str,
    ) -> &'a toml::Value {
        let matches = packages
            .iter()
            .filter(|package| {
                package.get("name").and_then(toml::Value::as_str) == Some(name)
                    && package.get("version").and_then(toml::Value::as_str) == Some(version)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "{name} {version} must resolve to exactly one locked package"
        );
        matches[0]
    }

    fn assert_locked_dependency(
        packages: &[toml::Value],
        parent_name: &str,
        parent_version: &str,
        dependency_name: &str,
        dependency_version: &str,
    ) {
        let parent = locked_package(packages, parent_name, parent_version);
        let selectors = parent
            .get("dependencies")
            .and_then(toml::Value::as_array)
            .expect("locked package dependencies must be an array")
            .iter()
            .filter_map(toml::Value::as_str)
            .filter(|selector| {
                *selector == dependency_name
                    || selector
                        .strip_prefix(dependency_name)
                        .is_some_and(|suffix| suffix.starts_with(' '))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            selectors.len(),
            1,
            "{parent_name} {parent_version} must select exactly one {dependency_name} dependency"
        );

        let selector_version = selectors[0].split_whitespace().nth(1);
        if let Some(selector_version) = selector_version {
            assert_eq!(selector_version, dependency_version);
        } else {
            let same_name = packages
                .iter()
                .filter(|package| {
                    package.get("name").and_then(toml::Value::as_str) == Some(dependency_name)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                same_name.len(),
                1,
                "an unversioned {dependency_name} lock selector must be unambiguous"
            );
            assert_eq!(
                same_name[0].get("version").and_then(toml::Value::as_str),
                Some(dependency_version)
            );
        }

        let dependency = locked_package(packages, dependency_name, dependency_version);
        assert_eq!(
            dependency.get("source").and_then(toml::Value::as_str),
            Some("registry+https://github.com/rust-lang/crates.io-index"),
            "{dependency_name} {dependency_version} must use the pinned registry source"
        );
    }

    fn assert_pinned_git_dependency(
        dependencies: &toml::map::Map<String, toml::Value>,
        packages: &[toml::Value],
        name: &str,
        repository: &str,
        revision: &str,
    ) {
        let dependency = dependencies
            .get(name)
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| panic!("{name} must be a direct git dependency"));
        assert_eq!(
            dependency.get("git").and_then(toml::Value::as_str),
            Some(repository),
            "{name} must use the pinned fork"
        );
        assert_eq!(
            dependency.get("rev").and_then(toml::Value::as_str),
            Some(revision),
            "{name} must pin the exact reviewed revision"
        );

        let expected_source = format!("git+{repository}?rev={revision}#{revision}");
        let locked = packages
            .iter()
            .filter(|package| {
                package.get("name").and_then(toml::Value::as_str) == Some(name)
                    && package.get("source").and_then(toml::Value::as_str)
                        == Some(expected_source.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            locked.len(),
            1,
            "{name} must resolve exactly once from the reviewed git revision"
        );
    }

    fn assert_vendored_tungstenite_dependency(
        dependencies: &toml::map::Map<String, toml::Value>,
        packages: &[toml::Value],
        manifest: &CodexWsProfileManifest,
    ) {
        let dependency = dependencies
            .get("tungstenite")
            .and_then(toml::Value::as_table)
            .expect("tungstenite must be a direct path dependency");
        assert_eq!(
            dependency.get("path").and_then(toml::Value::as_str),
            Some("../../vendor/tungstenite")
        );
        assert!(dependency.get("git").is_none());
        assert!(dependency.get("rev").is_none());

        let vendor_manifest: toml::Value = toml::from_str(VENDORED_TUNGSTENITE_CARGO_TOML)
            .expect("vendored tungstenite Cargo.toml must parse");
        let vendor_identity = vendor_manifest
            .get("package")
            .and_then(|package| package.get("metadata"))
            .and_then(|metadata| metadata.get("aether-vendor"))
            .and_then(toml::Value::as_table)
            .expect("vendored tungstenite must record its source identity");
        assert_eq!(
            vendor_identity
                .get("base_repository")
                .and_then(toml::Value::as_str),
            Some("https://github.com/openai-oss-forks/tungstenite-rs")
        );
        assert_eq!(
            vendor_identity
                .get("base_revision")
                .and_then(toml::Value::as_str),
            Some(manifest.dependencies.tungstenite_revision.as_str())
        );
        assert_eq!(
            vendor_identity
                .get("patch_id")
                .and_then(toml::Value::as_str),
            Some(manifest.dependencies.tungstenite_patch_id.as_str())
        );

        let locked = locked_package(packages, "tungstenite", "0.27.0");
        assert!(
            locked.get("source").is_none(),
            "vendored tungstenite must resolve as a path package"
        );
    }

    fn assert_vendor_identity(
        cargo_toml: &str,
        expected_version: &str,
        expected_revision: &str,
        expected_checksum: &str,
        expected_patch_id: &str,
    ) {
        let manifest: toml::Value =
            toml::from_str(cargo_toml).expect("vendored Cargo.toml must parse");
        let package = manifest
            .get("package")
            .and_then(toml::Value::as_table)
            .expect("vendored package metadata must exist");
        assert_eq!(
            package.get("version").and_then(toml::Value::as_str),
            Some(expected_version)
        );
        let identity = package
            .get("metadata")
            .and_then(|metadata| metadata.get("aether-vendor"))
            .and_then(toml::Value::as_table)
            .expect("vendored source identity must exist");
        assert_eq!(
            identity
                .get("base_git_revision")
                .and_then(toml::Value::as_str),
            Some(expected_revision)
        );
        assert_eq!(
            identity
                .get("base_crates_io_checksum")
                .and_then(toml::Value::as_str),
            Some(expected_checksum)
        );
        assert_eq!(
            identity.get("patch_id").and_then(toml::Value::as_str),
            Some(expected_patch_id)
        );
    }

    #[test]
    fn embedded_manifest_matches_the_account_profile_contract() {
        let manifest = codex_ws_profile_manifest();

        assert_eq!(manifest.schema_version, CODEX_WS_PROFILE_SCHEMA_VERSION);
        assert_eq!(manifest.profile_id, CODEX_WS_PROFILE_ID);
        assert_eq!(manifest.continuation_mode, CODEX_WS_CONTINUATION_MODE);
        assert_eq!(manifest.source.codex_revision, CODEX_SOURCE_REVISION);
        assert_eq!(
            manifest.dependencies.tokio_tungstenite_revision,
            TOKIO_TUNGSTENITE_REVISION
        );
        assert_eq!(
            manifest.dependencies.tungstenite_revision,
            TUNGSTENITE_REVISION
        );
        assert_eq!(
            manifest.dependencies.tungstenite_patch_id,
            TUNGSTENITE_PATCH_ID
        );
        assert_eq!(manifest.dependencies.rustls_version, "0.23.36");
        assert_eq!(manifest.dependencies.rustls_webpki_version, "0.103.13");
        assert_eq!(manifest.dependencies.rustls_native_certs_version, "0.8.3");
        assert_eq!(manifest.dependencies.rustls_pki_types_version, "1.14.0");
        assert_eq!(manifest.dependencies.tokio_rustls_version, "0.26.4");
        assert_eq!(manifest.dependencies.aws_lc_rs_version, "1.16.2");
        assert_eq!(manifest.dependencies.aws_lc_sys_version, "0.39.0");
        assert_eq!(manifest.tls.crypto_provider, "aws-lc-rs");
        assert!(manifest.tls.alpn_protocols.is_empty());
        assert_eq!(manifest.tls.client_config_scope, "per-connection");
        assert_eq!(
            manifest.tls.custom_ca_env_precedence,
            ["CODEX_CA_CERTIFICATE", "SSL_CERT_FILE"]
        );
        assert_eq!(manifest.tls.invalid_custom_ca_policy, "fail-closed");
        assert_eq!(
            manifest.tls.session_resumption,
            "rustls-default-empty-at-build"
        );
    }

    #[test]
    fn embedded_manifest_matches_exact_cargo_constraints_and_lock_resolution() {
        let manifest = codex_ws_profile_manifest();
        let cargo_manifest: toml::Value =
            toml::from_str(CONNECTOR_CARGO_TOML).expect("connector Cargo.toml must parse");
        let dependencies = cargo_manifest
            .get("dependencies")
            .and_then(toml::Value::as_table)
            .expect("connector dependencies must be a table");
        let cargo_lock: toml::Value =
            toml::from_str(WORKSPACE_CARGO_LOCK).expect("workspace Cargo.lock must parse");
        let locked_packages = cargo_lock
            .get("package")
            .and_then(toml::Value::as_array)
            .expect("workspace lock packages must be an array");

        assert_pinned_git_dependency(
            dependencies,
            locked_packages,
            "tokio-tungstenite",
            "https://github.com/openai-oss-forks/tokio-tungstenite",
            &manifest.dependencies.tokio_tungstenite_revision,
        );
        assert_vendored_tungstenite_dependency(dependencies, locked_packages, manifest);

        let expected = [
            ("rustls", manifest.dependencies.rustls_version.as_str()),
            (
                "rustls-webpki",
                manifest.dependencies.rustls_webpki_version.as_str(),
            ),
            (
                "rustls-native-certs",
                manifest.dependencies.rustls_native_certs_version.as_str(),
            ),
            (
                "rustls-pki-types",
                manifest.dependencies.rustls_pki_types_version.as_str(),
            ),
            (
                "tokio-rustls",
                manifest.dependencies.tokio_rustls_version.as_str(),
            ),
            (
                "aws-lc-rs",
                manifest.dependencies.aws_lc_rs_version.as_str(),
            ),
            (
                "aws-lc-sys",
                manifest.dependencies.aws_lc_sys_version.as_str(),
            ),
        ];

        for (package, version) in expected {
            let dependency = dependencies
                .get(package)
                .unwrap_or_else(|| panic!("{package} must be a direct pinned dependency"));
            let requirement = dependency
                .as_str()
                .or_else(|| dependency.get("version").and_then(toml::Value::as_str))
                .unwrap_or_else(|| panic!("{package} must declare an exact version"));
            assert_eq!(requirement, format!("={version}"));

            assert_locked_dependency(
                locked_packages,
                "aether-codex-ws-connector",
                "0.1.0",
                package,
                version,
            );
        }

        assert_locked_dependency(
            locked_packages,
            "rustls",
            "0.23.36",
            "rustls-webpki",
            "0.103.13",
        );
        assert_locked_dependency(locked_packages, "rustls", "0.23.36", "aws-lc-rs", "1.16.2");
        assert_locked_dependency(
            locked_packages,
            "tokio-rustls",
            "0.26.4",
            "rustls",
            "0.23.36",
        );
        assert_locked_dependency(
            locked_packages,
            "rustls-native-certs",
            "0.8.3",
            "rustls-pki-types",
            "1.14.0",
        );
        assert_locked_dependency(
            locked_packages,
            "rustls-webpki",
            "0.103.13",
            "rustls-pki-types",
            "1.14.0",
        );
        assert_locked_dependency(
            locked_packages,
            "rustls-webpki",
            "0.103.13",
            "aws-lc-rs",
            "1.16.2",
        );
        assert_locked_dependency(
            locked_packages,
            "aws-lc-rs",
            "1.16.2",
            "aws-lc-sys",
            "0.39.0",
        );
    }

    #[test]
    fn downstream_websocket_stack_uses_reviewed_vendored_retention_patches() {
        const TUNGSTENITE_028_PATCH_ID: &str = "aether-tungstenite-0.28-out-buffer-retention-v1";
        const AXUM_PATCH_ID: &str = "aether-axum-0.8.8-ws-retention-config-v1";
        let workspace: toml::Value =
            toml::from_str(WORKSPACE_CARGO_TOML).expect("workspace Cargo.toml must parse");
        let workspace_excludes = workspace
            .get("workspace")
            .and_then(|workspace| workspace.get("exclude"))
            .and_then(toml::Value::as_array)
            .expect("reviewed vendor crates must be excluded from workspace-wide tooling");
        for vendor_path in [
            "vendor/axum-0.8.8",
            "vendor/tungstenite",
            "vendor/tungstenite-0.28.0",
        ] {
            assert!(
                workspace_excludes
                    .iter()
                    .any(|path| path.as_str() == Some(vendor_path)),
                "{vendor_path} must stay outside workspace-wide formatting"
            );
        }
        let crates_io_patch = workspace
            .get("patch")
            .and_then(|patch| patch.get("crates-io"))
            .and_then(toml::Value::as_table)
            .expect("workspace must patch the downstream WebSocket crates");
        assert_eq!(
            crates_io_patch
                .get("tungstenite")
                .and_then(|dependency| dependency.get("path"))
                .and_then(toml::Value::as_str),
            Some("vendor/tungstenite-0.28.0")
        );
        assert_eq!(
            crates_io_patch
                .get("axum")
                .and_then(|dependency| dependency.get("path"))
                .and_then(toml::Value::as_str),
            Some("vendor/axum-0.8.8")
        );

        assert_vendor_identity(
            VENDORED_TUNGSTENITE_028_CARGO_TOML,
            "0.28.0",
            "2d4abe8dba23b283c1a3d2f4f4937c2f9a8d91e7",
            "8628dcc84e5a09eb3d8423d6cb682965dea9133204e8fb3efee74c2a0c259442",
            TUNGSTENITE_028_PATCH_ID,
        );
        assert_vendor_identity(
            VENDORED_AXUM_CARGO_TOML,
            "0.8.8",
            "d07863f97d2649c414d2cdd162d1a10750e29a25",
            "8b52af3cb4058c895d37317bb27508dccc8e5f2d39454016b297bf4a400597b8",
            AXUM_PATCH_ID,
        );

        let cargo_lock: toml::Value =
            toml::from_str(WORKSPACE_CARGO_LOCK).expect("workspace Cargo.lock must parse");
        let locked_packages = cargo_lock
            .get("package")
            .and_then(toml::Value::as_array)
            .expect("workspace lock packages must be an array");
        for (name, version) in [("tungstenite", "0.28.0"), ("axum", "0.8.8")] {
            assert!(
                locked_package(locked_packages, name, version)
                    .get("source")
                    .is_none(),
                "{name} {version} must resolve from the reviewed path vendor"
            );
        }
    }

    #[test]
    fn websocket_template_enables_deflate_and_bounds_write_buffer_retention() {
        let config = codex_websocket_config();

        assert!(config.extensions.permessage_deflate.is_some());
        assert_eq!(config.write_buffer_size, WRITE_BUFFER_SIZE_BYTES);
        assert_eq!(config.max_write_buffer_size, MAX_WRITE_BUFFER_SIZE_BYTES);
        assert_eq!(
            config.max_retained_write_buffer_capacity,
            MAX_RETAINED_WRITE_BUFFER_CAPACITY_BYTES
        );
        assert!(config.max_write_buffer_size > MAX_FRAME_SIZE_BYTES);
        assert_eq!(config.max_frame_size, Some(MAX_FRAME_SIZE_BYTES));
        assert_eq!(config.max_message_size, Some(MAX_MESSAGE_SIZE_BYTES));
        assert!(!codex_ws_profile_manifest().websocket.tcp_nodelay);
    }
}
