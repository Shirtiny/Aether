//! Codex model manifest follow: which slugs the upstream flags as Responses
//! Lite, kept current at runtime.
//!
//! codex-rs (`models-manager/src/manager.rs`) fetches
//! `GET {base}/models?client_version=<major.minor.patch>` once per session
//! start, caches it for 300 s, and re-fetches when a `/responses` reply
//! carries a new `X-Models-Etag`. Every capability the client keys on lives in
//! that manifest; the one Aether needs is `use_responses_lite`
//! (`protocol/src/openai_models.rs`): it decides both the
//! `x-openai-internal-codex-responses-lite` header and the lite request-body
//! contract. The backend checks header and body against the model it serves,
//! so a pool that remaps a lite alias to a plain target must know which of
//! the two is lite-capable (`codex_routing_hint`).
//!
//! The server gates the manifest by `client_version`: a slug whose
//! `minimal_client_version` is above the asked version is simply absent. So
//! the fetch asks with the newest stable version real inbound clients are
//! running (the client-release registry of `codex_client_release`), which is
//! the superset view a current client would see.
//!
//! Three sources, in order, for one lookup:
//!
//! 1. the live snapshot (fetched by the worker in this module, persisted to
//!    Redis so a restart does not start blind);
//! 2. the bundled slim manifest (`codex_model_catalog/bundled.json`, the
//!    capability fields of codex-rs `models-manager/models.json`, no prompt
//!    text) when the live snapshot does not list the slug (version gate,
//!    custom name) or the follow is switched off;
//! 3. `false` when neither lists the slug.
//!
//! Kill switch: `AETHER_CODEX_MODEL_CATALOG=off` stops the worker and makes
//! every lookup use the bundled manifest. See
//! `docs/operations/codex-runtime-identity-runbook.md` §3.13.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aether_contracts::{ExecutionPlan, ExecutionResult};
use aether_data_contracts::repository::provider_catalog::{
    StoredProviderCatalogEndpoint, StoredProviderCatalogKey, StoredProviderCatalogProvider,
};
use aether_model_fetch::ModelFetchTransportRuntime;
use aether_runtime_state::RuntimeState;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::codex_client_release::{
    compose_codex_client_user_agent, unix_now_secs, ClientReleaseStore, ClientVersion,
};
use crate::codex_environment_context::rewrite_switch_enabled;
use crate::provider_transport::GatewayProviderTransportSnapshot;
use crate::{AppState, GatewayError};

/// Redis key prefix; one JSON document per followed client version, the slug
/// map inside (the runtime state has no hash API and the document is ~3 KB).
const CATALOG_KEY_DOMAIN: &str = "aether:codex:model_catalog:v1";
/// Refresh lock so a multi-instance gateway fetches once per interval.
const CATALOG_LOCK_KEY: &str = "aether:codex:model_catalog:v1:refresh_lock";
const CATALOG_LOCK_TTL: Duration = Duration::from_secs(60);
/// A persisted snapshot outlives a long outage; the worker overwrites it
/// every interval while the upstream answers.
pub(crate) const CATALOG_REDIS_TTL: Duration = Duration::from_secs(7 * 86_400);
/// Mirrors codex-rs `DEFAULT_MODEL_CACHE_TTL`.
const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 300;
const MIN_REFRESH_INTERVAL_SECS: u64 = 60;
const STARTUP_DELAY: Duration = Duration::from_secs(15);
/// Accounts tried per refresh before giving up until the next tick.
const MAX_KEYS_PER_REFRESH: usize = 3;
/// A snapshot older than this is still used (it is newer than the bundled
/// manifest) but warned about once an hour.
const STALE_AFTER_SECS: u64 = 24 * 3_600;
const STALE_WARN_EVERY_SECS: u64 = 3_600;
/// Asked when the client-release registry has no record at all: the bundled
/// manifest's own client build. Keep in step with `bundled.json`.
pub(crate) const CODEX_MANIFEST_FALLBACK_CLIENT_VERSION: &str = "0.154.0";
/// `client_version` tag of the bundled snapshot; names the codex-rs commit the
/// slim copy was generated from.
pub(crate) const BUNDLED_CLIENT_VERSION: &str = "bundled@07f18d5f";

const BUNDLED_MANIFEST: &str = include_str!("codex_model_catalog/bundled.json");

// ---------------------------------------------------------------------------
// Manifest types
// ---------------------------------------------------------------------------

/// Capability fields kept per model. Everything else in the manifest entry
/// (`base_instructions` 14-21 KB each, tool lists, plan availability) is
/// dropped at parse time: it is neither needed nor something to persist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CodexModelCapability {
    pub(crate) slug: String,
    #[serde(default)]
    pub(crate) use_responses_lite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prefer_websockets: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) supported_in_api: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) visibility: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) minimal_client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) priority: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CodexModelCatalogSnapshot {
    /// The `client_version` the manifest was asked with (`0.154.0`), or
    /// [`BUNDLED_CLIENT_VERSION`].
    pub(crate) client_version: String,
    /// Response `ETag`, verbatim. Recorded for observability only: the
    /// official client does not send it back as a conditional request either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) etag: Option<String>,
    pub(crate) fetched_at_unix_secs: u64,
    /// Keyed by the normalised slug ([`normalize_slug`]).
    pub(crate) models: BTreeMap<String, CodexModelCapability>,
}

pub(crate) fn normalize_slug(slug: &str) -> String {
    slug.trim().to_ascii_lowercase()
}

/// Reads `body.models[]`. Entries without a usable `slug` are skipped (and
/// counted in the debug log); a body without a `models` array is an error so
/// an HTML error page or a changed shape never installs an empty snapshot.
pub(crate) fn parse_codex_models_manifest(
    body: &Value,
) -> Result<Vec<CodexModelCapability>, String> {
    let models = body
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "codex models manifest has no `models` array".to_string())?;
    let mut parsed = Vec::with_capacity(models.len());
    let mut skipped = 0usize;
    for entry in models {
        match serde_json::from_value::<CodexModelCapability>(entry.clone()) {
            Ok(capability) if !capability.slug.trim().is_empty() => parsed.push(capability),
            _ => skipped += 1,
        }
    }
    if skipped > 0 {
        debug!(
            event = "codex_model_catalog_entries_skipped",
            skipped, "codex models manifest entries without a slug were skipped"
        );
    }
    if parsed.is_empty() {
        return Err("codex models manifest lists no model with a slug".to_string());
    }
    Ok(parsed)
}

impl CodexModelCatalogSnapshot {
    pub(crate) fn from_models(
        client_version: impl Into<String>,
        etag: Option<String>,
        fetched_at_unix_secs: u64,
        models: Vec<CodexModelCapability>,
    ) -> Self {
        Self {
            client_version: client_version.into(),
            etag,
            fetched_at_unix_secs,
            models: models
                .into_iter()
                .map(|capability| (normalize_slug(&capability.slug), capability))
                .collect(),
        }
    }

    /// `None` when the snapshot does not list the slug at all.
    pub(crate) fn uses_responses_lite(&self, model: &str) -> Option<bool> {
        let slug = normalize_slug(model);
        if slug.is_empty() {
            return None;
        }
        self.models
            .get(&slug)
            .map(|capability| capability.use_responses_lite)
    }

    pub(crate) fn lite_slugs(&self) -> BTreeSet<String> {
        self.models
            .iter()
            .filter(|(_, capability)| capability.use_responses_lite)
            .map(|(slug, _)| slug.clone())
            .collect()
    }

    fn age_secs(&self, now: u64) -> u64 {
        now.saturating_sub(self.fetched_at_unix_secs)
    }
}

// ---------------------------------------------------------------------------
// Bundled fallback and in-process snapshot
// ---------------------------------------------------------------------------

pub(crate) fn bundled_snapshot() -> &'static CodexModelCatalogSnapshot {
    static BUNDLED: OnceLock<CodexModelCatalogSnapshot> = OnceLock::new();
    BUNDLED.get_or_init(|| {
        let body: Value =
            serde_json::from_str(BUNDLED_MANIFEST).expect("bundled codex manifest is valid JSON");
        let models = parse_codex_models_manifest(&body)
            .expect("bundled codex manifest lists models with slugs");
        CodexModelCatalogSnapshot::from_models(BUNDLED_CLIENT_VERSION, None, 0, models)
    })
}

