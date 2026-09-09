use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use aether_oauth::core::OAuthError;
use aether_oauth::network::{
    OAuthHttpExecutor, OAuthHttpRequest, OAuthHttpResponse, OAuthNetworkContext,
};
use aether_oauth::provider::ProviderOAuthTransportContext;
use aether_runtime_state::RuntimeState;
use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;

use super::generic_oauth::supports_local_generic_oauth_request_auth_resolution;
pub use super::generic_oauth::GenericOAuthRefreshAdapter;
use super::kiro::{
    supports_local_kiro_request_auth_resolution, KiroOAuthRefreshAdapter, KiroRequestAuth,
};
use super::snapshot::GatewayProviderTransportSnapshot;
use super::vertex::{
    supports_local_vertex_service_account_auth_resolution, VertexServiceAccountRefreshAdapter,
};

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum LocalResolvedOAuthRequestAuth {
    #[allow(dead_code)]
    Header {
        name: String,
        value: String,
    },
    Kiro(KiroRequestAuth),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalOAuthResolution {
    pub auth: Option<LocalResolvedOAuthRequestAuth>,
    pub refreshed_entry: Option<CachedOAuthEntry>,
    pub refresh_in_flight: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CachedOAuthEntry {
    pub provider_type: String,
    pub auth_header_name: String,
    pub auth_header_value: String,
    pub expires_at_unix_secs: Option<u64>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalOAuthHttpRequest {
    pub request_id: &'static str,
    pub method: reqwest::Method,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub json_body: Option<Value>,
    pub body_bytes: Option<Vec<u8>>,
    /// Account-scoped `User-Agent` to send on the request, carried from
    /// `OAuthHttpRequest.user_agent`. When set it is injected as the
    /// `user-agent` header (without overriding an explicit header already
    /// present in `headers`).
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOAuthHttpResponse {
    pub status_code: u16,
    pub body_text: String,
}

#[derive(Debug, Error)]
pub enum LocalOAuthRefreshError {
    #[error("{provider_type} oauth refresh request failed: {source}")]
    Transport {
        provider_type: &'static str,
        #[source]
        source: reqwest::Error,
    },
    #[error("{provider_type} oauth refresh returned HTTP {status_code}: {body_excerpt}")]
    HttpStatus {
        provider_type: &'static str,
        status_code: u16,
        body_excerpt: String,
    },
    #[error("{provider_type} oauth refresh transport failed: {message}")]
    TransportMessage {
        provider_type: &'static str,
        message: String,
    },
    #[error("{provider_type} oauth refresh returned invalid response: {message}")]
    InvalidResponse {
        provider_type: &'static str,
        message: String,
    },
}

#[async_trait]
pub trait LocalOAuthHttpExecutor: Send + Sync {
    async fn execute(
        &self,
        provider_type: &'static str,
        transport: &GatewayProviderTransportSnapshot,
        request: &LocalOAuthHttpRequest,
    ) -> Result<LocalOAuthHttpResponse, LocalOAuthRefreshError>;
}

#[derive(Debug, Clone)]
pub struct ReqwestLocalOAuthHttpExecutor {
    client: reqwest::Client,
}

impl ReqwestLocalOAuthHttpExecutor {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl LocalOAuthHttpExecutor for ReqwestLocalOAuthHttpExecutor {
    async fn execute(
        &self,
        provider_type: &'static str,
        _transport: &GatewayProviderTransportSnapshot,
        request: &LocalOAuthHttpRequest,
    ) -> Result<LocalOAuthHttpResponse, LocalOAuthRefreshError> {
        let mut builder = self
            .client
            .request(request.method.clone(), request.url.as_str());
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(ua) = request.user_agent.as_deref() {
            builder = builder.header("user-agent", ua);
        }
        if let Some(json_body) = request.json_body.as_ref() {
            builder = builder.json(json_body);
        } else if let Some(body_bytes) = request.body_bytes.as_ref() {
            builder = builder.body(body_bytes.clone());
        }

        let response =
            builder
                .send()
                .await
                .map_err(|source| LocalOAuthRefreshError::Transport {
                    provider_type,
                    source,
                })?;
        let status_code = response.status().as_u16();
        let body_text =
            response
                .text()
                .await
                .map_err(|source| LocalOAuthRefreshError::Transport {
                    provider_type,
                    source,
                })?;
        Ok(LocalOAuthHttpResponse {
            status_code,
            body_text,
        })
    }
}

pub(crate) struct ProviderOAuthLocalHttpExecutor<'a> {
    provider_type: &'static str,
    transport: &'a GatewayProviderTransportSnapshot,
    inner: &'a dyn LocalOAuthHttpExecutor,
}

impl<'a> ProviderOAuthLocalHttpExecutor<'a> {
    pub(crate) fn new(
        provider_type: &'static str,
        transport: &'a GatewayProviderTransportSnapshot,
        inner: &'a dyn LocalOAuthHttpExecutor,
    ) -> Self {
        Self {
            provider_type,
            transport,
            inner,
        }
    }
}

#[async_trait]
impl OAuthHttpExecutor for ProviderOAuthLocalHttpExecutor<'_> {
    async fn execute(&self, request: OAuthHttpRequest) -> Result<OAuthHttpResponse, OAuthError> {
        let response = self
            .inner
            .execute(
                self.provider_type,
                self.transport,
                &LocalOAuthHttpRequest {
                    request_id: "provider-oauth:local-refresh-token",
                    method: request.method,
                    url: request.url,
                    headers: request.headers,
                    json_body: request.json_body,
                    body_bytes: request.body_bytes,
                    user_agent: request.user_agent,
                },
            )
            .await
            .map_err(local_refresh_error_to_oauth_error)?;
        let json_body = serde_json::from_str::<Value>(&response.body_text).ok();
        Ok(OAuthHttpResponse {
            status_code: response.status_code,
            body_text: response.body_text,
            json_body,
        })
    }
}

/// Returns the `User-Agent` string assigned to this account's codex pool
/// profile, or `None` when the provider is not codex or no profile is
/// configured.  Mirrors the selection logic in the gateway planner so OAuth
/// maintenance traffic (token refresh) carries the same client fingerprint as
/// ordinary requests. Exposed for gateway-side logging of the selected UA.
pub fn resolve_oauth_maintenance_user_agent(
    transport: &GatewayProviderTransportSnapshot,
) -> Option<String> {
    resolve_oauth_maintenance_client_profile(transport).map(|profile| profile.user_agent)
}

/// Resolves the full client identity (`User-Agent` + `originator`) the
/// account's codex pool profile assigns. codex-rs sends both as default headers
/// on every auth call, so maintenance traffic must carry the pair the pool
/// traffic uses, not just the UA.
pub fn resolve_oauth_maintenance_client_profile(
    transport: &GatewayProviderTransportSnapshot,
) -> Option<CodexOAuthClientProfile> {
    codex_pool_account_client_profile(transport)
}

pub(crate) fn provider_oauth_transport_context_from_snapshot(
    transport: &GatewayProviderTransportSnapshot,
) -> ProviderOAuthTransportContext {
    let (user_agent, originator) = resolve_oauth_maintenance_client_profile(transport)
        .map(|profile| (Some(profile.user_agent), Some(profile.originator)))
        .unwrap_or((None, None));
    ProviderOAuthTransportContext {
        provider_id: transport.provider.id.clone(),
        provider_type: transport.provider.provider_type.clone(),
        endpoint_id: Some(transport.endpoint.id.clone()),
        key_id: Some(transport.key.id.clone()),
        auth_type: Some(transport.key.auth_type.clone()),
        decrypted_api_key: Some(transport.key.decrypted_api_key.clone()),
        decrypted_auth_config: transport.key.decrypted_auth_config.clone(),
        provider_config: transport.provider.config.clone(),
        endpoint_config: transport.endpoint.config.clone(),
        key_config: None,
        network: OAuthNetworkContext::provider_operation(None),
        user_agent,
        originator,
    }
}

/// Returns the `User-Agent` string assigned to this account's codex pool
/// profile, or `None` when the provider is not codex or no profile is
/// configured.  Mirrors the selection logic in
/// `ai_serving/planner/standard/codex.rs` so that OAuth maintenance traffic
/// (token refresh) carries the same client fingerprint as ordinary requests.
///
/// The built-in default profiles are embedded alongside the gateway planner's
/// `DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES_JSON` (same resource file) so the
/// two selection paths stay in sync even when `codex_client_headers.profiles`
/// is not configured.
const DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES_JSON: &str =
    include_str!("../../../../resources/codex-client-header-profiles.json");

/// A single codex client header profile: the `User-Agent` and `originator`
/// pair sent on the wire (both also feed profile-selection scoring).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexOAuthClientProfile {
    pub user_agent: String,
    pub originator: String,
}

type Profile = CodexOAuthClientProfile;

#[derive(serde::Deserialize)]
struct DefaultCodexClientHeaderProfile {
    #[serde(alias = "user-agent")]
    user_agent: String,
    originator: String,
}

/// Parses the built-in codex client header profiles resource. Mirrors the
/// gateway planner's `DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES_JSON` so a
/// missing `codex_client_headers.profiles` config still yields a stable UA.
fn default_codex_client_header_profiles() -> Option<Vec<Profile>> {
    let parsed: Vec<DefaultCodexClientHeaderProfile> =
        serde_json::from_str(DEFAULT_CODEX_POOL_CLIENT_HEADER_PROFILES_JSON).ok()?;
    let profiles = parsed
        .into_iter()
        .filter_map(|p| {
            let user_agent = p.user_agent.trim().to_string();
            let originator = p.originator.trim().to_string();
            if user_agent.is_empty() || originator.is_empty() {
                return None;
            }
            Some(Profile {
                user_agent,
                originator,
            })
        })
        .collect::<Vec<_>>();
    (!profiles.is_empty()).then_some(profiles)
}

fn codex_pool_account_client_profile(
    transport: &GatewayProviderTransportSnapshot,
) -> Option<Profile> {
    use sha2::{Digest, Sha256};

    if !transport
        .provider
        .provider_type
        .trim()
        .eq_ignore_ascii_case("codex")
    {
        return None;
    }

    let default_obj = serde_json::Value::Object(Default::default());
    let pool_advanced = transport
        .provider
        .config
        .as_ref()
        .and_then(|c| c.get("pool_advanced"))
        .unwrap_or(&default_obj);

    // Respect explicit disable.
    if pool_advanced
        .get("codex_client_headers")
        .and_then(|v| v.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        == Some(false)
    {
        return None;
    }

    // Collect configured profiles (same shape as planner codex.rs).
    let profiles: Vec<Profile> = pool_advanced
        .get("codex_client_headers")
        .and_then(|v| v.get("profiles"))
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let obj = p.as_object()?;
                    let ua = obj
                        .get("user_agent")
                        .or_else(|| obj.get("user-agent"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|v| !v.is_empty())?;
                    let orig = obj
                        .get("originator")
                        .and_then(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|v| !v.is_empty())?;
                    Some(Profile {
                        user_agent: ua.to_string(),
                        originator: orig.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let profiles = if profiles.is_empty() {
        let defaults = default_codex_client_header_profiles()?;
        defaults
    } else {
        profiles
    };

    // Selection key: first non-empty account identifier in auth_config, else
    // key.id, else key.name — identical to `codex_account_selection_key`.
    let selection_key: String = transport
        .key
        .decrypted_auth_config
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            [
                "account_id",
                "accountId",
                "chatgpt_account_id",
                "chatgptAccountId",
            ]
            .iter()
            .find_map(|key| {
                v.get(*key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(ToOwned::to_owned)
            })
        })
        .unwrap_or_else(|| {
            let kid = transport.key.id.trim();
            if !kid.is_empty() {
                kid.to_string()
            } else {
                transport.key.name.trim().to_string()
            }
        });

    // Pick profile by stable hash score (same algorithm as planner codex.rs).
    let selected_profile = {
        let best_index = profiles
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut h = Sha256::new();
                h.update(selection_key.as_bytes());
                h.update([0]);
                h.update(p.user_agent.as_bytes());
                h.update([0]);
                h.update(p.originator.as_bytes());
                let digest = h.finalize();
                let mut score = [0_u8; 32];
                score.copy_from_slice(&digest);
                (i, score)
            })
            .max_by(|(_, a), (_, b)| a.cmp(b))
            .map(|(i, _)| i)
            .unwrap_or(0);
        profiles[best_index].clone()
    };

    // Prefer the fingerprint's persisted profile when it belongs to this
    // account. Mirrors `resolve_codex_concrete_account_profile`: a stored
    // profile that matches the current selection identity wins over the
    // header-profile pick, so OAuth maintenance traffic keeps the same stable
    // UA/originator pair as requests. A persisted UA without an originator
    // (pre-originator fingerprints) keeps the selected originator.
    Some(match codex_profile_persisted_client_profile(transport) {
        Some((user_agent, originator)) => Profile {
            user_agent,
            originator: originator.unwrap_or(selected_profile.originator),
        },
        None => selected_profile,
    })
}

/// Returns the persisted `(user_agent, originator)` from the account's codex
/// profile fingerprint, when that fingerprint's selection identity matches the
/// current account. Mirrors the eligibility check in gateway `codex_profile.rs`
/// so the identity used for OAuth maintenance traffic is identical to the one
/// applied to ordinary requests.
fn codex_profile_persisted_client_profile(
    transport: &GatewayProviderTransportSnapshot,
) -> Option<(String, Option<String>)> {
    const CODEX_CLIENT_PROFILE_KEY: &str = "codex_client_profile";

    let fingerprint_profile = transport
        .key
        .fingerprint
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .and_then(|object| object.get(CODEX_CLIENT_PROFILE_KEY))
        .and_then(serde_json::Value::as_object)?;

    // The stored profile only applies to the account whose selection identity
    // (kind + hash) it was recorded for.
    let (selection_kind, selection_hash) = codex_profile_selection_identity(transport);
    let kind_matches = fingerprint_profile
        .get("selection_key_kind")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .is_some_and(|value| value == selection_kind);
    let hash_matches = fingerprint_profile
        .get("selection_key_hash")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .is_some_and(|value| value == selection_hash);
    if !(kind_matches && hash_matches) {
        return None;
    }

    let client_headers = fingerprint_profile
        .get("client_headers")
        .and_then(serde_json::Value::as_object);
    let user_agent = client_headers
        .and_then(|headers| {
            headers
                .get("user_agent")
                .or_else(|| headers.get("user-agent"))
        })
        .or_else(|| {
            fingerprint_profile
                .get("user_agent")
                .or_else(|| fingerprint_profile.get("user-agent"))
        })
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)?;
    let originator = client_headers
        .and_then(|headers| headers.get("originator"))
        .or_else(|| fingerprint_profile.get("originator"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Some((user_agent, originator))
}

/// Computes the codex account selection identity (kind + hash) exactly as
/// gateway `codex_profile.rs` does: `auth_account_id` when the auth config
/// carries an account identifier, else `key_id`, else `key_name`.
fn codex_profile_selection_identity(
    transport: &GatewayProviderTransportSnapshot,
) -> (&'static str, String) {
    use sha2::{Digest, Sha256};
    fn digest_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in &digest {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        format!("sha256:{out}")
    }
    let auth_account_id = transport
        .key
        .decrypted_auth_config
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            [
                "account_id",
                "accountId",
                "chatgpt_account_id",
                "chatgptAccountId",
            ]
            .iter()
            .find_map(|key| {
                v.get(*key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(ToOwned::to_owned)
            })
        });
    if let Some(account_id) = auth_account_id {
        return ("auth_account_id", digest_hex(account_id.as_bytes()));
    }
    let key_id = transport.key.id.trim();
    if !key_id.is_empty() {
        return ("key_id", digest_hex(key_id.as_bytes()));
    }
    ("key_name", digest_hex(transport.key.name.trim().as_bytes()))
}

pub(crate) fn oauth_error_to_local_refresh_error(
    provider_type: &'static str,
    error: OAuthError,
) -> LocalOAuthRefreshError {
    match error {
        OAuthError::HttpStatus {
            status_code,
            body_excerpt,
        } => LocalOAuthRefreshError::HttpStatus {
            provider_type,
            status_code,
            body_excerpt,
        },
        OAuthError::Transport(message) => LocalOAuthRefreshError::TransportMessage {
            provider_type,
            message,
        },
        OAuthError::InvalidRequest(message)
        | OAuthError::InvalidResponse(message)
        | OAuthError::Storage(message)
        | OAuthError::UnsupportedProvider(message) => LocalOAuthRefreshError::InvalidResponse {
            provider_type,
            message,
        },
        OAuthError::InvalidState => LocalOAuthRefreshError::InvalidResponse {
            provider_type,
            message: "oauth state is invalid or expired".to_string(),
        },
        OAuthError::EncryptionUnavailable => LocalOAuthRefreshError::InvalidResponse {
            provider_type,
            message: "oauth encryption unavailable".to_string(),
        },
    }
}

fn local_refresh_error_to_oauth_error(error: LocalOAuthRefreshError) -> OAuthError {
    match error {
        LocalOAuthRefreshError::Transport { source, .. } => {
            OAuthError::Transport(source.to_string())
        }
        LocalOAuthRefreshError::TransportMessage { message, .. } => OAuthError::Transport(message),
        LocalOAuthRefreshError::HttpStatus {
            status_code,
            body_excerpt,
            ..
        } => OAuthError::HttpStatus {
            status_code,
            body_excerpt,
        },
        LocalOAuthRefreshError::InvalidResponse { message, .. } => {
            OAuthError::InvalidResponse(message)
        }
    }
}

#[async_trait]
pub trait LocalOAuthRefreshAdapter: Send + Sync {
    fn provider_type(&self) -> &'static str;

    fn supports(&self, transport: &GatewayProviderTransportSnapshot) -> bool {
        transport
            .provider
            .provider_type
            .trim()
            .eq_ignore_ascii_case(self.provider_type())
    }

    fn resolve_cached(
        &self,
        transport: &GatewayProviderTransportSnapshot,
        entry: &CachedOAuthEntry,
    ) -> Option<LocalResolvedOAuthRequestAuth>;

    fn resolve_without_refresh(
        &self,
        transport: &GatewayProviderTransportSnapshot,
    ) -> Option<LocalResolvedOAuthRequestAuth>;

    fn should_refresh(
        &self,
        transport: &GatewayProviderTransportSnapshot,
        entry: Option<&CachedOAuthEntry>,
    ) -> bool;

    async fn refresh(
        &self,
        executor: &dyn LocalOAuthHttpExecutor,
        transport: &GatewayProviderTransportSnapshot,
        entry: Option<&CachedOAuthEntry>,
    ) -> Result<Option<CachedOAuthEntry>, LocalOAuthRefreshError>;
}

pub struct LocalOAuthRefreshCoordinator {
    adapters: Vec<Arc<dyn LocalOAuthRefreshAdapter>>,
    cache: Mutex<BTreeMap<String, CachedOAuthEntry>>,
    key_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}

impl fmt::Debug for LocalOAuthRefreshCoordinator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalOAuthRefreshCoordinator")
            .field("adapter_count", &self.adapters.len())
            .finish()
    }
}

impl Default for LocalOAuthRefreshCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalOAuthRefreshCoordinator {
    const DISTRIBUTED_REFRESH_LOCK_TTL_MS: u64 = 30_000;

    pub fn new() -> Self {
        Self {
            adapters: vec![
                Arc::new(KiroOAuthRefreshAdapter::default()),
                Arc::new(VertexServiceAccountRefreshAdapter),
                Arc::new(GenericOAuthRefreshAdapter::default()),
            ],
            cache: Mutex::new(BTreeMap::new()),
            key_locks: Mutex::new(BTreeMap::new()),
        }
    }

    async fn lock_for_key(&self, key_id: &str) -> Arc<Mutex<()>> {
        let mut key_locks = self.key_locks.lock().await;
        key_locks
            .entry(key_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn cached_entry(&self, key_id: &str) -> Option<CachedOAuthEntry> {
        self.cache.lock().await.get(key_id).cloned()
    }

    async fn insert_cached_entry(&self, key_id: &str, entry: CachedOAuthEntry) {
        self.cache.lock().await.insert(key_id.to_string(), entry);
    }

    pub async fn store_cached_entry(&self, key_id: &str, entry: CachedOAuthEntry) {
        self.insert_cached_entry(key_id, entry).await;
    }

    pub async fn invalidate_cached_entry(&self, key_id: &str) -> bool {
        self.cache.lock().await.remove(key_id).is_some()
    }

    pub async fn resolve_with_result(
        &self,
        executor: &dyn LocalOAuthHttpExecutor,
        transport: &GatewayProviderTransportSnapshot,
        distributed_lock: Option<&RuntimeState>,
        distributed_owner: Option<&str>,
    ) -> Result<Option<LocalOAuthResolution>, LocalOAuthRefreshError> {
        self.resolve_with_result_mode(
            executor,
            transport,
            distributed_lock,
            distributed_owner,
            false,
        )
        .await
    }

    pub async fn force_refresh_with_result(
        &self,
        executor: &dyn LocalOAuthHttpExecutor,
        transport: &GatewayProviderTransportSnapshot,
        distributed_lock: Option<&RuntimeState>,
        distributed_owner: Option<&str>,
    ) -> Result<Option<LocalOAuthResolution>, LocalOAuthRefreshError> {
        self.resolve_with_result_mode(
            executor,
            transport,
            distributed_lock,
            distributed_owner,
            true,
        )
        .await
    }

    async fn resolve_with_result_mode(
        &self,
        executor: &dyn LocalOAuthHttpExecutor,
        transport: &GatewayProviderTransportSnapshot,
        distributed_lock: Option<&RuntimeState>,
        distributed_owner: Option<&str>,
        force_refresh: bool,
    ) -> Result<Option<LocalOAuthResolution>, LocalOAuthRefreshError> {
        let Some(adapter) = self
            .adapters
            .iter()
            .find(|adapter| adapter.supports(transport))
        else {
            return Ok(None);
        };
        let key_id = transport.key.id.trim();

        let cached_entry = if key_id.is_empty() {
            None
        } else {
            self.cached_entry(key_id).await
        };
        if !force_refresh {
            if let Some(auth) = cached_entry
                .as_ref()
                .and_then(|entry| adapter.resolve_cached(transport, entry))
            {
                return Ok(Some(LocalOAuthResolution::resolved(auth, None)));
            }
            if let Some(auth) = adapter.resolve_without_refresh(transport) {
                return Ok(Some(LocalOAuthResolution::resolved(auth, None)));
            }
            if !adapter.should_refresh(transport, cached_entry.as_ref()) {
                return Ok(None);
            }
        }
        if key_id.is_empty() {
            return Ok(None);
        }

        let key_lock = self.lock_for_key(key_id).await;
        let _key_guard = key_lock.lock().await;

        let cached_entry = self.cached_entry(key_id).await;
        if !force_refresh {
            if let Some(auth) = cached_entry
                .as_ref()
                .and_then(|entry| adapter.resolve_cached(transport, entry))
            {
                return Ok(Some(LocalOAuthResolution::resolved(auth, None)));
            }
            if let Some(auth) = adapter.resolve_without_refresh(transport) {
                return Ok(Some(LocalOAuthResolution::resolved(auth, None)));
            }
            if !adapter.should_refresh(transport, cached_entry.as_ref()) {
                return Ok(None);
            }
        }

        let distributed_lease = match (distributed_lock, distributed_owner) {
            (Some(lock), Some(owner)) if !owner.trim().is_empty() => {
                match lock
                    .lock_try_acquire(
                        &format!("provider_oauth_refresh_lock:{key_id}"),
                        owner,
                        std::time::Duration::from_millis(Self::DISTRIBUTED_REFRESH_LOCK_TTL_MS),
                    )
                    .await
                {
                    Ok(Some(lease)) => Some(lease),
                    Ok(None) => return Ok(Some(LocalOAuthResolution::refresh_in_flight())),
                    Err(err) => {
                        tracing::warn!(
                            key_id = %key_id,
                            provider_type = adapter.provider_type(),
                            error = ?err,
                            "gateway local oauth refresh distributed lock unavailable"
                        );
                        None
                    }
                }
            }
            _ => None,
        };

        // Forced refresh still needs the latest rotated refresh_token as input.
        // Otherwise a second overlapping refresh can acquire the lock after the
        // first one completes, then immediately retry with the stale token that
        // came from the original transport snapshot.
        let refresh_entry = cached_entry.as_ref();
        let refresh_result = adapter.refresh(executor, transport, refresh_entry).await;
        if let (Some(lock), Some(lease)) = (distributed_lock, distributed_lease.as_ref()) {
            if let Err(err) = lock.lock_release(lease).await {
                tracing::warn!(
                    key_id = %key_id,
                    provider_type = adapter.provider_type(),
                    error = ?err,
                    "gateway local oauth refresh distributed lock release failed"
                );
            }
        }
        let Some(refreshed_entry) = refresh_result? else {
            return Ok(None);
        };
        Ok(adapter
            .resolve_cached(transport, &refreshed_entry)
            .map(|auth| LocalOAuthResolution::resolved(auth, Some(refreshed_entry))))
    }

    pub fn with_adapters_for_tests(adapters: Vec<Arc<dyn LocalOAuthRefreshAdapter>>) -> Self {
        Self {
            adapters,
            cache: Mutex::new(BTreeMap::new()),
            key_locks: Mutex::new(BTreeMap::new()),
        }
    }
}

impl LocalOAuthResolution {
    fn resolved(
        auth: LocalResolvedOAuthRequestAuth,
        refreshed_entry: Option<CachedOAuthEntry>,
    ) -> Self {
        Self {
            auth: Some(auth),
            refreshed_entry,
            refresh_in_flight: false,
        }
    }

    fn refresh_in_flight() -> Self {
        Self {
            auth: None,
            refreshed_entry: None,
            refresh_in_flight: true,
        }
    }
}

pub fn supports_local_oauth_request_auth_resolution(
    transport: &GatewayProviderTransportSnapshot,
) -> bool {
    supports_local_kiro_request_auth_resolution(transport)
        || supports_local_vertex_service_account_auth_resolution(transport)
        || supports_local_generic_oauth_request_auth_resolution(transport)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::snapshot::{
        GatewayProviderTransportEndpoint, GatewayProviderTransportKey,
        GatewayProviderTransportProvider, GatewayProviderTransportSnapshot,
    };
    use super::{
        CachedOAuthEntry, LocalOAuthHttpExecutor, LocalOAuthRefreshAdapter,
        LocalOAuthRefreshCoordinator, LocalOAuthRefreshError, LocalOAuthResolution,
        LocalResolvedOAuthRequestAuth, ReqwestLocalOAuthHttpExecutor,
    };
    use async_trait::async_trait;
    use std::sync::Arc;

    #[derive(Debug)]
    struct TestAdapter {
        refresh_hits: Arc<AtomicUsize>,
        refresh_with_entry_hits: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LocalOAuthRefreshAdapter for TestAdapter {
        fn provider_type(&self) -> &'static str {
            "test-oauth"
        }

        fn resolve_cached(
            &self,
            _transport: &GatewayProviderTransportSnapshot,
            entry: &CachedOAuthEntry,
        ) -> Option<LocalResolvedOAuthRequestAuth> {
            (entry.provider_type == "test-oauth").then(|| LocalResolvedOAuthRequestAuth::Header {
                name: entry.auth_header_name.clone(),
                value: entry.auth_header_value.clone(),
            })
        }

        fn resolve_without_refresh(
            &self,
            transport: &GatewayProviderTransportSnapshot,
        ) -> Option<LocalResolvedOAuthRequestAuth> {
            let secret = transport.key.decrypted_api_key.trim();
            (!secret.is_empty() && secret != "__placeholder__").then(|| {
                LocalResolvedOAuthRequestAuth::Header {
                    name: "authorization".to_string(),
                    value: format!("Bearer {secret}"),
                }
            })
        }

        fn should_refresh(
            &self,
            transport: &GatewayProviderTransportSnapshot,
            entry: Option<&CachedOAuthEntry>,
        ) -> bool {
            entry.is_none() && transport.key.decrypted_api_key.trim() == "__placeholder__"
        }

        async fn refresh(
            &self,
            _executor: &dyn LocalOAuthHttpExecutor,
            _transport: &GatewayProviderTransportSnapshot,
            entry: Option<&CachedOAuthEntry>,
        ) -> Result<Option<CachedOAuthEntry>, LocalOAuthRefreshError> {
            self.refresh_hits.fetch_add(1, Ordering::SeqCst);
            if entry.is_some() {
                self.refresh_with_entry_hits.fetch_add(1, Ordering::SeqCst);
            }
            Ok(Some(CachedOAuthEntry {
                provider_type: "test-oauth".to_string(),
                auth_header_name: "authorization".to_string(),
                auth_header_value: "Bearer refreshed-token".to_string(),
                expires_at_unix_secs: Some(4_102_444_800),
                metadata: None,
            }))
        }
    }

    fn sample_transport() -> GatewayProviderTransportSnapshot {
        GatewayProviderTransportSnapshot {
            provider: GatewayProviderTransportProvider {
                id: "provider-1".to_string(),
                name: "test".to_string(),
                provider_type: "test-oauth".to_string(),
                website: None,
                is_active: true,
                keep_priority_on_conversion: false,
                enable_format_conversion: false,
                concurrent_limit: None,
                max_retries: None,
                proxy: None,
                request_timeout_secs: None,
                stream_first_byte_timeout_secs: None,
                stream_idle_timeout_secs: None,
                config: None,
            },
            endpoint: GatewayProviderTransportEndpoint {
                id: "endpoint-1".to_string(),
                provider_id: "provider-1".to_string(),
                api_format: "claude:messages".to_string(),
                api_family: Some("claude".to_string()),
                endpoint_kind: Some("cli".to_string()),
                is_active: true,
                base_url: "https://example.test".to_string(),
                header_rules: None,
                body_rules: None,
                max_retries: None,
                custom_path: None,
                config: None,
                format_acceptance_config: None,
                proxy: None,
            },
            key: GatewayProviderTransportKey {
                id: "key-1".to_string(),
                provider_id: "provider-1".to_string(),
                name: "key".to_string(),
                auth_type: "bearer".to_string(),
                is_active: true,
                api_formats: None,
                auth_type_by_format: None,
                allow_auth_channel_mismatch_formats: None,

                allowed_models: None,
                capabilities: None,
                rate_multipliers: None,
                global_priority_by_format: None,
                expires_at_unix_secs: None,
                proxy: None,
                fingerprint: None,
                upstream_metadata: None,
                decrypted_api_key: "__placeholder__".to_string(),
                decrypted_auth_config: Some("{\"refresh_token\":\"rt-1\"}".to_string()),
            },
        }
    }

    #[tokio::test]
    async fn coordinator_reuses_runtime_cached_refresh_result() {
        let refresh_hits = Arc::new(AtomicUsize::new(0));
        let refresh_with_entry_hits = Arc::new(AtomicUsize::new(0));
        let coordinator =
            LocalOAuthRefreshCoordinator::with_adapters_for_tests(vec![Arc::new(TestAdapter {
                refresh_hits: Arc::clone(&refresh_hits),
                refresh_with_entry_hits: Arc::clone(&refresh_with_entry_hits),
            })]);
        let transport = sample_transport();
        let executor = ReqwestLocalOAuthHttpExecutor::new(reqwest::Client::new());

        let first = coordinator
            .resolve_with_result(&executor, &transport, None, None)
            .await
            .expect("first resolve should succeed");
        coordinator
            .insert_cached_entry(
                transport.key.id.as_str(),
                first
                    .as_ref()
                    .and_then(|result| result.refreshed_entry.clone())
                    .expect("first resolve should provide cached entry"),
            )
            .await;
        let second = coordinator
            .resolve_with_result(&executor, &transport, None, None)
            .await
            .expect("second resolve should succeed");

        assert_eq!(refresh_hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            first,
            Some(LocalOAuthResolution {
                auth: Some(LocalResolvedOAuthRequestAuth::Header {
                    name: "authorization".to_string(),
                    value: "Bearer refreshed-token".to_string(),
                }),
                refreshed_entry: Some(CachedOAuthEntry {
                    provider_type: "test-oauth".to_string(),
                    auth_header_name: "authorization".to_string(),
                    auth_header_value: "Bearer refreshed-token".to_string(),
                    expires_at_unix_secs: Some(4_102_444_800),
                    metadata: None,
                }),
                refresh_in_flight: false,
            })
        );
        assert_eq!(
            second,
            Some(LocalOAuthResolution {
                auth: Some(LocalResolvedOAuthRequestAuth::Header {
                    name: "authorization".to_string(),
                    value: "Bearer refreshed-token".to_string(),
                }),
                refreshed_entry: None,
                refresh_in_flight: false,
            })
        );
    }

    #[tokio::test]
    async fn coordinator_force_refresh_bypasses_runtime_cache() {
        let refresh_hits = Arc::new(AtomicUsize::new(0));
        let refresh_with_entry_hits = Arc::new(AtomicUsize::new(0));
        let coordinator =
            LocalOAuthRefreshCoordinator::with_adapters_for_tests(vec![Arc::new(TestAdapter {
                refresh_hits: Arc::clone(&refresh_hits),
                refresh_with_entry_hits: Arc::clone(&refresh_with_entry_hits),
            })]);
        let transport = sample_transport();
        let executor = ReqwestLocalOAuthHttpExecutor::new(reqwest::Client::new());

        let first = coordinator
            .resolve_with_result(&executor, &transport, None, None)
            .await
            .expect("initial resolve should succeed");
        coordinator
            .insert_cached_entry(
                transport.key.id.as_str(),
                first
                    .as_ref()
                    .and_then(|result| result.refreshed_entry.clone())
                    .expect("first resolve should provide cached entry"),
            )
            .await;
        let forced = coordinator
            .force_refresh_with_result(&executor, &transport, None, None)
            .await
            .expect("forced refresh should succeed");

        assert!(first.and_then(|result| result.refreshed_entry).is_some());
        assert!(forced.and_then(|result| result.refreshed_entry).is_some());
        assert_eq!(refresh_hits.load(Ordering::SeqCst), 2);
        assert_eq!(refresh_with_entry_hits.load(Ordering::SeqCst), 1);
    }

    fn codex_transport(
        auth_config: Option<&str>,
        pool_advanced: serde_json::Value,
    ) -> GatewayProviderTransportSnapshot {
        let mut transport = sample_transport();
        transport.provider.provider_type = "codex".to_string();
        transport.key.decrypted_auth_config = auth_config.map(ToOwned::to_owned);
        transport.provider.config = Some(serde_json::json!({
            "pool_advanced": pool_advanced,
        }));
        transport
    }

    #[test]
    fn codex_oauth_user_agent_uses_account_profile() {
        let pool_advanced = serde_json::json!({
            "codex_client_headers": {
                "enabled": true,
                "profiles": [
                    { "user_agent": "codex-tui/0.153.3 (Debian 13.0.0; x86_64)", "originator": "codex-tui" },
                    { "user_agent": "Codex Desktop/0.153.1 (Windows 10.0.26100; x86_64)", "originator": "Codex Desktop" },
                ],
            },
        });
        let transport = codex_transport(Some(r#"{"account_id":"acc-1"}"#), pool_advanced);
        let ua = super::codex_pool_account_client_profile(&transport)
            .map(|p| p.user_agent)
            .expect("profile UA should resolve");
        assert!(
            ua == "codex-tui/0.153.3 (Debian 13.0.0; x86_64)"
                || ua == "Codex Desktop/0.153.1 (Windows 10.0.26100; x86_64)",
            "user agent should be one of the configured profiles, got {ua}"
        );
    }

    #[test]
    fn codex_oauth_user_agent_uses_chatgpt_account_id_alias() {
        let pool_advanced = serde_json::json!({
            "codex_client_headers": { "enabled": true, "profiles": [] },
        });
        let transport = codex_transport(Some(r#"{"chatgptAccountId":"acc-2"}"#), pool_advanced);
        // Empty profiles → falls back to built-in defaults; must still resolve.
        let ua = super::codex_pool_account_client_profile(&transport)
            .map(|p| p.user_agent)
            .expect("default profile UA should resolve");
        assert!(
            ua.contains("codex-tui/") || ua.contains("Codex Desktop/"),
            "default UA {ua}"
        );
    }

    #[test]
    fn codex_oauth_user_agent_disabled_profiles_returns_none() {
        let pool_advanced = serde_json::json!({
            "codex_client_headers": { "enabled": false, "profiles": [] },
        });
        let transport = codex_transport(Some(r#"{"account_id":"acc-3"}"#), pool_advanced);
        assert!(super::codex_pool_account_client_profile(&transport)
            .map(|p| p.user_agent)
            .is_none());
    }

    #[test]
    fn codex_oauth_user_agent_prefers_matching_fingerprint_profile() {
        // account_id "acc-4" → selection identity ("auth_account_id", sha256:...).
        let pool_advanced = serde_json::json!({
            "codex_client_headers": {
                "enabled": true,
                "profiles": [
                    { "user_agent": "codex-tui/0.153.3", "originator": "codex-tui" },
                ],
            },
        });
        let mut transport = codex_transport(Some(r#"{"account_id":"acc-4"}"#), pool_advanced);
        // The fingerprint stores a persisted UA for the same account.
        let account_hash = super::codex_profile_selection_identity(&transport).1;
        transport.key.fingerprint = Some(serde_json::json!({
            "codex_client_profile": {
                "selection_key_kind": "auth_account_id",
                "selection_key_hash": account_hash,
                "client_headers": {
                    "user_agent": "codex-tui/0.153.4 (Windows 10.0.26200; x86_64)",
                    "originator": "codex-tui",
                },
            },
        }));
        let ua = super::codex_pool_account_client_profile(&transport)
            .map(|p| p.user_agent)
            .expect("profile UA should resolve");
        assert_eq!(
            ua, "codex-tui/0.153.4 (Windows 10.0.26200; x86_64)",
            "matching fingerprint profile should win over header-profile UA"
        );
    }

    #[test]
    fn codex_oauth_client_profile_carries_originator_from_fingerprint() {
        let pool_advanced = serde_json::json!({
            "codex_client_headers": {
                "enabled": true,
                "profiles": [
                    { "user_agent": "codex-tui/0.153.3", "originator": "codex-tui" },
                ],
            },
        });
        let mut transport = codex_transport(Some(r#"{"account_id":"acc-6"}"#), pool_advanced);
        let account_hash = super::codex_profile_selection_identity(&transport).1;
        transport.key.fingerprint = Some(serde_json::json!({
            "codex_client_profile": {
                "selection_key_kind": "auth_account_id",
                "selection_key_hash": account_hash,
                "client_headers": {
                    "user_agent": "Codex Desktop/0.153.1 (Windows 10.0.26100; x86_64)",
                    "originator": "Codex Desktop",
                },
            },
        }));
        let profile = super::resolve_oauth_maintenance_client_profile(&transport)
            .expect("profile should resolve");
        assert_eq!(
            profile,
            super::CodexOAuthClientProfile {
                user_agent: "Codex Desktop/0.153.1 (Windows 10.0.26100; x86_64)".to_string(),
                originator: "Codex Desktop".to_string(),
            }
        );
        let ctx = super::provider_oauth_transport_context_from_snapshot(&transport);
        assert_eq!(
            ctx.user_agent.as_deref(),
            Some("Codex Desktop/0.153.1 (Windows 10.0.26100; x86_64)")
        );
        assert_eq!(ctx.originator.as_deref(), Some("Codex Desktop"));
    }

    #[test]
    fn codex_oauth_client_profile_originator_matches_selected_user_agent() {
        let pool_advanced = serde_json::json!({
            "codex_client_headers": {
                "enabled": true,
                "profiles": [
                    { "user_agent": "codex-tui/0.153.3 (Debian 13.0.0; x86_64)", "originator": "codex-tui" },
                    { "user_agent": "Codex Desktop/0.153.1 (Windows 10.0.26100; x86_64)", "originator": "Codex Desktop" },
                ],
            },
        });
        let transport = codex_transport(Some(r#"{"account_id":"acc-7"}"#), pool_advanced);
        let profile = super::resolve_oauth_maintenance_client_profile(&transport)
            .expect("profile should resolve");
        let expected_originator = if profile.user_agent.starts_with("codex-tui/") {
            "codex-tui"
        } else {
            "Codex Desktop"
        };
        assert_eq!(profile.originator, expected_originator);
    }

    #[test]
    fn codex_oauth_user_agent_ignores_mismatched_fingerprint_profile() {
        let pool_advanced = serde_json::json!({
            "codex_client_headers": {
                "enabled": true,
                "profiles": [
                    { "user_agent": "codex-tui/0.153.3 (Debian 13.0.0; x86_64)", "originator": "codex-tui" },
                ],
            },
        });
        let mut transport = codex_transport(Some(r#"{"account_id":"acc-5"}"#), pool_advanced);
        // Fingerprint was recorded for a DIFFERENT account → ignored.
        transport.key.fingerprint = Some(serde_json::json!({
            "codex_client_profile": {
                "selection_key_kind": "auth_account_id",
                "selection_key_hash": "sha256:other-account-hash",
                "client_headers": { "user_agent": "persisted-other", "originator": "codex-tui" },
            },
        }));
        let ua = super::codex_pool_account_client_profile(&transport)
            .map(|p| p.user_agent)
            .expect("profile UA should resolve");
        assert!(
            ua == "codex-tui/0.153.3 (Debian 13.0.0; x86_64)",
            "mismatched fingerprint profile must be ignored, got {ua}"
        );
    }
}