static SNAPSHOT: parking_lot::RwLock<Option<Arc<CodexModelCatalogSnapshot>>> =
    parking_lot::RwLock::new(None);

pub(crate) fn current_snapshot() -> Option<Arc<CodexModelCatalogSnapshot>> {
    if !catalog_enabled() {
        return None;
    }
    let snapshot = SNAPSHOT.read().clone();
    if let Some(snapshot) = snapshot.as_deref() {
        warn_if_stale(snapshot);
    }
    snapshot
}

pub(crate) fn install_snapshot(snapshot: Arc<CodexModelCatalogSnapshot>) {
    *SNAPSHOT.write() = Some(snapshot);
}

#[cfg(test)]
pub(crate) fn clear_snapshot() {
    *SNAPSHOT.write() = None;
}

/// Live-snapshot answer for `model`: `Some(flag)` when the follow is on and
/// the snapshot lists the slug, `None` otherwise (caller falls back to the
/// bundled manifest).
pub(crate) fn uses_responses_lite(model: &str) -> Option<bool> {
    current_snapshot()
        .as_deref()
        .and_then(|snapshot| snapshot.uses_responses_lite(model))
}

/// `AETHER_CODEX_MODEL_CATALOG=off|0|false|disabled|no` turns the runtime
/// follow off (worker not started, lookups use the bundled manifest).
pub(crate) fn catalog_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        rewrite_switch_enabled(std::env::var("AETHER_CODEX_MODEL_CATALOG").ok().as_deref())
    })
}

pub(crate) fn refresh_interval() -> Duration {
    let secs = std::env::var("AETHER_CODEX_MODEL_CATALOG_INTERVAL_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_REFRESH_INTERVAL_SECS)
        .max(MIN_REFRESH_INTERVAL_SECS);
    Duration::from_secs(secs)
}

fn warn_if_stale(snapshot: &CodexModelCatalogSnapshot) {
    static LAST_WARNED: Mutex<u64> = Mutex::new(0);
    let now = unix_now_secs();
    let age = snapshot.age_secs(now);
    if age <= STALE_AFTER_SECS {
        return;
    }
    let mut last = LAST_WARNED
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if now.saturating_sub(*last) < STALE_WARN_EVERY_SECS {
        return;
    }
    *last = now;
    warn!(
        event = "codex_model_catalog_stale",
        age_secs = age,
        client_version = %snapshot.client_version,
        "codex model catalog snapshot is stale; still serving it over the bundled manifest"
    );
}

// ---------------------------------------------------------------------------
// Drift against the bundled manifest (layer 2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LiteDrift {
    /// Lite in the live manifest, not lite (or unknown) in the bundled copy.
    pub(crate) added: Vec<String>,
    /// Lite in the bundled copy, listed by the live manifest as not lite.
    pub(crate) removed: Vec<String>,
    /// Lite in the bundled copy, not listed by the live manifest at all
    /// (usually the version gate; not a contradiction).
    pub(crate) hidden: Vec<String>,
}

impl LiteDrift {
    pub(crate) fn compute(
        live: &CodexModelCatalogSnapshot,
        bundled: &CodexModelCatalogSnapshot,
    ) -> Self {
        let live_lite = live.lite_slugs();
        let bundled_lite = bundled.lite_slugs();
        let mut drift = Self::default();
        for slug in &live_lite {
            if !bundled_lite.contains(slug) {
                drift.added.push(slug.clone());
            }
        }
        for slug in &bundled_lite {
            if live_lite.contains(slug) {
                continue;
            }
            if live.models.contains_key(slug) {
                drift.removed.push(slug.clone());
            } else {
                drift.hidden.push(slug.clone());
            }
        }
        drift
    }

    pub(crate) fn contradicts(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty()
    }

    fn signature(&self) -> String {
        format!(
            "+{} -{} ?{}",
            self.added.join(","),
            self.removed.join(","),
            self.hidden.join(",")
        )
    }
}

/// Logs the drift once per distinct signature at warn level (contradictions
/// only), debug afterwards. Returns whether this call warned.
pub(crate) fn report_lite_drift(client_version: &str, drift: &LiteDrift) -> bool {
    static LAST_SIGNATURE: Mutex<Option<String>> = Mutex::new(None);
    let signature = drift.signature();
    let mut last = LAST_SIGNATURE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let repeated = last.as_deref() == Some(signature.as_str());
    *last = Some(signature);
    if drift.contradicts() && !repeated {
        warn!(
            event = "codex_model_catalog_lite_drift",
            client_version = %client_version,
            added = ?drift.added,
            removed = ?drift.removed,
            hidden = ?drift.hidden,
            "live codex manifest disagrees with the bundled Responses Lite set; regenerate bundled.json"
        );
        return true;
    }
    debug!(
        event = "codex_model_catalog_lite_drift",
        client_version = %client_version,
        added = ?drift.added,
        removed = ?drift.removed,
        hidden = ?drift.hidden,
        repeated,
        "codex manifest drift check"
    );
    false
}

// ---------------------------------------------------------------------------
// Redis persistence
// ---------------------------------------------------------------------------

pub(crate) fn catalog_key(client_version: &str) -> String {
    format!("{CATALOG_KEY_DOMAIN}:{client_version}")
}

pub(crate) async fn store_snapshot(
    runtime: &RuntimeState,
    snapshot: &CodexModelCatalogSnapshot,
) -> Result<(), String> {
    let serialized = serde_json::to_string(snapshot).map_err(|error| error.to_string())?;
    runtime
        .kv_set(
            &catalog_key(&snapshot.client_version),
            serialized,
            Some(CATALOG_REDIS_TTL),
        )
        .await
        .map_err(|error| error.to_string())
}

pub(crate) async fn load_snapshot(
    runtime: &RuntimeState,
    client_version: &str,
) -> Result<Option<CodexModelCatalogSnapshot>, String> {
    let Some(raw) = runtime
        .kv_get(&catalog_key(client_version))
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| format!("codex model catalog document is unreadable: {error}"))
}

/// The persisted snapshot with the newest parsable client version, for a
/// start-up whose followed version has no document yet.
pub(crate) async fn load_newest_snapshot(
    runtime: &RuntimeState,
) -> Result<Option<CodexModelCatalogSnapshot>, String> {
    let keys = runtime
        .scan_keys(&format!("{CATALOG_KEY_DOMAIN}:*"), 100)
        .await
        .map_err(|error| error.to_string())?;
    let prefix = format!("{CATALOG_KEY_DOMAIN}:");
    let mut versions = keys
        .iter()
        .filter_map(|key| {
            let suffix = key.rsplit(&prefix).next()?;
            ClientVersion::parse(suffix)
        })
        .collect::<Vec<_>>();
    versions.sort();
    let Some(newest) = versions.pop() else {
        return Ok(None);
    };
    load_snapshot(runtime, &newest.to_string()).await
}

// ---------------------------------------------------------------------------
// Followed client version
// ---------------------------------------------------------------------------

/// Newest stable version any inbound Codex client has been seen running,
/// across the given providers and every originator family. No adoption lag:
/// the manifest wants the superset a current client sees. Falls back to
/// [`CODEX_MANIFEST_FALLBACK_CLIENT_VERSION`] when nothing is recorded.
pub(crate) async fn resolve_followed_codex_client_version(
    runtime: &RuntimeState,
    codex_provider_ids: &[String],
) -> ClientVersion {
    let fallback = ClientVersion::parse(CODEX_MANIFEST_FALLBACK_CLIENT_VERSION)
        .expect("fallback client version is a stable version");
    let mut newest: Option<ClientVersion> = None;
    for provider_id in codex_provider_ids {
        let store = ClientReleaseStore::new(runtime, provider_id);
        let families = match store.families().await {
            Ok(families) => families,
            Err(error) => {
                debug!(
                    event = "codex_model_catalog_registry_unavailable",
                    error = %error,
                    "codex client release registry could not be scanned"
                );
                continue;
            }
        };
        for family in families {
            if let Ok(records) = store.release_records(&family).await {
                if let Some(record) = records.first() {
                    newest =
                        Some(newest.map_or(record.version, |current| current.max(record.version)));
                }
            }
        }
    }
    newest.unwrap_or(fallback)
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

/// What the refresh needs from the gateway; `AppState` implements it, tests
/// use a stub.
#[async_trait]
pub(crate) trait CodexModelCatalogRuntime: ModelFetchTransportRuntime + Sync {
    fn runtime_state(&self) -> &RuntimeState;

    async fn list_provider_catalog_providers(
        &self,
        active_only: bool,
    ) -> Result<Vec<StoredProviderCatalogProvider>, GatewayError>;

    async fn list_provider_catalog_endpoints_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<StoredProviderCatalogEndpoint>, GatewayError>;

    async fn list_provider_catalog_keys_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<StoredProviderCatalogKey>, GatewayError>;

    async fn read_provider_transport_snapshot(
        &self,
        provider_id: &str,
        endpoint_id: &str,
        key_id: &str,
    ) -> Result<Option<GatewayProviderTransportSnapshot>, GatewayError>;

    async fn execute_execution_runtime_sync_plan(
        &self,
        plan: &ExecutionPlan,
    ) -> Result<ExecutionResult, GatewayError>;
}

#[async_trait]
impl CodexModelCatalogRuntime for AppState {
    fn runtime_state(&self) -> &RuntimeState {
        &self.runtime_state
    }

    async fn list_provider_catalog_providers(
        &self,
        active_only: bool,
    ) -> Result<Vec<StoredProviderCatalogProvider>, GatewayError> {
        AppState::list_provider_catalog_providers(self, active_only).await
    }

    async fn list_provider_catalog_endpoints_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<StoredProviderCatalogEndpoint>, GatewayError> {
        AppState::list_provider_catalog_endpoints_by_provider_ids(self, provider_ids).await
    }

    async fn list_provider_catalog_keys_by_provider_ids(
        &self,
        provider_ids: &[String],
    ) -> Result<Vec<StoredProviderCatalogKey>, GatewayError> {
        AppState::list_provider_catalog_keys_by_provider_ids(self, provider_ids).await
    }

    async fn read_provider_transport_snapshot(
        &self,
        provider_id: &str,
        endpoint_id: &str,
        key_id: &str,
    ) -> Result<Option<GatewayProviderTransportSnapshot>, GatewayError> {
        AppState::read_provider_transport_snapshot(self, provider_id, endpoint_id, key_id).await
    }

    async fn execute_execution_runtime_sync_plan(
        &self,
        plan: &ExecutionPlan,
    ) -> Result<ExecutionResult, GatewayError> {
        crate::execution_runtime::execute_execution_runtime_sync_plan(self, None, plan).await
    }
}

/// One account able to fetch the manifest: an active key of an active Codex
/// provider whose `openai:responses` endpoint points at the chatgpt backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestFetchTarget {
    pub(crate) provider_id: String,
    pub(crate) endpoint_id: String,
    pub(crate) key_id: String,
}

pub(crate) async fn collect_manifest_targets<S>(
    state: &S,
) -> Result<(Vec<String>, Vec<ManifestFetchTarget>), GatewayError>
where
    S: CodexModelCatalogRuntime + ?Sized,
{
    let providers = state.list_provider_catalog_providers(true).await?;
    let provider_ids = providers
        .into_iter()
        .filter(|provider| provider.is_active)
        .filter(|provider| provider.provider_type.trim().eq_ignore_ascii_case("codex"))
        .map(|provider| provider.id)
        .collect::<Vec<_>>();
    if provider_ids.is_empty() {
        return Ok((provider_ids, Vec::new()));
    }
    let endpoints = state
        .list_provider_catalog_endpoints_by_provider_ids(&provider_ids)
        .await?
        .into_iter()
        .filter(|endpoint| endpoint.is_active)
        .filter(|endpoint| {
            crate::ai_serving::normalize_api_format_alias(&endpoint.api_format)
                == "openai:responses"
        })
        .filter(|endpoint| {
            aether_provider_transport::provider_types::is_codex_cli_backend_url(&endpoint.base_url)
        })
        .map(|endpoint| (endpoint.provider_id, endpoint.id))
        .collect::<BTreeMap<_, _>>();
    if endpoints.is_empty() {
        return Ok((provider_ids, Vec::new()));
    }
    let mut targets = state
        .list_provider_catalog_keys_by_provider_ids(&provider_ids)
        .await?
        .into_iter()
        .filter(|key| key.is_active)
        .filter_map(|key| {
            let endpoint_id = endpoints.get(&key.provider_id)?;
            Some(ManifestFetchTarget {
                provider_id: key.provider_id,
                endpoint_id: endpoint_id.clone(),
                key_id: key.id,
            })
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| left.key_id.cmp(&right.key_id));
    Ok((provider_ids, targets))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefreshOutcome {
    /// The follow is off or no account can fetch.
    Skipped(&'static str),
    /// A fresh enough Redis document was installed without a fetch.
    Installed { client_version: String },
    /// Fetched from the upstream and installed.
    Refreshed {
        client_version: String,
        model_count: usize,
        lite_count: usize,
        drift_warned: bool,
    },
    /// Every tried account failed; the previous snapshot stays.
    Failed { attempts: usize },
}

fn header_value_ci(headers: &BTreeMap<String, String>, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn result_json_body(result: &ExecutionResult) -> Option<Value> {
    let body = result.body.as_ref()?;
    if let Some(json) = body.json_body.as_ref() {
        return Some(json.clone());
    }
    let raw = body.body_bytes_b64.as_deref()?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(raw).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn result_error_text(result: &ExecutionResult) -> String {
    let text = result
        .error
        .as_ref()
        .map(|error| error.message.trim().to_string())
        .filter(|message| !message.is_empty())
        .or_else(|| {
            let body = result_json_body(result)?;
            // `{"error":{"message"}}` / `{"message"}` (OpenAI shapes) or the
            // chatgpt backend's `{"detail": "..."}`.
            aether_model_fetch::extract_error_message(&body).or_else(|| {
                body.get("detail")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|detail| !detail.is_empty())
                    .map(str::to_owned)
            })
        })
        .unwrap_or_else(|| format!("HTTP {}", result.status_code));
    text.chars().take(200).collect()
}

/// The identity a manifest request presents for one account: the account's
/// frozen user-agent with its version token moved to `client_version`, plus
/// its originator. `None` when the frozen token cannot be moved (a
/// pre-release build the release registry never carries, or a shape without
/// a version token): the request then keeps the neutral model-sync identity
/// the plan builder set, so the URL's `client_version` and the user-agent
/// never claim two different builds.
fn manifest_client_identity(
    frozen_user_agent: &str,
    originator: &str,
    client_version: ClientVersion,
) -> Option<(String, String)> {
    let user_agent = compose_codex_client_user_agent(frozen_user_agent, client_version);
    let presented = crate::codex_profile::codex_client_version_from_user_agent(&user_agent)?;
    (presented == client_version.to_string()).then(|| (user_agent, originator.to_string()))
}

/// Fetches the manifest through one account, as the client build the account
/// currently presents (its frozen user-agent moved to `client_version`).
async fn fetch_manifest_via<S>(
    state: &S,
    target: &ManifestFetchTarget,
    client_version: &ClientVersion,
) -> Result<(Vec<CodexModelCapability>, Option<String>), String>
where
    S: CodexModelCatalogRuntime + ?Sized,
{
    let version = client_version.to_string();
    let transport = state
        .read_provider_transport_snapshot(&target.provider_id, &target.endpoint_id, &target.key_id)
        .await
        .map_err(GatewayError::into_message)?
        .ok_or_else(|| "transport snapshot not found".to_string())?;
    let mut plan =
        aether_model_fetch::build_codex_models_manifest_execution_plan(state, &transport, &version)
            .await?;
    if let Some(profile) =
        crate::ai_serving::resolve_codex_pool_concrete_account_profile(&transport)
    {
        match manifest_client_identity(&profile.user_agent, &profile.originator, *client_version) {
            Some((user_agent, originator)) => {
                crate::codex_profile::apply_codex_client_identity_headers(
                    &mut plan.headers,
                    &user_agent,
                    &originator,
                );
            }
            None => debug!(
                event = "codex_model_catalog_identity_fallback",
                provider_id = %target.provider_id,
                key_id = %target.key_id,
                client_version = %version,
                "frozen user-agent cannot present the followed build; asking as the model-sync client"
            ),
        }
    }
    let result = state
        .execute_execution_runtime_sync_plan(&plan)
        .await
        .map_err(GatewayError::into_message)?;
    if !(200..300).contains(&result.status_code) {
        return Err(format!(
            "HTTP {}: {}",
            result.status_code,
            result_error_text(&result)
        ));
    }
    let body = result_json_body(&result)
        .ok_or_else(|| "manifest response has no JSON body".to_string())?;
    let models = parse_codex_models_manifest(&body)?;
    Ok((models, header_value_ci(&result.headers, "etag")))
}

/// One refresh round. `cycle` rotates the starting account so the periodic
/// `/models` GET is spread across the pool instead of hitting one account
/// every interval.
pub(crate) async fn refresh_once<S>(state: &S, cycle: u64, interval: Duration) -> RefreshOutcome
where
    S: CodexModelCatalogRuntime + ?Sized,
{
    if !catalog_enabled() {
        return RefreshOutcome::Skipped("disabled");
    }
    let runtime = state.runtime_state();
    let (provider_ids, targets) = match collect_manifest_targets(state).await {
        Ok(collected) => collected,
        Err(error) => {
            warn!(
                event = "codex_model_catalog_refresh_failed",
                error = %error.into_message(),
                "codex model catalog could not enumerate pool accounts"
            );
            return RefreshOutcome::Failed { attempts: 0 };
        }
    };
    if targets.is_empty() {
        return RefreshOutcome::Skipped("no_codex_account");
    }
    let client_version = resolve_followed_codex_client_version(runtime, &provider_ids).await;
    let version = client_version.to_string();
    let now = unix_now_secs();

    // A document another instance (or this one before a restart) fetched
    // within the interval is current: install it, do not fetch again.
    match load_snapshot(runtime, &version).await {
        Ok(Some(snapshot)) if snapshot.age_secs(now) < interval.as_secs() => {
            let current_matches = current_snapshot().is_some_and(|current| {
                current.client_version == snapshot.client_version
                    && current.fetched_at_unix_secs == snapshot.fetched_at_unix_secs
            });
            if !current_matches {
                install_snapshot(Arc::new(snapshot));
            }
            return RefreshOutcome::Installed {
                client_version: version,
            };
        }
        Ok(_) => {}
        Err(error) => debug!(
            event = "codex_model_catalog_load_failed",
            error = %error,
            "codex model catalog document could not be read before refresh"
        ),
    }

    let lease = match runtime
        .lock_try_acquire(CATALOG_LOCK_KEY, "codex_model_catalog", CATALOG_LOCK_TTL)
        .await
    {
        Ok(Some(lease)) => Some(lease),
        Ok(None) => {
            debug!(
                event = "codex_model_catalog_refresh_locked",
                "another gateway instance is refreshing the codex model catalog"
            );
            return RefreshOutcome::Skipped("locked");
        }
        Err(error) => {
            debug!(
                event = "codex_model_catalog_lock_failed",
                error = %error,
                "codex model catalog refresh lock unavailable; refreshing without it"
            );
            None
        }
    };

    let start = (cycle % targets.len() as u64) as usize;
    let mut attempts = 0usize;
    let mut outcome = None;
    for offset in 0..targets.len().min(MAX_KEYS_PER_REFRESH) {
        let target = &targets[(start + offset) % targets.len()];
        attempts += 1;
        match fetch_manifest_via(state, target, &client_version).await {
            Ok((models, etag)) => {
                let snapshot = CodexModelCatalogSnapshot::from_models(
                    version.clone(),
                    etag.clone(),
                    unix_now_secs(),
                    models,
                );
                let etag_changed = current_snapshot()
                    .map(|current| current.etag != snapshot.etag)
                    .unwrap_or(true);
                if let Err(error) = store_snapshot(runtime, &snapshot).await {
                    debug!(
                        event = "codex_model_catalog_store_failed",
                        error = %error,
                        "codex model catalog document could not be persisted"
                    );
                }
                let drift = LiteDrift::compute(&snapshot, bundled_snapshot());
                let drift_warned = report_lite_drift(&version, &drift);
                let model_count = snapshot.models.len();
                let lite_count = snapshot.lite_slugs().len();
                install_snapshot(Arc::new(snapshot));
                info!(
                    event = "codex_model_catalog_refreshed",
                    client_version = %version,
                    model_count,
                    lite_count,
                    etag_changed,
                    provider_id = %target.provider_id,
                    key_id = %target.key_id,
                    "codex model catalog refreshed from the upstream manifest"
                );
                outcome = Some(RefreshOutcome::Refreshed {
                    client_version: version.clone(),
                    model_count,
                    lite_count,
                    drift_warned,
                });
                break;
            }
            Err(error) => {
                warn!(
                    event = "codex_model_catalog_refresh_failed",
                    client_version = %version,
                    provider_id = %target.provider_id,
                    key_id = %target.key_id,
                    error = %error,
                    "codex model catalog fetch failed through this account"
                );
            }
        }
    }
    if let Some(lease) = lease {
        let _ = runtime.lock_release(&lease).await;
    }
    outcome.unwrap_or(RefreshOutcome::Failed { attempts })
}

/// Start-up: install whatever Redis still holds so the first requests after
/// a restart do not wait for the first fetch.
pub(crate) async fn load_persisted_snapshot<S>(state: &S)
where
    S: CodexModelCatalogRuntime + ?Sized,
{
    let runtime = state.runtime_state();
    let provider_ids = match collect_manifest_targets(state).await {
        Ok((provider_ids, _)) => provider_ids,
        Err(_) => Vec::new(),
    };
    let version = resolve_followed_codex_client_version(runtime, &provider_ids)
        .await
        .to_string();
    let loaded = match load_snapshot(runtime, &version).await {
        Ok(Some(snapshot)) => Some(snapshot),
        Ok(None) => load_newest_snapshot(runtime).await.ok().flatten(),
        Err(error) => {
            debug!(
                event = "codex_model_catalog_load_failed",
                error = %error,
                "codex model catalog document could not be read at start-up"
            );
            load_newest_snapshot(runtime).await.ok().flatten()
        }
    };
    if let Some(snapshot) = loaded {
        info!(
            event = "codex_model_catalog_loaded",
            client_version = %snapshot.client_version,
            model_count = snapshot.models.len(),
            lite_count = snapshot.lite_slugs().len(),
            age_secs = snapshot.age_secs(unix_now_secs()),
            "codex model catalog restored from redis"
        );
        install_snapshot(Arc::new(snapshot));
    }
}

pub(crate) fn spawn_codex_model_catalog_worker(
    state: AppState,
) -> Option<tokio::task::JoinHandle<()>> {
    if !catalog_enabled() {
        info!(
            event = "codex_model_catalog_disabled",
            "codex model catalog follow is off; Responses Lite uses the bundled manifest"
        );
        return None;
    }
    if !state.has_provider_catalog_data_reader() {
        return None;
    }
    Some(tokio::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        load_persisted_snapshot(&state).await;
        let interval_duration = refresh_interval();
        let mut cycle: u64 = 0;
        let outcome = refresh_once(&state, cycle, interval_duration).await;
        debug!(event = "codex_model_catalog_cycle", ?outcome, cycle);
        let mut interval = tokio::time::interval(interval_duration);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            cycle = cycle.wrapping_add(1);
            let outcome = refresh_once(&state, cycle, interval_duration).await;
            debug!(event = "codex_model_catalog_cycle", ?outcome, cycle);
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use aether_contracts::{ExecutionPlan, ExecutionResult, ProxySnapshot, ResponseBody};
    use aether_provider_transport::LocalResolvedOAuthRequestAuth;
    use serde_json::json;

    use super::*;
    use crate::provider_transport::{
        GatewayProviderTransportEndpoint, GatewayProviderTransportKey,
        GatewayProviderTransportProvider,
    };

    /// Tests that install the process-wide snapshot run one at a time.
    static GLOBAL_SNAPSHOT_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn test_runtime() -> RuntimeState {
        RuntimeState::memory(aether_runtime_state::MemoryRuntimeStateConfig::default())
    }

    const BUNDLED_LITE: [&str; 8] = [
        "codex-auto-review",
        "gpt-5.6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-6-astra",
        "gpt-daybreak-blue-latest",
        "gpt-daybreak-red-latest",
        "gpt-reserve",
    ];

    #[test]
    fn bundled_manifest_carries_the_capability_fields_only() {
        let bundled = bundled_snapshot();
        assert_eq!(bundled.client_version, BUNDLED_CLIENT_VERSION);
        assert_eq!(bundled.models.len(), 12);
        assert_eq!(
            bundled.lite_slugs().into_iter().collect::<Vec<_>>(),
            BUNDLED_LITE
                .iter()
                .map(|slug| slug.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(bundled.uses_responses_lite("gpt-5.5"), Some(false));
        assert_eq!(bundled.uses_responses_lite("gpt-5.4-mini"), Some(false));
        assert_eq!(bundled.uses_responses_lite("gpt-5.7-nova"), None);
        // No prompt text may ship in the slim copy.
        let raw = BUNDLED_MANIFEST.to_ascii_lowercase();
        assert!(!raw.contains("instructions"));
        assert!(!raw.contains("model_messages"));
        let serialized = serde_json::to_string(bundled).expect("serializes");
        assert!(!serialized.contains("instructions"));
        assert_eq!(
            bundled.models["gpt-6-astra"],
            CodexModelCapability {
                slug: "gpt-6-astra".to_string(),
                use_responses_lite: true,
                prefer_websockets: Some(true),
                supported_in_api: Some(true),
                visibility: Some("list".to_string()),
                minimal_client_version: Some("0.153.0".to_string()),
                priority: Some(1),
            }
        );
        // gpt-reserve is a server-only slug (absent from the upstream codex-rs
        // models.json); its fields come from the live 0.154.0 snapshot, so
        // regenerating from upstream alone would drop it (see runbook §5).
        assert_eq!(
            bundled.models["gpt-reserve"],
            CodexModelCapability {
                slug: "gpt-reserve".to_string(),
                use_responses_lite: true,
                prefer_websockets: Some(true),
                supported_in_api: Some(true),
                visibility: Some("hide".to_string()),
                minimal_client_version: Some("0.144.0".to_string()),
                priority: Some(3),
            }
        );
        assert!(ClientVersion::parse(CODEX_MANIFEST_FALLBACK_CLIENT_VERSION).is_some());
    }

    #[test]
    fn manifest_parse_keeps_whitelisted_fields_and_skips_slugless_entries() {
        let body = json!({"models": [
            {"slug": "gpt-5.7-nova", "use_responses_lite": true, "base_instructions": "SECRET PROMPT",
             "minimal_client_version": "0.160.0", "priority": 3, "visibility": "list"},
            {"slug": "gpt-5.5", "prefer_websockets": false},
            {"use_responses_lite": true},
            {"slug": "   "},
            "not an object"
        ]});
        let models = parse_codex_models_manifest(&body).expect("parses");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].slug, "gpt-5.7-nova");
        assert!(models[0].use_responses_lite);
        assert_eq!(models[0].minimal_client_version.as_deref(), Some("0.160.0"));
        assert!(!models[1].use_responses_lite);
        assert_eq!(models[1].prefer_websockets, Some(false));
        let serialized = serde_json::to_string(&models).expect("serializes");
        assert!(!serialized.contains("SECRET"));
        assert!(!serialized.contains("base_instructions"));

        for body in [
            json!({}),
            json!({"models": {}}),
            json!({"models": []}),
            json!([]),
        ] {
            assert!(parse_codex_models_manifest(&body).is_err(), "{body}");
        }
        assert!(
            parse_codex_models_manifest(&json!({"models": [{"use_responses_lite": true}]}))
                .is_err()
        );
    }

    #[test]
    fn snapshot_lookup_normalizes_slug_case_and_whitespace() {
        let snapshot = CodexModelCatalogSnapshot::from_models(
            "0.155.0",
            Some("\"abc\"".to_string()),
            10,
            vec![
                CodexModelCapability {
                    slug: " GPT-5.7-Nova ".to_string(),
                    use_responses_lite: true,
                    ..Default::default()
                },
                CodexModelCapability {
                    slug: "gpt-5.5".to_string(),
                    ..Default::default()
                },
            ],
        );
        assert_eq!(snapshot.uses_responses_lite("gpt-5.7-nova"), Some(true));
        assert_eq!(snapshot.uses_responses_lite(" GPT-5.7-NOVA"), Some(true));
        assert_eq!(snapshot.uses_responses_lite("gpt-5.5"), Some(false));
        assert_eq!(snapshot.uses_responses_lite("gpt-5.6-sol"), None);
        assert_eq!(snapshot.uses_responses_lite(""), None);
        assert_eq!(
            snapshot.lite_slugs().into_iter().collect::<Vec<_>>(),
            vec!["gpt-5.7-nova".to_string()]
        );
        assert_eq!(snapshot.age_secs(25), 15);
        assert_eq!(snapshot.age_secs(5), 0);
    }

    #[tokio::test]
    async fn redis_round_trip_by_version_and_newest_version() {
        let runtime = test_runtime();
        assert_eq!(load_snapshot(&runtime, "0.154.0").await.unwrap(), None);
        assert_eq!(load_newest_snapshot(&runtime).await.unwrap(), None);

        let older = CodexModelCatalogSnapshot::from_models(
            "0.146.0",
            None,
            100,
            vec![CodexModelCapability {
                slug: "gpt-5.5".to_string(),
                ..Default::default()
            }],
        );
        let newer = CodexModelCatalogSnapshot::from_models(
            "0.154.0",
            Some("W/\"etag\"".to_string()),
            200,
            vec![CodexModelCapability {
                slug: "gpt-6-astra".to_string(),
                use_responses_lite: true,
                ..Default::default()
            }],
        );
        store_snapshot(&runtime, &older).await.unwrap();
        store_snapshot(&runtime, &newer).await.unwrap();
        // An unrelated key under the domain (the lock, a typo) is ignored.
        runtime
            .kv_set(&catalog_key("refresh_lock"), "x", None)
            .await
            .unwrap();

        assert_eq!(
            load_snapshot(&runtime, "0.154.0").await.unwrap(),
            Some(newer.clone())
        );
        assert_eq!(
            load_snapshot(&runtime, "0.146.0").await.unwrap(),
            Some(older)
        );
        assert_eq!(load_newest_snapshot(&runtime).await.unwrap(), Some(newer));
        let ttl = runtime
            .kv_ttl_seconds(&catalog_key("0.154.0"))
            .await
            .unwrap()
            .expect("ttl set");
        assert!(ttl > 6 * 86_400 && ttl <= 7 * 86_400, "{ttl}");

        runtime
            .kv_set(&catalog_key("0.155.0"), "{not json", None)
            .await
            .unwrap();
        assert!(load_snapshot(&runtime, "0.155.0").await.is_err());
    }

    #[tokio::test]
    async fn followed_version_is_the_registry_maximum_or_the_fallback() {
        let runtime = test_runtime();
        let providers = vec!["prov-a".to_string(), "prov-b".to_string()];
        assert_eq!(
            resolve_followed_codex_client_version(&runtime, &providers).await,
            ClientVersion::parse(CODEX_MANIFEST_FALLBACK_CLIENT_VERSION).unwrap()
        );
        assert_eq!(
            resolve_followed_codex_client_version(&runtime, &[]).await,
            ClientVersion::parse(CODEX_MANIFEST_FALLBACK_CLIENT_VERSION).unwrap()
        );

        let now = unix_now_secs();
        let store_a = ClientReleaseStore::new(&runtime, "prov-a");
        store_a
            .record_release("codex-tui", ClientVersion::parse("0.146.0").unwrap(), now)
            .await
            .unwrap();
        store_a
            .record_release(
                "codex_cli_rs",
                ClientVersion::parse("0.157.2").unwrap(),
                now,
            )
            .await
            .unwrap();
        let store_b = ClientReleaseStore::new(&runtime, "prov-b");
        store_b
            .record_release(
                "codex desktop",
                ClientVersion::parse("0.156.0").unwrap(),
                now,
            )
            .await
            .unwrap();
        assert_eq!(
            store_a.families().await.unwrap(),
            vec!["codex-tui".to_string(), "codex_cli_rs".to_string()]
        );
        assert_eq!(
            store_b.families().await.unwrap(),
            vec!["codex desktop".to_string()]
        );
        assert_eq!(
            ClientReleaseStore::new(&runtime, "prov-none")
                .families()
                .await
                .unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            resolve_followed_codex_client_version(&runtime, &providers).await,
            ClientVersion::parse("0.157.2").unwrap()
        );
        assert_eq!(
            resolve_followed_codex_client_version(&runtime, &providers[1..]).await,
            ClientVersion::parse("0.156.0").unwrap()
        );
        // An older registry than the fallback is still what clients run.
        let runtime = test_runtime();
        ClientReleaseStore::new(&runtime, "prov-a")
            .record_release("codex-tui", ClientVersion::parse("0.146.0").unwrap(), now)
            .await
            .unwrap();
        assert_eq!(
            resolve_followed_codex_client_version(&runtime, &providers).await,
            ClientVersion::parse("0.146.0").unwrap()
        );
    }

    #[test]
    fn lite_drift_classifies_added_removed_and_hidden() {
        let live = CodexModelCatalogSnapshot::from_models(
            "0.155.0",
            None,
            0,
            vec![
                CodexModelCapability {
                    slug: "gpt-5.7-nova".to_string(),
                    use_responses_lite: true,
                    ..Default::default()
                },
                CodexModelCapability {
                    slug: "gpt-5.6-luna".to_string(),
                    use_responses_lite: false,
                    ..Default::default()
                },
                CodexModelCapability {
                    slug: "gpt-5.6-sol".to_string(),
                    use_responses_lite: true,
                    ..Default::default()
                },
                CodexModelCapability {
                    slug: "gpt-5.5".to_string(),
                    ..Default::default()
                },
            ],
        );
        let drift = LiteDrift::compute(&live, bundled_snapshot());
        assert_eq!(drift.added, vec!["gpt-5.7-nova".to_string()]);
        assert_eq!(drift.removed, vec!["gpt-5.6-luna".to_string()]);
        assert_eq!(
            drift.hidden,
            vec![
                "codex-auto-review".to_string(),
                "gpt-5.6-terra".to_string(),
                "gpt-6-astra".to_string(),
                "gpt-daybreak-blue-latest".to_string(),
                "gpt-daybreak-red-latest".to_string(),
                "gpt-reserve".to_string(),
            ]
        );
        assert!(drift.contradicts());

        // Same set as bundled: nothing to say.
        let same = LiteDrift::compute(bundled_snapshot(), bundled_snapshot());
        assert_eq!(same, LiteDrift::default());
        assert!(!same.contradicts());

        // A gate-hidden slug alone is not a contradiction.
        let mut hidden_only = bundled_snapshot().clone();
        hidden_only.models.remove("gpt-6-astra");
        let drift_hidden = LiteDrift::compute(&hidden_only, bundled_snapshot());
        assert_eq!(drift_hidden.hidden, vec!["gpt-6-astra".to_string()]);
        assert!(!drift_hidden.contradicts());
    }

    #[test]
    fn lite_drift_is_warned_once_per_signature() {
        let _guard = GLOBAL_SNAPSHOT_TEST_LOCK.blocking_lock();
        let drift = LiteDrift {
            added: vec!["gpt-drift-test-a".to_string()],
            ..Default::default()
        };
        assert!(report_lite_drift("0.155.0", &drift));
        assert!(!report_lite_drift("0.155.0", &drift));
        let other = LiteDrift {
            added: vec!["gpt-drift-test-b".to_string()],
            ..Default::default()
        };
        assert!(report_lite_drift("0.155.0", &other));
        // A non-contradicting drift never warns, but still resets the signature.
        assert!(!report_lite_drift("0.155.0", &LiteDrift::default()));
        assert!(report_lite_drift("0.155.0", &other));
    }

    #[test]
    fn header_and_body_helpers_are_case_insensitive_and_decode_base64() {
        let headers = BTreeMap::from([
            ("ETag".to_string(), " W/\"abc\" ".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]);
        assert_eq!(
            header_value_ci(&headers, "etag").as_deref(),
            Some("W/\"abc\"")
        );
        assert_eq!(header_value_ci(&headers, "x-missing"), None);
        assert_eq!(
            header_value_ci(
                &BTreeMap::from([("etag".to_string(), "  ".to_string())]),
                "etag"
            ),
            None
        );

        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(br#"{"models":[]}"#);
        let result = ExecutionResult {
            request_id: "r".to_string(),
            candidate_id: None,
            status_code: 200,
            headers: BTreeMap::new(),
            body: Some(ResponseBody {
                json_body: None,
                body_bytes_b64: Some(encoded),
            }),
            telemetry: None,
            error: None,
        };
        assert_eq!(result_json_body(&result), Some(json!({"models": []})));
        let mut without_body = result.clone();
        without_body.body = None;
        assert_eq!(result_json_body(&without_body), None);
        assert_eq!(result_error_text(&without_body), "HTTP 200");
        let mut with_detail = result;
        with_detail.status_code = 401;
        with_detail.body = Some(ResponseBody {
            json_body: Some(json!({"detail": "unauthorized"})),
            body_bytes_b64: None,
        });
        assert!(result_error_text(&with_detail).contains("unauthorized"));
    }

    #[test]
    fn manifest_identity_moves_a_stable_frozen_build_and_skips_a_pre_release() {
        let version = ClientVersion::parse("0.155.0").unwrap();
        let moved = manifest_client_identity(
            "codex-tui/0.146.0 (Mac OS 26.0.1; arm64) iTerm.app/3.6.0 (codex-tui; 0.146.0)",
            "codex_cli_rs",
            version,
        )
        .expect("stable build moves");
        assert_eq!(
            moved.0,
            "codex-tui/0.155.0 (Mac OS 26.0.1; arm64) iTerm.app/3.6.0 (codex-tui; 0.155.0)"
        );
        assert_eq!(moved.1, "codex_cli_rs");
        // Already at the followed build: presented as is.
        let same = manifest_client_identity(
            "Codex Desktop/0.155.0 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.901.41123)",
            "Codex Desktop",
            version,
        )
        .expect("same build presents");
        assert!(same.0.starts_with("Codex Desktop/0.155.0 "));
        // A pre-release frozen build has no stable token to move: fall back.
        assert_eq!(
            manifest_client_identity(
                "Codex Desktop/0.153.0-alpha.5 (Windows 10.0.19045; x86_64) unknown (Codex Desktop; 26.901.20858)",
                "Codex Desktop",
                version,
            ),
            None
        );
        assert_eq!(
            manifest_client_identity("Go-http-client/1.1", "curl", version),
            None
        );
    }

    // -----------------------------------------------------------------------
    // refresh_once through a stub gateway
    // -----------------------------------------------------------------------

    struct StubState {
        runtime: RuntimeState,
        providers: Vec<StoredProviderCatalogProvider>,
        endpoints: Vec<StoredProviderCatalogEndpoint>,
        keys: Vec<StoredProviderCatalogKey>,
        results: Mutex<VecDeque<Result<ExecutionResult, GatewayError>>>,
        executed: Mutex<Vec<ExecutionPlan>>,
    }

    #[async_trait]
    impl ModelFetchTransportRuntime for StubState {
        async fn resolve_local_oauth_request_auth(
            &self,
            transport: &GatewayProviderTransportSnapshot,
        ) -> Result<Option<LocalResolvedOAuthRequestAuth>, String> {
            Ok(Some(LocalResolvedOAuthRequestAuth::Header {
                name: "authorization".to_string(),
                value: format!("Bearer token-for-{}", transport.key.id),
            }))
        }

        async fn resolve_model_fetch_proxy(
            &self,
            _transport: &GatewayProviderTransportSnapshot,
        ) -> Option<ProxySnapshot> {
            None
        }

        async fn execute_model_fetch_execution_plan(
            &self,
            _plan: &ExecutionPlan,
        ) -> Result<ExecutionResult, String> {
            unreachable!("the catalog executes through the gateway runtime")
        }
    }

    #[async_trait]
    impl CodexModelCatalogRuntime for StubState {
        fn runtime_state(&self) -> &RuntimeState {
            &self.runtime
        }

        async fn list_provider_catalog_providers(
            &self,
            _active_only: bool,
        ) -> Result<Vec<StoredProviderCatalogProvider>, GatewayError> {
            Ok(self.providers.clone())
        }

        async fn list_provider_catalog_endpoints_by_provider_ids(
            &self,
            provider_ids: &[String],
        ) -> Result<Vec<StoredProviderCatalogEndpoint>, GatewayError> {
            Ok(self
                .endpoints
                .iter()
                .filter(|endpoint| provider_ids.contains(&endpoint.provider_id))
                .cloned()
                .collect())
        }

        async fn list_provider_catalog_keys_by_provider_ids(
            &self,
            provider_ids: &[String],
        ) -> Result<Vec<StoredProviderCatalogKey>, GatewayError> {
            Ok(self
                .keys
                .iter()
                .filter(|key| provider_ids.contains(&key.provider_id))
                .cloned()
                .collect())
        }

        async fn read_provider_transport_snapshot(
            &self,
            provider_id: &str,
            endpoint_id: &str,
            key_id: &str,
        ) -> Result<Option<GatewayProviderTransportSnapshot>, GatewayError> {
            let endpoint = self
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == endpoint_id && endpoint.provider_id == provider_id);
            let key = self
                .keys
                .iter()
                .find(|key| key.id == key_id && key.provider_id == provider_id);
            let (Some(endpoint), Some(key)) = (endpoint, key) else {
                return Ok(None);
            };
            Ok(Some(GatewayProviderTransportSnapshot {
                provider: GatewayProviderTransportProvider {
                    id: provider_id.to_string(),
                    name: provider_id.to_string(),
                    provider_type: "codex".to_string(),
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
                    id: endpoint.id.clone(),
                    provider_id: provider_id.to_string(),
                    api_format: endpoint.api_format.clone(),
                    api_family: None,
                    endpoint_kind: None,
                    is_active: true,
                    base_url: endpoint.base_url.clone(),
                    header_rules: None,
                    body_rules: None,
                    max_retries: None,
                    custom_path: None,
                    config: None,
                    format_acceptance_config: None,
                    proxy: None,
                },
                key: GatewayProviderTransportKey {
                    id: key.id.clone(),
                    provider_id: provider_id.to_string(),
                    name: key.name.clone(),
                    auth_type: "oauth".to_string(),
                    is_active: true,
                    api_formats: Some(vec!["openai:responses".to_string()]),
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
                    decrypted_api_key: String::new(),
                    decrypted_auth_config: Some(format!(r#"{{"account_id":"acct-{}"}}"#, key.id)),
                },
            }))
        }

        async fn execute_execution_runtime_sync_plan(
            &self,
            plan: &ExecutionPlan,
        ) -> Result<ExecutionResult, GatewayError> {
            self.executed
                .lock()
                .expect("executed mutex")
                .push(plan.clone());
            self.results
                .lock()
                .expect("results mutex")
                .pop_front()
                .unwrap_or_else(|| Err(GatewayError::Internal("no scripted result".to_string())))
        }
    }

    fn stub_provider(id: &str, provider_type: &str) -> StoredProviderCatalogProvider {
        StoredProviderCatalogProvider::new(
            id.to_string(),
            id.to_string(),
            None,
            provider_type.to_string(),
        )
        .expect("provider")
        .with_transport_fields(true, false, false, None, None, None, None, None, None)
    }

    fn stub_endpoint(
        id: &str,
        provider_id: &str,
        api_format: &str,
        base_url: &str,
    ) -> StoredProviderCatalogEndpoint {
        StoredProviderCatalogEndpoint::new(
            id.to_string(),
            provider_id.to_string(),
            api_format.to_string(),
            None,
            None,
            true,
        )
        .expect("endpoint")
        .with_transport_fields(
            base_url.to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("endpoint transport")
    }

    fn stub_key(id: &str, provider_id: &str, is_active: bool) -> StoredProviderCatalogKey {
        let mut key = StoredProviderCatalogKey::new(
            id.to_string(),
            provider_id.to_string(),
            "primary".to_string(),
            "oauth".to_string(),
            None,
            is_active,
        )
        .expect("key")
        .with_transport_fields(
            Some(json!(["openai:responses"])),
            "encrypted".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("key transport");
        // The catalog does not care about the model-sync opt-in.
        key.auto_fetch_models = false;
        key
    }

    fn manifest_result(status_code: u16, body: Value, etag: Option<&str>) -> ExecutionResult {
        let mut headers = BTreeMap::new();
        if let Some(etag) = etag {
            headers.insert("ETag".to_string(), etag.to_string());
        }
        ExecutionResult {
            request_id: "req".to_string(),
            candidate_id: None,
            status_code,
            headers,
            body: Some(ResponseBody {
                json_body: Some(body),
                body_bytes_b64: None,
            }),
            telemetry: None,
            error: None,
        }
    }

    /// A manifest agreeing with the bundled copy on every bundled slug (so
    /// installing it process-wide changes no other test's answer) plus one
    /// new lite model, one entry without a slug and some prompt text.
    fn live_manifest() -> Value {
        let mut models = bundled_snapshot()
            .models
            .values()
            .map(|capability| serde_json::to_value(capability).expect("capability"))
            .collect::<Vec<_>>();
        models.push(json!({
            "slug": "gpt-5.7-nova",
            "use_responses_lite": true,
            "base_instructions": "PROMPT TEXT THAT MUST NOT BE PERSISTED"
        }));
        models.push(json!({"use_responses_lite": true}));
        json!({"models": models})
    }

    fn stub_state(results: Vec<Result<ExecutionResult, GatewayError>>) -> StubState {
        StubState {
            runtime: test_runtime(),
            providers: vec![
                stub_provider("prov-a", "codex"),
                stub_provider("prov-c", "codex"),
                stub_provider("prov-o", "openai"),
            ],
            endpoints: vec![
                stub_endpoint(
                    "ep-a",
                    "prov-a",
                    "openai:responses",
                    "https://chatgpt.com/backend-api/codex",
                ),
                stub_endpoint(
                    "ep-a-compact",
                    "prov-a",
                    "openai:responses:compact",
                    "https://chatgpt.com/backend-api/codex",
                ),
                // A Codex-typed provider on a relay: not the chatgpt backend.
                stub_endpoint(
                    "ep-c",
                    "prov-c",
                    "openai:responses",
                    "https://relay.example/v1",
                ),
                stub_endpoint(
                    "ep-o",
                    "prov-o",
                    "openai:responses",
                    "https://chatgpt.com/backend-api/codex",
                ),
            ],
            keys: vec![
                stub_key("key-b", "prov-a", true),
                stub_key("key-a", "prov-a", true),
                stub_key("key-inactive", "prov-a", false),
                stub_key("key-c", "prov-c", true),
                stub_key("key-o", "prov-o", true),
            ],
            results: Mutex::new(results.into_iter().collect()),
            executed: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn manifest_targets_are_active_keys_of_codex_providers_on_the_chatgpt_backend() {
        let state = stub_state(Vec::new());
        let (provider_ids, targets) = collect_manifest_targets(&state).await.unwrap();
        assert_eq!(
            provider_ids,
            vec!["prov-a".to_string(), "prov-c".to_string()]
        );
        assert_eq!(
            targets,
            vec![
                ManifestFetchTarget {
                    provider_id: "prov-a".to_string(),
                    endpoint_id: "ep-a".to_string(),
                    key_id: "key-a".to_string(),
                },
                ManifestFetchTarget {
                    provider_id: "prov-a".to_string(),
                    endpoint_id: "ep-a".to_string(),
                    key_id: "key-b".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn refresh_rotates_accounts_reuses_a_fresh_document_and_keeps_the_last_snapshot() {
        let _guard = GLOBAL_SNAPSHOT_TEST_LOCK.lock().await;
        clear_snapshot();
        let interval = Duration::from_secs(300);
        let state = stub_state(vec![
            Ok(manifest_result(401, json!({"detail": "expired"}), None)),
            Ok(manifest_result(200, live_manifest(), Some("W/\"m1\""))),
        ]);
        // Real clients seen on 0.155.0: the manifest is asked as that build.
        ClientReleaseStore::new(&state.runtime, "prov-a")
            .record_release(
                "codex-tui",
                ClientVersion::parse("0.155.0").unwrap(),
                unix_now_secs(),
            )
            .await
            .unwrap();

        // Cycle 0 starts at key-a (401) and moves on to key-b (200).
        let outcome = refresh_once(&state, 0, interval).await;
        assert_eq!(
            outcome,
            RefreshOutcome::Refreshed {
                client_version: "0.155.0".to_string(),
                model_count: 13,
                lite_count: 9,
                drift_warned: true,
            }
        );
        {
            let executed = state.executed.lock().unwrap();
            assert_eq!(executed.len(), 2);
            assert_eq!(executed[0].key_id, "key-a");
            assert_eq!(executed[1].key_id, "key-b");
            for plan in executed.iter() {
                assert_eq!(
                    plan.url,
                    "https://chatgpt.com/backend-api/codex/models?client_version=0.155.0"
                );
                assert_eq!(plan.method, "GET");
                // Either the account's own client identity moved to the
                // followed build (UA token + derived `version` header), or,
                // when its frozen build cannot be moved, the neutral
                // model-sync identity without a version claim.
                let user_agent = plan.headers.get("user-agent").expect("user-agent");
                if user_agent == "openai-codex/1.0" {
                    assert!(!plan.headers.contains_key("version"), "{plan:?}");
                    assert!(!plan.headers.contains_key("originator"), "{plan:?}");
                } else {
                    assert!(user_agent.contains("/0.155.0 "), "{user_agent}");
                    assert_eq!(
                        plan.headers.get("version").map(String::as_str),
                        Some("0.155.0")
                    );
                    assert!(plan
                        .headers
                        .get("originator")
                        .is_some_and(|originator| !originator.is_empty()));
                }
                assert_eq!(
                    plan.headers.get("chatgpt-account-id").map(String::as_str),
                    Some(format!("acct-{}", plan.key_id).as_str())
                );
                assert!(plan.headers["authorization"].starts_with("Bearer token-for-"));
            }
        }
        let live = current_snapshot().expect("snapshot installed");
        assert_eq!(live.client_version, "0.155.0");
        assert_eq!(live.etag.as_deref(), Some("W/\"m1\""));
        assert_eq!(live.uses_responses_lite("gpt-5.7-nova"), Some(true));
        assert_eq!(live.uses_responses_lite("gpt-5.6-luna"), Some(true));
        assert_eq!(live.uses_responses_lite("gpt-5.5"), Some(false));
        assert!(
            crate::codex_routing_hint::model_name_uses_responses_lite("gpt-5.7-nova"),
            "the routing hint follows the installed snapshot"
        );
        let stored = load_snapshot(&state.runtime, "0.155.0")
            .await
            .unwrap()
            .expect("persisted");
        assert_eq!(&stored, live.as_ref());
        assert!(!serde_json::to_string(&stored)
            .unwrap()
            .contains("PROMPT TEXT"));

        // Next cycle inside the interval: the document is fresh, no request.
        let outcome = refresh_once(&state, 1, interval).await;
        assert_eq!(
            outcome,
            RefreshOutcome::Installed {
                client_version: "0.155.0".to_string()
            }
        );
        assert_eq!(state.executed.lock().unwrap().len(), 2);

        // Document aged out, another instance holds the lock: skipped.
        let mut aged = stored.clone();
        aged.fetched_at_unix_secs = unix_now_secs().saturating_sub(10_000);
        store_snapshot(&state.runtime, &aged).await.unwrap();
        let lease = state
            .runtime
            .lock_try_acquire(CATALOG_LOCK_KEY, "other-instance", CATALOG_LOCK_TTL)
            .await
            .unwrap()
            .expect("lock acquired");
        assert_eq!(
            refresh_once(&state, 1, interval).await,
            RefreshOutcome::Skipped("locked")
        );
        assert_eq!(state.executed.lock().unwrap().len(), 2);
        state.runtime.lock_release(&lease).await.unwrap();

        // Every account fails: the previous snapshot stays installed and the
        // lock is released for the next tick.
        state.results.lock().unwrap().extend([
            Err(GatewayError::Internal("connect timeout".to_string())),
            Ok(manifest_result(
                500,
                json!({"error": {"message": "boom"}}),
                None,
            )),
        ]);
        assert_eq!(
            refresh_once(&state, 1, interval).await,
            RefreshOutcome::Failed { attempts: 2 }
        );
        {
            let executed = state.executed.lock().unwrap();
            assert_eq!(executed.len(), 4);
            // Cycle 1 starts at the second account.
            assert_eq!(executed[2].key_id, "key-b");
            assert_eq!(executed[3].key_id, "key-a");
        }
        assert_eq!(
            current_snapshot().unwrap().etag.as_deref(),
            Some("W/\"m1\"")
        );
        let probe = state
            .runtime
            .lock_try_acquire(CATALOG_LOCK_KEY, "probe", CATALOG_LOCK_TTL)
            .await
            .unwrap()
            .expect("lock released after the failed round");
        state.runtime.lock_release(&probe).await.unwrap();

        // A response without a `models` array never installs anything new.
        aged.fetched_at_unix_secs = 1;
        store_snapshot(&state.runtime, &aged).await.unwrap();
        state.results.lock().unwrap().push_back(Ok(manifest_result(
            200,
            json!({"data": []}),
            None,
        )));
        assert!(matches!(
            refresh_once(&state, 0, interval).await,
            RefreshOutcome::Failed { .. }
        ));
        assert_eq!(
            current_snapshot().unwrap().fetched_at_unix_secs,
            live.fetched_at_unix_secs
        );

        clear_snapshot();
    }

    #[tokio::test]
    async fn refresh_without_a_codex_account_is_skipped_and_startup_restores_redis() {
        let _guard = GLOBAL_SNAPSHOT_TEST_LOCK.lock().await;
        clear_snapshot();
        let mut state = stub_state(Vec::new());
        state.keys.retain(|key| key.provider_id != "prov-a");
        assert_eq!(
            refresh_once(&state, 0, Duration::from_secs(300)).await,
            RefreshOutcome::Skipped("no_codex_account")
        );
        assert!(state.executed.lock().unwrap().is_empty());
        assert!(current_snapshot().is_none());

        // Start-up finds the newest persisted document even when the followed
        // version has none of its own.
        let persisted = CodexModelCatalogSnapshot::from_models(
            "0.150.0",
            None,
            unix_now_secs(),
            bundled_snapshot().models.values().cloned().collect(),
        );
        store_snapshot(&state.runtime, &persisted).await.unwrap();
        load_persisted_snapshot(&state).await;
        assert_eq!(current_snapshot().as_deref(), Some(&persisted));
        clear_snapshot();
    }
}
