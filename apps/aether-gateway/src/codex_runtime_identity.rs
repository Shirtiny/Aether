//! Codex pool outbound runtime identity synthesis.
//!
//! Inbound official Codex identity (`session_id` / `thread_id` / `turn_id` /
//! `window_id`) keeps owning sticky routing, WebSocket binding and usage
//! settlement. When `pool_advanced.codex_runtime_identity.enabled` is `true`
//! on a Codex pool provider, the identity that the *selected account* shows
//! upstream is replaced, after key selection, by a per-account, per-day
//! synthetic tree negotiated through the shared runtime state:
//!
//! - hard account-level ceilings over a trailing 24h window: at most N
//!   threads active and at most M turns minted (`expected_threads_per_day` /
//!   `expected_turns_per_day`). The effective ceiling of a given day is drawn
//!   per account from `[ceil(N/2), N]` / `[ceil(M/2), M]`, so busy accounts
//!   do not all saturate at the same configured number every day; the
//!   configured value is never exceeded in any 24h window
//! - threads grow with activity: a new inbound root mints a new thread while
//!   the account's active roster is below its ceiling (and a turn is still
//!   affordable), then reuses the least recently active thread
//! - one open turn per synthetic thread, like a real thread: a new inbound
//!   turn mints the next outbound turn while the account's turn budget lasts,
//!   otherwise it continues (steers into) the thread's open turn. Turn ids on
//!   one thread therefore never interleave or come back
//! - the same inbound root keeps the same outbound thread across HTTP compact,
//!   Search, WebSocket reconnects and day rollovers while its freeze is alive
//! - the same inbound turn keeps its outbound turn while it is the thread's
//!   open turn; `x-codex-turn-state` is forwarded only in that case
//! - all UUIDs are UUIDv7 minted at first binding through `SET NX` / atomic
//!   sliding-window admission
//!
//! Anything the store cannot answer falls back to passthrough. Nothing is
//! minted in-process without the store.
//!
//! See `docs/architecture/codex-pool-runtime-identity-synthesis-plan-2026-09-03.md`.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_runtime_state::RuntimeState;
use http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::codex_profile::{hex_lower, serialize_ascii_json};

pub(crate) const CODEX_RUNTIME_IDENTITY_KEY: &str = "codex_runtime_identity";

const MIN_THREADS_PER_DAY: u64 = 1;
const MAX_THREADS_PER_DAY: u64 = 64;
const MIN_TURNS_PER_DAY: u64 = 1;
const MAX_TURNS_PER_DAY: u64 = 512;
const DAY_WINDOW_SECS: u64 = 86_400;
/// State outlives the 24h ceiling window by this much, so an idle roster /
/// ledger entry is still there to be counted when the window slides past it.
const TTL_GRACE_SECS: u64 = 43_200;

const SELECTION_FP_DOMAIN: &[u8] = b"aether:codex:rid:sel:v1";
const JITTER_DOMAIN: &[u8] = b"aether:codex:rid:jitter:v1";
const BOUND_DOMAIN: &[u8] = b"aether:codex:rid:bound:v1";

const X_CODEX_TURN_METADATA: &str = "x-codex-turn-metadata";
const X_CODEX_WINDOW_ID: &str = "x-codex-window-id";
const X_CODEX_PARENT_THREAD_ID: &str = "x-codex-parent-thread-id";
const X_OPENAI_SUBAGENT: &str = "x-openai-subagent";
const X_CODEX_TURN_STATE: &str = "x-codex-turn-state";
const X_CLIENT_REQUEST_ID: &str = "x-client-request-id";
const SESSION_ID_HEADER: &str = "session-id";
const THREAD_ID_HEADER: &str = "thread-id";
const GUARDIAN_PROMPT_CACHE_PREFIX: &str = "guardian:";
/// `AgentPath::root()` — the only agent a synthetic root thread ever has.
const ROOT_AGENT_NAME: &str = "/root";
/// `ThreadSource::User` — every folded thread presents as a plain user thread.
const USER_THREAD_SOURCE: &str = "user";

// Outbound field whitelist. Every key a request can carry on a surface falls
// into exactly one class: rewritten to the synthetic identity, normalized to
// the root user-thread shape, removed as an inbound-tree marker, or forwarded
// verbatim. Anything else is unknown: removed and reported once per process
// as `codex_rid_unknown_metadata_key`, so a new codex-rs field can never leak
// through unrewritten (the `window_number` / `context_window_id` lesson).
// Reference: `core/src/responses_metadata.rs` @ codex-rs 07f18d5f.

/// Turn-metadata blob keys rewritten to the synthetic identity. Official
/// `request_kind=memory` blobs omit all of them. `installation_id` is owned by
/// the account profile pass and listed only so it is known here.
const BLOB_IDENTITY_KEYS: &[&str] = &[
    "installation_id",
    "session_id",
    "thread_id",
    "turn_id",
    "window_id",
    "window_number",
    "context_window_id",
];
/// Blob keys normalized instead of removed: `agent_name` → `/root`,
/// `thread_source` → `user` (memory keeps `memory_consolidation`),
/// `root_turn_id` → the outbound turn (root turns are their own root),
/// `sandbox` → the platform sandbox of the outbound user-agent's OS.
const BLOB_NORMALIZED_KEYS: &[&str] = &["agent_name", "thread_source", "root_turn_id", "sandbox"];
/// Blob keys that only exist on forked / child threads; a root thread never
/// carries them.
const BLOB_LEAK_KEYS: &[&str] = &[
    "forked_from_thread_id",
    "forked_from_ordinal_exclusive",
    "parent_thread_id",
    "parent_turn_id",
    "subagent_kind",
];
/// Blob keys forwarded verbatim (`CodexResponsesMetadata` fields plus the
/// Desktop `workspace_kind` extra observed in production). On request kinds
/// that carry the request identity the ones a current client always sends
/// are filled with the default-configuration value when an older inbound
/// client omitted them (`request_identity_blob`); `BLOB_APP_SERVER_KEYS` are
/// kept only under an app-server user-agent.
const BLOB_PASS_KEYS: &[&str] = &[
    "request_kind",
    "compaction",
    "turn_trigger",
    "sandbox_mode",
    "auto_review_enabled",
    "node_repl_auto_review_required",
    "node_repl_disabled",
    "workspaces",
    "workspace_kind",
    "tool_namespaces_info",
    "turn_started_at_unix_ms",
    "history_ingest_requested",
];
/// Blob keys only an app-server host (Desktop, VS Code, `codex_app`) ever
/// sets: `turn_trigger` comes from `app-server/src/turn_processor.rs` and
/// `workspace_kind` from its `responsesapi_client_metadata`; the TUI, `codex
/// exec` and `codex_cli_rs` pass both as `None`. Under a terminal user-agent
/// they are a shape no real client produces, so they are dropped there.
const BLOB_APP_SERVER_KEYS: &[&str] = &["turn_trigger", "workspace_kind"];
/// Originators of the terminal clients (`codex-rs` `tui`, `exec`, `cli`
/// default), as they lead the official user-agent.
const TERMINAL_ORIGINATOR_PREFIXES: &[&str] = &["codex-tui/", "codex_exec/", "codex_cli_rs/"];
/// Flat `client_metadata` keys rewritten to the synthetic identity (the
/// installation id by the profile pass, the blob by the blob pass).
const FLAT_IDENTITY_KEYS: &[&str] = &[
    "x-codex-installation-id",
    "session_id",
    "thread_id",
    "turn_id",
    "root_turn_id",
    X_CODEX_WINDOW_ID,
    X_CODEX_TURN_METADATA,
    X_CODEX_TURN_STATE,
];
/// Flat `client_metadata` keys that expose the inbound session tree.
const FLAT_LEAK_KEYS: &[&str] = &[
    X_CODEX_PARENT_THREAD_ID,
    X_OPENAI_SUBAGENT,
    "parent_thread_id",
    "forked_from_thread_id",
    "parent_turn_id",
    "subagent_kind",
    "thread_source",
];
/// Flat keys forwarded verbatim (`client_metadata()` WebSocket extras and the
/// guardian receipt keys).
const FLAT_PASS_KEYS: &[&str] = &[
    "ws_request_header_x_openai_internal_codex_responses_lite",
    "x-codex-ws-stream-request-start-ms",
    "guardian_ticket",
    "guardian_ticket_requested",
];
/// Aether's own WebSocket step-control keys are not client metadata.
const FLAT_CONTROL_PREFIXES: &[&str] = &["sub2api_", "aether."];
/// Request header prefixes that carry Codex client identity or routing.
const HEADER_IDENTITY_PREFIXES: &[&str] = &["x-codex-", "x-openai-", "x-oai-", "x-responsesapi-"];
/// Prefixed headers a real codex-rs client sends and Aether forwards
/// (rewritten above where they carry identity).
const HEADER_PASS_KEYS: &[&str] = &[
    "x-codex-installation-id",
    X_CODEX_WINDOW_ID,
    X_CODEX_TURN_METADATA,
    X_CODEX_TURN_STATE,
    "x-codex-beta-features",
    "x-codex-routing-hint",
    // Preserve the managed US residency routing hint when a Codex client sends it.
    "x-openai-internal-codex-residency",
    "x-openai-internal-codex-responses-lite",
    "x-openai-memgen-request",
    "x-responsesapi-include-timing-metrics",
];
/// Prefixed headers removed without a report: tree markers, and attestation
/// (already dropped by the pool blocklist; must never resurface).
const HEADER_STRIP_KEYS: &[&str] = &[
    X_CODEX_PARENT_THREAD_ID,
    X_OPENAI_SUBAGENT,
    "x-oai-attestation",
];
/// HTTP compatibility short headers Aether derives from `prompt_cache_key`.
/// Official Codex HTTP clients never send them.
const SHORT_HEADERS: &[&str] = &["session_id", "conversation_id"];

// Synthetic root for HTTP `/responses` requests without any official Codex
// identity (typically a downstream relay that strips `x-codex-*`, `session-id`
// and `thread-id` from a real client). Upstream would otherwise see a Codex
// user-agent / originator with no thread at all — a shape codex-rs never
// produces. The request carries no ids, so the thread is anchored on what a
// real client keeps constant across a conversation (the first real user
// prompt, `store:false` history is replayed verbatim) and the turn on what a
// real client keeps constant across the requests of one turn (the latest user
// prompt; tool-call follow-ups only append items after it).
const SYNTHETIC_ROOT_DOMAIN: &[u8] = b"aether:codex:rid:synthetic-root:v1";
const SYNTHETIC_TURN_DOMAIN: &[u8] = b"aether:codex:rid:synthetic-turn:v1";
const DOWNSTREAM_FP_DOMAIN: &[u8] = b"aether:codex:rid:downstream:v1";
/// Headers that tell downstream callers apart (relay user id first, then the
/// credential). Only a domain-separated hash of them is ever used or kept.
const DOWNSTREAM_IDENTITY_HEADERS: &[&str] = &["cafecode-uid", "authorization", "x-api-key"];
/// codex-rs `prompts/templates/compact/summary_prefix.md`: compaction
/// summaries are re-injected as user messages starting with this text.
pub(crate) const COMPACT_SUMMARY_PREFIX: &str =
    "Another language model started to solve this problem";
const X_CODEX_INSTALLATION_ID: &str = "x-codex-installation-id";
const USER_AGENT_HEADER: &str = "user-agent";
const REQUEST_KIND_TURN: &str = "turn";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexRuntimeIdentityConfig {
    pub(crate) expected_threads_per_day: u32,
    pub(crate) expected_turns_per_day: u32,
}

/// Write-path validation for `pool_advanced.codex_runtime_identity`.
///
/// `enabled: false` (or a missing `enabled`) is a valid "off" and does not
/// require the bounds. When enabled, both bounds are required.
pub(crate) fn validate_codex_runtime_identity_config(value: &Value) -> Result<(), String> {
    parse_codex_runtime_identity_config(value).map(|_| ())
}

/// Parses the object; `Ok(None)` means explicitly disabled.
pub(crate) fn parse_codex_runtime_identity_config(
    value: &Value,
) -> Result<Option<CodexRuntimeIdentityConfig>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "pool_advanced.codex_runtime_identity 必须是 JSON 对象".to_string())?;
    let enabled = match object.get("enabled") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(enabled)) => *enabled,
        Some(_) => return Err("codex_runtime_identity.enabled 必须是布尔值".to_string()),
    };
    let threads = bounded_field(
        object,
        "expected_threads_per_day",
        MIN_THREADS_PER_DAY,
        MAX_THREADS_PER_DAY,
        enabled,
    )?;
    let turns = bounded_field(
        object,
        "expected_turns_per_day",
        MIN_TURNS_PER_DAY,
        MAX_TURNS_PER_DAY,
        enabled,
    )?;
    if !enabled {
        return Ok(None);
    }
    match (threads, turns) {
        (Some(threads), Some(turns)) => Ok(Some(CodexRuntimeIdentityConfig {
            expected_threads_per_day: threads,
            expected_turns_per_day: turns,
        })),
        // `bounded_field` already rejected missing required values.
        _ => Err(
            "codex_runtime_identity 缺少 expected_threads_per_day / expected_turns_per_day"
                .to_string(),
        ),
    }
}

fn bounded_field(
    object: &Map<String, Value>,
    field: &str,
    min: u64,
    max: u64,
    required: bool,
) -> Result<Option<u32>, String> {
    let error = || format!("codex_runtime_identity.{field} 必须是 {min} 到 {max} 之间的整数");
    match object.get(field) {
        None | Some(Value::Null) => {
            if required {
                Err(error())
            } else {
                Ok(None)
            }
        }
        Some(value) => match value.as_u64() {
            Some(number) if (min..=max).contains(&number) => Ok(Some(number as u32)),
            _ => Err(error()),
        },
    }
}

/// Read-path resolution. Missing object / `enabled: false` → `None`.
/// An invalid object also yields `None` (synthesis off) and logs
/// `codex_rid_config_invalid`; it never silently clamps to defaults.
pub(crate) fn codex_runtime_identity_rewrite_enabled(
    pool_advanced: Option<&Value>,
    provider_id: &str,
) -> Option<CodexRuntimeIdentityConfig> {
    let value = pool_advanced?.get(CODEX_RUNTIME_IDENTITY_KEY)?;
    if value.is_null() {
        return None;
    }
    match parse_codex_runtime_identity_config(value) {
        Ok(config) => config,
        Err(error) => {
            warn!(
                event_name = "codex_rid_config_invalid",
                log_type = "event",
                provider_id = %provider_id,
                error = %error,
                "codex runtime identity config is invalid; synthesis disabled for this provider"
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Scope: fixed once the pool account is selected
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexRuntimeIdentityScope {
    pub(crate) provider_id: String,
    selection_key: String,
    /// `hex(SHA256("aether:codex:rid:sel:v1" || selection_key)[0..16])`
    pub(crate) selection_fp: String,
    pub(crate) config: CodexRuntimeIdentityConfig,
    account_jitter_secs: u64,
}

impl CodexRuntimeIdentityScope {
    pub(crate) fn new(
        provider_id: &str,
        selection_key: &str,
        config: CodexRuntimeIdentityConfig,
    ) -> Self {
        let selection_fp =
            hex_lower(&sha256(&[SELECTION_FP_DOMAIN, selection_key.as_bytes()])[..16]);
        let account_jitter_secs =
            u64_prefix(&sha256(&[JITTER_DOMAIN, selection_key.as_bytes()])) % DAY_WINDOW_SECS;
        Self {
            provider_id: provider_id.to_string(),
            selection_key: selection_key.to_string(),
            selection_fp,
            config,
            account_jitter_secs,
        }
    }

    fn shifted_secs(&self, now: SystemTime) -> u64 {
        unix_secs(now).saturating_add(self.account_jitter_secs)
    }

    pub(crate) fn day_id(&self, now: SystemTime) -> u64 {
        self.shifted_secs(now) / DAY_WINDOW_SECS
    }

    /// Lifetime of every state key: the 24h ceiling window plus a 12h grace,
    /// slid on activity. Constant, so a roster or ledger entry is always still
    /// present while it can count against a window.
    pub(crate) fn ttl(&self) -> Duration {
        Duration::from_secs(DAY_WINDOW_SECS + TTL_GRACE_SECS)
    }

    /// Number of threads this account may have active in the trailing 24h on
    /// `day_id`: a deterministic draw from `[ceil(N/2), N]`. The configured
    /// value is the ceiling; the per-day draw keeps busy accounts from all
    /// showing exactly N threads every day, which no population of real codex
    /// users produces.
    fn thread_bound(&self, day_id: u64) -> u64 {
        let digest = sha256(&[
            BOUND_DOMAIN,
            b"\0thread\0",
            self.selection_key.as_bytes(),
            b"\0",
            day_id.to_string().as_bytes(),
        ]);
        jittered_bound(self.config.expected_threads_per_day, &digest)
    }

    /// Number of turns this account may mint in the trailing 24h on `day_id`:
    /// a deterministic draw from `[ceil(M/2), M]`. Account-level, not per
    /// thread: the official per-account turn count is what risk control sees.
    fn turn_bound(&self, day_id: u64) -> u64 {
        let digest = sha256(&[
            BOUND_DOMAIN,
            b"\0turn\0",
            self.selection_key.as_bytes(),
            b"\0",
            day_id.to_string().as_bytes(),
        ]);
        jittered_bound(self.config.expected_turns_per_day, &digest)
    }

    fn key_prefix(&self) -> String {
        format!("ap:{}:codex_rid:{}", self.provider_id, self.selection_fp)
    }

    /// The account's threads: a sorted set of outbound thread ids scored by
    /// last activity (unix seconds). Members active in the trailing 24h count
    /// against `thread_bound`; the least recently active one is reused once
    /// the ceiling is reached.
    fn thread_roster_key(&self) -> String {
        format!("{}:threads", self.key_prefix())
    }

    /// The account's minted turns: a sorted set of outbound turn ids scored by
    /// mint time (unix seconds). Members minted in the trailing 24h count
    /// against `turn_bound`.
    fn turn_ledger_key(&self) -> String {
        format!("{}:turns", self.key_prefix())
    }

    /// The open turn of one synthetic thread (`OpenTurn`): the turn every
    /// request on the thread continues until the next one is minted.
    fn open_turn_key(&self, outbound_thread_id: &str) -> String {
        format!("{}:open:{outbound_thread_id}", self.key_prefix())
    }

    fn freeze_key(&self, inbound_root_hash: &str) -> String {
        format!("{}:freeze:{inbound_root_hash}", self.key_prefix())
    }

    /// Context-window state of one synthetic thread (not day-scoped: a thread
    /// keeps its window across day rollovers like a real one).
    fn window_key(&self, outbound_thread_id: &str) -> String {
        format!("{}:window:{outbound_thread_id}", self.key_prefix())
    }

    fn turn_freeze_key(&self, inbound_root_hash: &str, inbound_turn_hash: &str) -> String {
        format!(
            "{}:freeze:{inbound_root_hash}:turn:{inbound_turn_hash}",
            self.key_prefix()
        )
    }
}

// ---------------------------------------------------------------------------
// Inbound identity (read-only projection of what the client sent)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexRequestKind {
    Turn,
    Prewarm,
    Compaction,
    Memory,
    Other,
}

impl CodexRequestKind {
    fn parse(value: &str) -> Self {
        match value.trim() {
            "turn" => Self::Turn,
            "prewarm" => Self::Prewarm,
            "compaction" => Self::Compaction,
            "memory" => Self::Memory,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InboundCodexRuntimeIdentity {
    pub(crate) session_id: Option<String>,
    pub(crate) thread_id: Option<String>,
    pub(crate) turn_id: Option<String>,
    pub(crate) window_id: Option<String>,
    pub(crate) request_kind: Option<CodexRequestKind>,
    /// Whether the *original* client body carried a non-empty
    /// `prompt_cache_key`. Aether fillers may have inserted a UUIDv5 since.
    pub(crate) prompt_cache_key_present: bool,
    pub(crate) previous_response_id_present: bool,
    /// Root / turn derived from the request content when it carries no
    /// official identity (see `synthesize_missing_root`).
    pub(crate) synthetic: Option<SyntheticCodexIdentity>,
}

/// Content-derived identity of a request without official Codex ids. Both
/// values are 16-byte domain-separated hashes; neither the prompt text nor the
/// downstream credential is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntheticCodexIdentity {
    root: String,
    turn_key: String,
}

impl InboundCodexRuntimeIdentity {
    /// Precedence mirrors `client_session_affinity`: body turn-metadata blob →
    /// body flat `client_metadata` → header turn-metadata blob → dash headers.
    pub(crate) fn from_request(body: Option<&Value>, headers: Option<&HeaderMap>) -> Self {
        let mut inbound = Self::default();
        if let Some(body) = body {
            if let Some(client_metadata) = body.get("client_metadata").and_then(Value::as_object) {
                if let Some(blob) = client_metadata.get(X_CODEX_TURN_METADATA) {
                    inbound.absorb_blob_value(blob);
                }
                inbound.fill_session(client_metadata.get("session_id"));
                inbound.fill_thread(client_metadata.get("thread_id"));
                inbound.fill_turn(client_metadata.get("turn_id"));
                inbound.fill_window(client_metadata.get(X_CODEX_WINDOW_ID));
            }
            inbound.prompt_cache_key_present =
                non_empty_str(body.get("prompt_cache_key")).is_some();
            inbound.previous_response_id_present =
                non_empty_str(body.get("previous_response_id")).is_some();
        }
        if let Some(headers) = headers {
            if let Some(raw) = header_str(headers, X_CODEX_TURN_METADATA) {
                if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                    inbound.absorb_blob_value(&parsed);
                }
            }
            inbound.fill_session_str(header_str(headers, SESSION_ID_HEADER));
            inbound.fill_thread_str(header_str(headers, THREAD_ID_HEADER));
            inbound.fill_window_str(header_str(headers, X_CODEX_WINDOW_ID));
        }
        inbound
    }

    fn absorb_blob_value(&mut self, blob: &Value) {
        let parsed;
        let object = match blob {
            Value::String(raw) => {
                let Ok(value) = serde_json::from_str::<Value>(raw) else {
                    return;
                };
                parsed = value;
                parsed.as_object()
            }
            other => other.as_object(),
        };
        let Some(object) = object else {
            return;
        };
        self.fill_session(object.get("session_id"));
        self.fill_thread(object.get("thread_id"));
        self.fill_turn(object.get("turn_id"));
        self.fill_window(object.get("window_id"));
        if self.request_kind.is_none() {
            self.request_kind =
                non_empty_str(object.get("request_kind")).map(CodexRequestKind::parse);
        }
    }

    fn fill_session(&mut self, value: Option<&Value>) {
        self.fill_session_str(non_empty_str(value));
    }
    fn fill_thread(&mut self, value: Option<&Value>) {
        self.fill_thread_str(non_empty_str(value));
    }
    fn fill_turn(&mut self, value: Option<&Value>) {
        if self.turn_id.is_none() {
            self.turn_id = non_empty_str(value).map(str::to_string);
        }
    }
    fn fill_window(&mut self, value: Option<&Value>) {
        self.fill_window_str(non_empty_str(value));
    }
    fn fill_session_str(&mut self, value: Option<&str>) {
        if self.session_id.is_none() {
            self.session_id = value.map(str::to_string);
        }
    }
    fn fill_thread_str(&mut self, value: Option<&str>) {
        if self.thread_id.is_none() {
            self.thread_id = value.map(str::to_string);
        }
    }
    fn fill_window_str(&mut self, value: Option<&str>) {
        if self.window_id.is_none() {
            self.window_id = value.map(str::to_string);
        }
    }

    /// Official root: `session_id` when present, otherwise `thread_id`.
    /// Same rule as `CodexSessionIdentity::root_session`.
    fn official_root(&self) -> Option<&str> {
        self.session_id.as_deref().or(self.thread_id.as_deref())
    }

    /// Root the outbound thread is bound to: the official root, otherwise the
    /// synthetic root when one was derived.
    pub(crate) fn root(&self) -> Option<&str> {
        self.official_root().or_else(|| {
            self.synthetic
                .as_ref()
                .map(|synthetic| synthetic.root.as_str())
        })
    }

    /// Official `turn_id`, otherwise `root || thread || window` so a turn-less
    /// client still maps every request of one thread/window to one slot;
    /// synthetic requests use the turn key derived from the latest prompt.
    pub(crate) fn turn_key(&self) -> Option<String> {
        if let Some(turn_id) = self.turn_id.as_deref() {
            return Some(turn_id.to_string());
        }
        if let Some(root) = self.official_root() {
            return Some(format!(
                "{root}\0{}\0{}",
                self.thread_id.as_deref().unwrap_or(""),
                self.window_id.as_deref().unwrap_or("")
            ));
        }
        self.synthetic
            .as_ref()
            .map(|synthetic| synthetic.turn_key.clone())
    }

    /// The request carried no official identity and its root was derived from
    /// the content: every outbound projection has to be materialized.
    pub(crate) fn is_synthetic(&self) -> bool {
        self.official_root().is_none() && self.synthetic.is_some()
    }

    /// Derives a synthetic root / turn key for an HTTP `/responses` request
    /// that carries no official Codex identity. Returns `false` (and changes
    /// nothing) when an official root exists or the body has no `input`.
    ///
    /// * root = H(downstream caller, first real user prompt): a real client
    ///   replays its `store:false` history verbatim, so the first prompt is
    ///   constant for the whole conversation.
    /// * turn = H(root, index and text of the latest real user prompt): the
    ///   requests of one turn (retries, tool-call follow-ups) share it; the
    ///   next prompt starts a new turn.
    /// * without a usable prompt, or with a `previous_response_id` chain
    ///   (history lives upstream, no prompt is stable): one thread per
    ///   downstream caller, one turn per input shape.
    ///
    /// Injected wrapper messages (`<user_instructions>`,
    /// `<environment_context>`, …) and compaction summaries are not prompts.
    pub(crate) fn synthesize_missing_root(
        &mut self,
        body: Option<&Value>,
        headers: &HeaderMap,
    ) -> bool {
        if self.official_root().is_some() || self.synthetic.is_some() {
            return false;
        }
        let Some(input) = body.and_then(|body| body.get("input")) else {
            return false;
        };
        let downstream = downstream_fingerprint(headers);
        let prompts = real_user_prompts(input);
        let (root, turn_key) = match (prompts.first(), prompts.last()) {
            (Some((_, first)), Some((last_index, last))) if !self.previous_response_id_present => {
                let root = hex_lower(
                    &sha256(&[
                        SYNTHETIC_ROOT_DOMAIN,
                        downstream.as_bytes(),
                        SEP,
                        first.as_bytes(),
                    ])[..16],
                );
                let turn_key = hex_lower(
                    &sha256(&[
                        SYNTHETIC_TURN_DOMAIN,
                        root.as_bytes(),
                        SEP,
                        last_index.to_string().as_bytes(),
                        SEP,
                        last.as_bytes(),
                    ])[..16],
                );
                (root, turn_key)
            }
            _ => {
                let root = hex_lower(
                    &sha256(&[
                        SYNTHETIC_ROOT_DOMAIN,
                        downstream.as_bytes(),
                        SEP,
                        b"no-prompt",
                    ])[..16],
                );
                let input_len = input.as_array().map_or(1, Vec::len);
                let turn_key = hex_lower(
                    &sha256(&[
                        SYNTHETIC_TURN_DOMAIN,
                        root.as_bytes(),
                        SEP,
                        input_len.to_string().as_bytes(),
                    ])[..16],
                );
                (root, turn_key)
            }
        };
        self.synthetic = Some(SyntheticCodexIdentity { root, turn_key });
        true
    }

    pub(crate) fn is_memory(&self) -> bool {
        self.request_kind == Some(CodexRequestKind::Memory)
    }

    /// Local (`/responses`, `request_kind=compaction`) and remote
    /// (`/responses/compact`) compactions both carry this kind in the blob.
    pub(crate) fn is_compaction(&self) -> bool {
        self.request_kind == Some(CodexRequestKind::Compaction)
    }

    fn matches_session(&self, value: &str) -> bool {
        let value = value.trim();
        !value.is_empty()
            && (self.session_id.as_deref() == Some(value)
                || self.thread_id.as_deref() == Some(value))
    }

    fn matches_window(&self, value: &str) -> bool {
        let value = value.trim();
        if value.is_empty() {
            return false;
        }
        if self.window_id.as_deref() == Some(value) {
            return true;
        }
        value
            .split_once(':')
            .is_some_and(|(thread, _)| self.matches_session(thread))
    }
}

// ---------------------------------------------------------------------------
// Outbound identity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundTurnSource {
    /// Same inbound turn as the WebSocket candidate snapshot.
    Snapshot,
    /// The thread's open turn, already bound to this inbound turn (per-turn
    /// freeze, or opened for it by a concurrent peer).
    Frozen,
    /// Freshly minted by this request against the account's turn budget.
    Minted,
    /// The thread's open turn was opened for a different inbound turn and
    /// this request continues it: the budget was spent, the request chains
    /// onto a previous response, or its own earlier turn was superseded.
    Steered,
    /// No turn on this request (`request_kind=memory`).
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutboundCodexRuntimeIdentity {
    pub(crate) session_id: String,
    pub(crate) thread_id: String,
    /// `{thread_id}:{window_number}`; memory requests always project `:0`
    /// like `memories/write/src/runtime.rs` does.
    pub(crate) window_id: String,
    /// Compactions upstream has seen on this synthetic thread.
    pub(crate) window_number: u64,
    /// UUIDv7 minted when the current window started; `None` only when the
    /// store could not answer (the blob key is then removed, never leaked).
    pub(crate) context_window_id: Option<String>,
    pub(crate) turn_id: Option<String>,
    pub(crate) turn_source: OutboundTurnSource,
    pub(crate) inbound_root: String,
    pub(crate) inbound_turn_key: Option<String>,
}

impl OutboundCodexRuntimeIdentity {
    /// `x-codex-turn-state` was issued by upstream for the outbound turn of an
    /// earlier request of the same inbound turn. Forward it only when this
    /// request's outbound turn is that same turn; never attach it to a freshly
    /// minted turn or to another inbound turn's open turn.
    pub(crate) fn forwards_turn_state(&self) -> bool {
        matches!(
            self.turn_source,
            OutboundTurnSource::Snapshot | OutboundTurnSource::Frozen | OutboundTurnSource::None
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexRuntimeIdentityResolution {
    Rewrite(OutboundCodexRuntimeIdentity),
    Passthrough,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RootFreeze {
    session_id: String,
    thread_id: String,
    /// Legacy (`{thread}:0`); the live window is read from `window_key`.
    window_id: String,
    day_id: u64,
}

impl RootFreeze {
    fn parse(raw: &str) -> Option<Self> {
        serde_json::from_str::<Self>(raw)
            .ok()
            .filter(|freeze| !freeze.thread_id.is_empty() && !freeze.session_id.is_empty())
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The open turn of one synthetic thread. A real thread runs one turn at a
/// time, so every request on the thread presents this turn until a new inbound
/// turn mints the next one (budget permitting); turn ids on one thread then
/// never interleave or come back. `inbound_turn` names the inbound turn the
/// outbound turn was opened for (`inbound_turn_ref`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct OpenTurn {
    turn_id: String,
    inbound_turn: String,
}

impl OpenTurn {
    fn parse(raw: &str) -> Option<Self> {
        serde_json::from_str::<Self>(raw)
            .ok()
            .filter(|open| !open.turn_id.is_empty())
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// `{root_hash}:{turn_hash}`: one inbound turn of one inbound root, the same
/// pair the per-turn freeze key is built from. Raw ids are never stored.
fn inbound_turn_ref(root_hash: &str, turn_hash: &str) -> String {
    format!("{root_hash}:{turn_hash}")
}

/// Per-thread context window, mirroring codex-rs `AutoCompactWindowIds`: the
/// real client starts at window 0 with a fresh `Uuid::now_v7()`, and every
/// compaction (local or remote, both upstream-visible per thread) increments
/// the number and mints a new context window id. Upstream therefore expects
/// `window_number == compactions seen on this thread`; a thread that compacts
/// but never advances would be a shape no real client produces.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct ThreadWindow {
    #[serde(default)]
    number: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_window_id: Option<String>,
}

impl ThreadWindow {
    fn parse(raw: &str) -> Option<Self> {
        serde_json::from_str::<Self>(raw).ok()
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// `x-codex-window-id` / blob `window_id` projection of a thread window.
fn window_id_projection(thread_id: &str, window_number: u64, memory: bool) -> String {
    if memory {
        format!("{thread_id}:0")
    } else {
        format!("{thread_id}:{window_number}")
    }
}

// ---------------------------------------------------------------------------
// Store: thin wrapper over the shared runtime state kv API
// ---------------------------------------------------------------------------

pub(crate) struct CodexRuntimeIdentityStore<'a> {
    runtime: &'a RuntimeState,
    #[cfg(test)]
    unavailable: bool,
}

impl<'a> CodexRuntimeIdentityStore<'a> {
    pub(crate) fn new(runtime: &'a RuntimeState) -> Self {
        Self {
            runtime,
            #[cfg(test)]
            unavailable: false,
        }
    }

    #[cfg(test)]
    fn unavailable(runtime: &'a RuntimeState) -> Self {
        Self {
            runtime,
            unavailable: true,
        }
    }

    fn check(&self) -> Result<(), String> {
        #[cfg(test)]
        if self.unavailable {
            return Err("runtime state unavailable (test)".to_string());
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<String>, String> {
        self.check()?;
        self.runtime
            .kv_get(key)
            .await
            .map_err(|error| error.to_string())
    }

    async fn set(&self, key: &str, value: &str, ttl: Duration) -> Result<(), String> {
        self.check()?;
        self.runtime
            .kv_set(key, value, Some(ttl))
            .await
            .map_err(|error| error.to_string())
    }

    async fn delete(&self, key: &str) -> Result<(), String> {
        self.check()?;
        self.runtime
            .kv_delete(key)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    async fn set_if_absent(&self, key: &str, value: &str, ttl: Duration) -> Result<bool, String> {
        self.check()?;
        self.runtime
            .kv_set_if_absent(key, value, ttl)
            .await
            .map_err(|error| error.to_string())
    }

    async fn expire_if_value(
        &self,
        key: &str,
        expected: &str,
        ttl: Duration,
    ) -> Result<bool, String> {
        self.check()?;
        self.runtime
            .kv_expire_if_value(key, expected, ttl)
            .await
            .map_err(|error| error.to_string())
    }

    async fn set_if_value(
        &self,
        key: &str,
        expected: &str,
        value: &str,
        ttl: Duration,
    ) -> Result<bool, String> {
        self.check()?;
        self.runtime
            .kv_set_if_value(key, expected, value, ttl)
            .await
            .map_err(|error| error.to_string())
    }

    /// Atomic sliding-window admission: drops members scored before
    /// `window_start`, then adds `member` at `score` only while fewer than
    /// `max_count` members remain in the window. This is what makes the
    /// thread and turn ceilings hard under concurrency.
    async fn admit(
        &self,
        key: &str,
        member: &str,
        score: f64,
        window_start: f64,
        max_count: u64,
        ttl: Duration,
    ) -> Result<bool, String> {
        self.check()?;
        let max_count = usize::try_from(max_count).unwrap_or(usize::MAX);
        self.runtime
            .score_add_if_count_below(
                key,
                member,
                score,
                window_start,
                window_start,
                max_count,
                ttl,
            )
            .await
            .map_err(|error| error.to_string())
    }

    /// Least recently active member (lowest score; ties by member).
    async fn roster_oldest(&self, key: &str) -> Result<Option<String>, String> {
        self.check()?;
        Ok(self
            .runtime
            .score_range_by_min(key, 0.0)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .next())
    }

    /// Inserts or re-scores `member` and slides the roster's TTL.
    async fn roster_touch(
        &self,
        key: &str,
        member: &str,
        score: f64,
        ttl: Duration,
    ) -> Result<(), String> {
        self.check()?;
        self.runtime
            .score_set(key, member, score)
            .await
            .map_err(|error| error.to_string())?;
        self.runtime
            .key_expire(key, ttl)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn roster_remove(&self, key: &str, member: &str) -> Result<bool, String> {
        self.check()?;
        self.runtime
            .score_remove(key, member)
            .await
            .map_err(|error| error.to_string())
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Resolves the outbound identity for one request.
///
/// * `ws_snapshot`: the WebSocket candidate's authoritative in-process
///   snapshot (same inbound root). Session/thread/window come from it without
///   touching the store; only a new inbound turn needs the store.
/// * Any store error → `Passthrough` (or, with a snapshot, the snapshot) and
///   `codex_rid_store_unavailable`. Nothing is minted in-process.
pub(crate) async fn resolve_outbound_codex_runtime_identity(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    inbound: &InboundCodexRuntimeIdentity,
    ws_snapshot: Option<&OutboundCodexRuntimeIdentity>,
    now: SystemTime,
) -> CodexRuntimeIdentityResolution {
    let Some(root) = inbound.root() else {
        return CodexRuntimeIdentityResolution::Passthrough;
    };
    let turn_key = if inbound.is_memory() {
        None
    } else {
        inbound.turn_key()
    };
    let snapshot = ws_snapshot.filter(|snapshot| snapshot.inbound_root == root);
    let request = ResolveRequest {
        root,
        turn_key: turn_key.as_deref(),
        chained: inbound.previous_response_id_present,
        memory: inbound.is_memory(),
        compaction: inbound.is_compaction(),
    };
    match resolve_inner(store, scope, &request, snapshot, now).await {
        Ok(outbound) => CodexRuntimeIdentityResolution::Rewrite(outbound),
        Err(error) => {
            warn!(
                event_name = "codex_rid_store_unavailable",
                log_type = "event",
                provider_id = %scope.provider_id,
                selection_fp = %scope.selection_fp,
                inbound_root_hash = %hash16(root),
                has_ws_snapshot = snapshot.is_some(),
                error = %error,
                "codex runtime identity store unavailable; passing inbound identity through"
            );
            match snapshot {
                // A bound WebSocket already presented the snapshot identity to
                // upstream at handshake; keep the connection coherent instead
                // of leaking the inbound tree mid-connection.
                Some(snapshot) => {
                    let mut outbound = snapshot.clone();
                    outbound.inbound_turn_key = turn_key;
                    outbound.window_id = window_id_projection(
                        &snapshot.thread_id,
                        snapshot.window_number,
                        inbound.is_memory(),
                    );
                    if inbound.is_memory() {
                        outbound.turn_id = None;
                        outbound.turn_source = OutboundTurnSource::None;
                    } else {
                        outbound.turn_source = OutboundTurnSource::Snapshot;
                    }
                    CodexRuntimeIdentityResolution::Rewrite(outbound)
                }
                None => CodexRuntimeIdentityResolution::Passthrough,
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ResolveRequest<'a> {
    root: &'a str,
    turn_key: Option<&'a str>,
    /// `previous_response_id` present: continue the root's last turn on a
    /// per-turn freeze miss.
    chained: bool,
    /// `request_kind=memory`: no turn, window projected as `:0`.
    memory: bool,
    /// `request_kind=compaction`: this request carries the current window and
    /// the thread's window advances for every request after it.
    compaction: bool,
}

async fn resolve_inner(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    request: &ResolveRequest<'_>,
    snapshot: Option<&OutboundCodexRuntimeIdentity>,
    now: SystemTime,
) -> Result<OutboundCodexRuntimeIdentity, String> {
    let ResolveRequest {
        root,
        turn_key,
        chained,
        memory,
        compaction,
    } = *request;
    let day_id = scope.day_id(now);
    let ttl = scope.ttl();
    let root_hash = hash16(root);

    if let Some(snapshot) = snapshot {
        let (turn_id, turn_source) = match turn_key {
            None => (None, OutboundTurnSource::None),
            Some(key) => {
                // The snapshot's own turn is authoritative for the same inbound
                // turn only while it is still the thread's open turn; a folded
                // sibling may have moved the thread on in the meantime.
                let snapshot_turn = snapshot
                    .turn_id
                    .as_deref()
                    .filter(|_| snapshot.inbound_turn_key.as_deref() == Some(key));
                resolve_turn(
                    store,
                    scope,
                    &root_hash,
                    &snapshot.thread_id,
                    key,
                    TurnContext {
                        day_id,
                        ttl,
                        now,
                        chained,
                        snapshot_turn,
                    },
                )
                .await?
            }
        };
        let window =
            resolve_window(store, scope, &snapshot.thread_id, compaction, ttl, now).await?;
        return Ok(OutboundCodexRuntimeIdentity {
            session_id: snapshot.session_id.clone(),
            thread_id: snapshot.thread_id.clone(),
            window_id: window_id_projection(&snapshot.thread_id, window.number, memory),
            window_number: window.number,
            context_window_id: window.context_window_id,
            turn_id,
            turn_source,
            inbound_root: root.to_string(),
            inbound_turn_key: turn_key.map(str::to_string),
        });
    }

    let freeze_key = scope.freeze_key(&root_hash);
    let mut frozen = match store.get(&freeze_key).await? {
        Some(raw) => {
            let freeze = RootFreeze::parse(&raw);
            if freeze.is_some() {
                // Sliding TTL: an active session never changes thread mid-flight.
                let _ = store.expire_if_value(&freeze_key, &raw, ttl).await?;
            }
            freeze
        }
        None => None,
    };
    // A turn minted together with a fresh thread by this request.
    let mut minted_turn: Option<String> = None;
    if frozen.is_none() {
        if chained {
            debug!(
                event_name = "codex_rid_chain_freeze_miss",
                log_type = "event",
                provider_id = %scope.provider_id,
                selection_fp = %scope.selection_fp,
                inbound_root_hash = %root_hash,
                "chained request has no root freeze; assigning a thread"
            );
        }
        let inbound_turn = turn_key.map(|key| inbound_turn_ref(&root_hash, &hash16(key)));
        let mut assigned =
            assign_thread(store, scope, day_id, ttl, now, inbound_turn.as_deref()).await?;
        let fresh = RootFreeze {
            session_id: assigned.thread_id.clone(),
            thread_id: assigned.thread_id.clone(),
            window_id: format!("{}:0", assigned.thread_id),
            day_id,
        };
        frozen = if store
            .set_if_absent(&freeze_key, &fresh.to_json(), ttl)
            .await?
        {
            minted_turn = assigned.reserved_turn.take();
            Some(fresh)
        } else {
            match store.get(&freeze_key).await? {
                Some(existing) => match RootFreeze::parse(&existing) {
                    Some(winner) => {
                        if assigned.fresh && winner.thread_id != assigned.thread_id {
                            // Lost the freeze race for this root: the thread
                            // (and turn) minted here never served a request.
                            withdraw_thread(store, scope, &assigned).await?;
                        }
                        Some(winner)
                    }
                    None => {
                        minted_turn = assigned.reserved_turn.take();
                        Some(fresh)
                    }
                },
                None => {
                    minted_turn = assigned.reserved_turn.take();
                    Some(fresh)
                }
            }
        };
    }
    let freeze = frozen.expect("root freeze resolved above");

    let (turn_id, turn_source) = match turn_key {
        None => (None, OutboundTurnSource::None),
        Some(key) => match minted_turn {
            Some(turn_id) => {
                // Opened on the fresh thread by `assign_thread`; only the
                // per-turn freeze is still missing.
                store
                    .set(
                        &scope.turn_freeze_key(&root_hash, &hash16(key)),
                        &turn_id,
                        ttl,
                    )
                    .await?;
                (Some(turn_id), OutboundTurnSource::Minted)
            }
            None => {
                resolve_turn(
                    store,
                    scope,
                    &root_hash,
                    &freeze.thread_id,
                    key,
                    TurnContext {
                        day_id,
                        ttl,
                        now,
                        chained,
                        snapshot_turn: None,
                    },
                )
                .await?
            }
        },
    };

    let window = resolve_window(store, scope, &freeze.thread_id, compaction, ttl, now).await?;
    Ok(OutboundCodexRuntimeIdentity {
        session_id: freeze.session_id,
        window_id: window_id_projection(&freeze.thread_id, window.number, memory),
        window_number: window.number,
        context_window_id: window.context_window_id,
        thread_id: freeze.thread_id,
        turn_id,
        turn_source,
        inbound_root: root.to_string(),
        inbound_turn_key: turn_key.map(str::to_string),
    })
}

/// Outcome of `assign_thread` for an inbound root without a freeze.
#[derive(Debug)]
struct AssignedThread {
    thread_id: String,
    /// Turn minted against the account's budget and opened on the fresh
    /// thread; `None` on reuse or for a memory request.
    reserved_turn: Option<String>,
    /// Minted by this request (a lost freeze race withdraws it again).
    fresh: bool,
}

/// Picks the outbound thread for an inbound root that has no freeze yet.
///
/// Threads grow with activity, the way a person opens conversations: while
/// fewer than the account's thread ceiling are active in the trailing 24h a
/// fresh UUIDv7 is admitted to the roster (atomically, so concurrent new roots
/// never mint past the ceiling); once the ceiling is reached the least
/// recently active thread is reused. A new thread always starts with a new
/// turn, so the turn budget is reserved first: a spent budget, or a request
/// without a turn (memory), folds the root onto an existing thread instead of
/// opening one that could only steer. An empty roster always mints.
async fn assign_thread(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    day_id: u64,
    ttl: Duration,
    now: SystemTime,
    inbound_turn: Option<&str>,
) -> Result<AssignedThread, String> {
    let roster_key = scope.thread_roster_key();
    let bound = scope.thread_bound(day_id);
    let score = unix_secs(now) as f64;
    let window_start = score - DAY_WINDOW_SECS as f64;

    let mut reserved_turn = match inbound_turn {
        Some(_) => reserve_turn(store, scope, day_id, ttl, now).await?,
        None => None,
    };
    if reserved_turn.is_some() {
        let minted = uuid_v7_at(unix_millis(now));
        if store
            .admit(&roster_key, &minted, score, window_start, bound, ttl)
            .await?
        {
            open_reserved_turn(
                store,
                scope,
                &minted,
                reserved_turn.as_deref(),
                inbound_turn,
                ttl,
            )
            .await?;
            return Ok(AssignedThread {
                thread_id: minted,
                reserved_turn,
                fresh: true,
            });
        }
    }
    match store.roster_oldest(&roster_key).await? {
        Some(thread_id) => {
            let turn_budget_spent = inbound_turn.is_some() && reserved_turn.is_none();
            if let Some(turn_id) = reserved_turn.take() {
                release_turn(store, scope, &turn_id).await?;
            }
            store
                .roster_touch(&roster_key, &thread_id, score, ttl)
                .await?;
            debug!(
                event_name = "codex_rid_thread_reused",
                log_type = "event",
                provider_id = %scope.provider_id,
                selection_fp = %scope.selection_fp,
                day_id,
                bound,
                turn_budget_spent,
                "thread ceiling reached or turn budget spent; reusing least recently active thread"
            );
            Ok(AssignedThread {
                thread_id,
                reserved_turn: None,
                fresh: false,
            })
        }
        None => {
            // Nothing to reuse (first activity, or the roster expired): a
            // thread must exist, and with it a turn.
            let minted = uuid_v7_at(unix_millis(now));
            store.roster_touch(&roster_key, &minted, score, ttl).await?;
            if inbound_turn.is_some() && reserved_turn.is_none() {
                reserved_turn = Some(force_turn(store, scope, day_id, ttl, now).await?);
            }
            open_reserved_turn(
                store,
                scope,
                &minted,
                reserved_turn.as_deref(),
                inbound_turn,
                ttl,
            )
            .await?;
            Ok(AssignedThread {
                thread_id: minted,
                reserved_turn,
                fresh: true,
            })
        }
    }
}

/// Records the turn reserved for a fresh thread as that thread's open turn.
async fn open_reserved_turn(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    outbound_thread_id: &str,
    reserved_turn: Option<&str>,
    inbound_turn: Option<&str>,
    ttl: Duration,
) -> Result<(), String> {
    if let (Some(turn_id), Some(inbound_turn)) = (reserved_turn, inbound_turn) {
        let open = OpenTurn {
            turn_id: turn_id.to_string(),
            inbound_turn: inbound_turn.to_string(),
        };
        let _ = store
            .set_if_absent(
                &scope.open_turn_key(outbound_thread_id),
                &open.to_json(),
                ttl,
            )
            .await?;
    }
    Ok(())
}

/// Undoes `assign_thread` for a fresh thread that lost its root's freeze race.
async fn withdraw_thread(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    assigned: &AssignedThread,
) -> Result<(), String> {
    let _ = store
        .roster_remove(&scope.thread_roster_key(), &assigned.thread_id)
        .await?;
    store
        .delete(&scope.open_turn_key(&assigned.thread_id))
        .await?;
    if let Some(turn_id) = assigned.reserved_turn.as_deref() {
        release_turn(store, scope, turn_id).await?;
    }
    Ok(())
}

/// Mints a UUIDv7 turn against the account's budget: admitted to the ledger
/// only while fewer than `turn_bound(day_id)` turns were minted in the
/// trailing 24h. `None` when the budget is spent.
async fn reserve_turn(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    day_id: u64,
    ttl: Duration,
    now: SystemTime,
) -> Result<Option<String>, String> {
    let turn_id = uuid_v7_at(unix_millis(now));
    let score = unix_secs(now) as f64;
    let window_start = score - DAY_WINDOW_SECS as f64;
    let admitted = store
        .admit(
            &scope.turn_ledger_key(),
            &turn_id,
            score,
            window_start,
            scope.turn_bound(day_id),
            ttl,
        )
        .await?;
    Ok(admitted.then_some(turn_id))
}

/// Mints a turn past the budget because a thread has nothing to continue
/// (no open turn). Rare: it takes a thread that went idle long enough for its
/// open turn to expire, or an evicted key. Ledgered so it still counts.
async fn force_turn(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    day_id: u64,
    ttl: Duration,
    now: SystemTime,
) -> Result<String, String> {
    let turn_id = uuid_v7_at(unix_millis(now));
    store
        .roster_touch(
            &scope.turn_ledger_key(),
            &turn_id,
            unix_secs(now) as f64,
            ttl,
        )
        .await?;
    debug!(
        event_name = "codex_rid_turn_budget_exceeded",
        log_type = "event",
        provider_id = %scope.provider_id,
        selection_fp = %scope.selection_fp,
        day_id,
        bound = scope.turn_bound(day_id),
        "turn budget spent but the thread has no open turn; minting past the budget"
    );
    Ok(turn_id)
}

/// Returns an unused reservation to the budget.
async fn release_turn(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    turn_id: &str,
) -> Result<(), String> {
    let _ = store
        .roster_remove(&scope.turn_ledger_key(), turn_id)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct TurnContext<'a> {
    day_id: u64,
    ttl: Duration,
    now: SystemTime,
    /// `previous_response_id` present: the request continues the thread's
    /// open turn rather than minting.
    chained: bool,
    /// The WebSocket snapshot's outbound turn for this very inbound turn.
    snapshot_turn: Option<&'a str>,
}

async fn resolve_window(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    outbound_thread_id: &str,
    compaction: bool,
    ttl: Duration,
    now: SystemTime,
) -> Result<ThreadWindow, String> {
    let key = scope.window_key(outbound_thread_id);
    let mut raw = store.get(&key).await?;
    let mut window = raw
        .as_deref()
        .and_then(ThreadWindow::parse)
        .unwrap_or_default();
    if window.context_window_id.is_none() {
        let mut minted = window.clone();
        minted.context_window_id = Some(uuid_v7_at(unix_millis(now)));
        let minted_raw = minted.to_json();
        let won = match raw.as_deref() {
            None => store.set_if_absent(&key, &minted_raw, ttl).await?,
            Some(current) => store.set_if_value(&key, current, &minted_raw, ttl).await?,
        };
        if won {
            window = minted;
            raw = Some(minted_raw);
        } else {
            match store.get(&key).await? {
                Some(existing) => {
                    window = ThreadWindow::parse(&existing).unwrap_or(minted);
                    raw = Some(existing);
                }
                None => {
                    window = minted;
                    raw = Some(minted_raw);
                }
            }
        }
    } else if let Some(current) = raw.as_deref() {
        let _ = store.expire_if_value(&key, current, ttl).await?;
    }
    if compaction {
        if let Some(current) = raw.as_deref() {
            let advanced = ThreadWindow {
                number: window.number.saturating_add(1),
                context_window_id: None,
            };
            let _ = store
                .set_if_value(&key, current, &advanced.to_json(), ttl)
                .await?;
        }
    }
    Ok(window)
}

/// Resolves the outbound turn of one request on `outbound_thread_id`.
///
/// The thread has at most one open turn (`open_turn_key`). This inbound turn
/// keeps it when the open turn was opened for it, or when its per-turn freeze
/// already names it (`Frozen`). A different inbound turn mints the next turn
/// against the account budget and moves the thread's open turn to it
/// (`Minted`), unless the budget is spent, the request is chained, or this
/// inbound turn was already mapped to an earlier outbound turn that the thread
/// has since left behind: then it continues the open turn (`Steered`), so a
/// thread never revisits a turn id. The per-turn freeze always follows the
/// turn actually sent, and every resolved turn is activity on the thread.
async fn resolve_turn(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    root_hash: &str,
    outbound_thread_id: &str,
    inbound_turn_key: &str,
    context: TurnContext<'_>,
) -> Result<(Option<String>, OutboundTurnSource), String> {
    let TurnContext {
        day_id,
        ttl,
        now,
        chained,
        snapshot_turn,
    } = context;
    let turn_hash = hash16(inbound_turn_key);
    let turn_freeze_key = scope.turn_freeze_key(root_hash, &turn_hash);
    let inbound_turn = inbound_turn_ref(root_hash, &turn_hash);
    let open_key = scope.open_turn_key(outbound_thread_id);

    let open_raw = store.get(&open_key).await?;
    let open = open_raw.as_deref().and_then(OpenTurn::parse);
    let previous = store.get(&turn_freeze_key).await?;

    let opened_for_this_turn = |open: &OpenTurn| {
        open.inbound_turn == inbound_turn || previous.as_deref() == Some(open.turn_id.as_str())
    };
    let (turn_id, turn_source) = match (snapshot_turn, open) {
        // WebSocket step of the turn the candidate was bound for. A bound
        // connection is authoritative for its own in-flight turn on its own
        // wire, whatever folded onto the thread since; honor it. Slide the
        // open key when it still names this turn, reopen it when it expired,
        // but never move a newer open turn back to it (that would make the
        // thread revisit an id it already left behind for other traffic).
        (Some(snapshot_turn), open) => {
            match open.as_ref().map(|open| open.turn_id.as_str()) {
                Some(open_turn) if open_turn == snapshot_turn => {
                    if let Some(current) = open_raw.as_deref() {
                        let _ = store.expire_if_value(&open_key, current, ttl).await?;
                    }
                }
                Some(_) => {}
                None => {
                    let reopened = OpenTurn {
                        turn_id: snapshot_turn.to_string(),
                        inbound_turn: inbound_turn.clone(),
                    };
                    let _ = store
                        .set_if_absent(&open_key, &reopened.to_json(), ttl)
                        .await?;
                }
            }
            (snapshot_turn.to_string(), OutboundTurnSource::Snapshot)
        }
        (_, Some(open)) if opened_for_this_turn(&open) => {
            if let Some(current) = open_raw.as_deref() {
                let _ = store.expire_if_value(&open_key, current, ttl).await?;
            }
            (open.turn_id, OutboundTurnSource::Frozen)
        }
        (_, Some(open)) => {
            // Another inbound turn opened the thread's turn. Mint the next one
            // only for an inbound turn the thread has never seen, unchained,
            // and within budget; otherwise continue the open turn.
            let mintable = previous.is_none() && !chained;
            let reserved = if mintable {
                reserve_turn(store, scope, day_id, ttl, now).await?
            } else {
                None
            };
            match reserved {
                Some(minted) => {
                    let next = OpenTurn {
                        turn_id: minted.clone(),
                        inbound_turn: inbound_turn.clone(),
                    };
                    let current = open_raw.as_deref().unwrap_or_default();
                    if store
                        .set_if_value(&open_key, current, &next.to_json(), ttl)
                        .await?
                    {
                        (minted, OutboundTurnSource::Minted)
                    } else {
                        // A concurrent request moved the thread on first.
                        release_turn(store, scope, &minted).await?;
                        let current = store
                            .get(&open_key)
                            .await?
                            .as_deref()
                            .and_then(OpenTurn::parse)
                            .map(|open| open.turn_id)
                            .unwrap_or(open.turn_id);
                        (current, OutboundTurnSource::Steered)
                    }
                }
                None => {
                    if mintable {
                        debug!(
                            event_name = "codex_rid_turn_steered",
                            log_type = "event",
                            provider_id = %scope.provider_id,
                            selection_fp = %scope.selection_fp,
                            day_id,
                            bound = scope.turn_bound(day_id),
                            "turn budget spent; continuing the thread's open turn"
                        );
                    }
                    // Steered traffic is activity on the open turn too.
                    if let Some(current) = open_raw.as_deref() {
                        let _ = store.expire_if_value(&open_key, current, ttl).await?;
                    }
                    (open.turn_id, OutboundTurnSource::Steered)
                }
            }
        }
        (_, None) => {
            // The thread has no open turn: reopen this inbound turn's own turn
            // when it has one, otherwise mint (past the budget if it must).
            let (turn_id, source) = match previous.as_deref() {
                Some(previous) => (previous.to_string(), OutboundTurnSource::Frozen),
                None => match reserve_turn(store, scope, day_id, ttl, now).await? {
                    Some(minted) => (minted, OutboundTurnSource::Minted),
                    None => (
                        force_turn(store, scope, day_id, ttl, now).await?,
                        OutboundTurnSource::Minted,
                    ),
                },
            };
            let opened = OpenTurn {
                turn_id: turn_id.clone(),
                inbound_turn: inbound_turn.clone(),
            };
            if store
                .set_if_absent(&open_key, &opened.to_json(), ttl)
                .await?
            {
                (turn_id, source)
            } else {
                // A concurrent request opened the thread first: follow it.
                if source == OutboundTurnSource::Minted {
                    release_turn(store, scope, &turn_id).await?;
                }
                match store
                    .get(&open_key)
                    .await?
                    .as_deref()
                    .and_then(OpenTurn::parse)
                {
                    Some(open) if open.turn_id == turn_id => (turn_id, source),
                    Some(open) => (open.turn_id, OutboundTurnSource::Steered),
                    None => (turn_id, source),
                }
            }
        }
    };

    // The per-turn freeze names the outbound turn this inbound turn last used,
    // which is what decides whether the client's turn-state is for it.
    if previous.as_deref() == Some(turn_id.as_str()) {
        let _ = store
            .expire_if_value(&turn_freeze_key, &turn_id, ttl)
            .await?;
    } else {
        store.set(&turn_freeze_key, &turn_id, ttl).await?;
    }
    // Every request with a turn is activity: the thread moves to the back of
    // the reuse order and stays counted in the trailing 24h.
    store
        .roster_touch(
            &scope.thread_roster_key(),
            outbound_thread_id,
            unix_secs(now) as f64,
            ttl,
        )
        .await?;
    Ok((Some(turn_id), turn_source))
}

// ---------------------------------------------------------------------------
// Rewrite
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexRuntimeIdentitySurface {
    /// HTTP `/responses`: headers + body. The only surface that materializes
    /// a synthetic identity and inserts the official headers when missing.
    HttpResponses,
    /// HTTP `/responses/compact`: headers + body, rewrite only.
    HttpCompact,
    /// Search / chat / family / image on a Codex provider: headers only.
    Headers,
    /// WebSocket `response.create` step: body only (handshake headers are
    /// composed by the WS runtime from the outbound snapshot).
    WsStepBody,
}

/// Rewrites every outbound projection so dash headers, flat `client_metadata`
/// and the `x-codex-turn-metadata` blob agree on the synthetic identity.
///
/// The request must look like it came from the client the outbound user-agent
/// names (a current codex-rs build), not from the inbound client. On
/// `request_kind` turn / compaction / prewarm the turn-metadata blob is rebuilt
/// in the official field order with every key such a client always sends
/// (older inbound clients omit `window_number`, `context_window_id`,
/// `agent_name`, `sandbox_mode` and the review flags), `sandbox` follows the
/// outbound OS, and the headers an official HTTP `/responses` client sends
/// unconditionally are inserted. Blobs without a request kind keep their key
/// set (official `request_kind=None` blobs carry no installation / window
/// keys). Inbound-tree keys (parent / fork / subagent) are removed,
/// `agent_name` / `thread_source` / `root_turn_id` are normalized to the root
/// user-thread shape, and any key outside the per-surface whitelist is removed
/// and reported.
///
/// `user_agent` is the outbound user-agent for the surface whose headers are
/// not at hand (the WebSocket step body); elsewhere it is read from `headers`.
///
/// A synthetic inbound (no official identity) has nothing to rewrite: the
/// HTTP `/responses` surface materializes the full official shape instead.
pub(crate) fn apply_outbound_codex_runtime_identity(
    headers: &mut BTreeMap<String, String>,
    body: Option<&mut Value>,
    inbound: &InboundCodexRuntimeIdentity,
    outbound: &OutboundCodexRuntimeIdentity,
    surface: CodexRuntimeIdentitySurface,
    user_agent: Option<&str>,
) {
    let header_user_agent = header_entry(headers, USER_AGENT_HEADER).map(|(_, value)| value);
    let client = OutboundClient::from_user_agent(user_agent.or(header_user_agent.as_deref()));
    if inbound.is_synthetic() {
        if surface == CodexRuntimeIdentitySurface::HttpResponses {
            materialize_http_responses(headers, body, outbound, client);
        }
        return;
    }
    if surface != CodexRuntimeIdentitySurface::WsStepBody {
        rewrite_headers(headers, inbound, outbound, surface, client);
    }
    if surface != CodexRuntimeIdentitySurface::Headers {
        if let Some(body) = body {
            rewrite_body(body, inbound, outbound, client);
        }
    }
}

fn rewrite_headers(
    headers: &mut BTreeMap<String, String>,
    inbound: &InboundCodexRuntimeIdentity,
    outbound: &OutboundCodexRuntimeIdentity,
    surface: CodexRuntimeIdentitySurface,
    client: OutboundClient,
) {
    // Official HTTP `/responses` and `/responses/compact` requests both carry
    // session-id, thread-id and x-codex-window-id unconditionally (codex-api
    // `build_session_headers`, `endpoint/responses.rs`; core client.rs
    // `compact_conversation_history` extends the same `build_session_headers`
    // + `compatibility_headers`). A relay that strips them in front of a real
    // client (prod v0.7.104: the dominant downstream) leaves a shape no client
    // produces, so on those surfaces missing ones are inserted; header-only
    // surfaces (search / chat / image) only rewrite what is present.
    //
    // The dash `session-id` header is also what the ChatGPT Codex backend pins
    // prompt-cache routing on: prod v0.7.104 rewrote requests whose only
    // affinity header was the legacy `session_id` short header Aether used to
    // derive, stripped it, and cache misses went from 2% to 44% (`docs/
    // architecture/codex-pool-runtime-identity-synthesis-plan-2026-09-03.md`
    // §18.14.7).
    let insert_missing = matches!(
        surface,
        CodexRuntimeIdentitySurface::HttpResponses | CodexRuntimeIdentitySurface::HttpCompact
    );
    project_header(
        headers,
        SESSION_ID_HEADER,
        |value| inbound.matches_session(value),
        &outbound.session_id,
        insert_missing,
    );
    project_header(
        headers,
        THREAD_ID_HEADER,
        |value| inbound.matches_session(value),
        &outbound.thread_id,
        insert_missing,
    );
    project_header(
        headers,
        X_CODEX_WINDOW_ID,
        |value| inbound.matches_window(value),
        &outbound.window_id,
        insert_missing,
    );
    // x-client-request-id = thread_id is set by the `/responses` stream
    // endpoint (codex-api endpoint/responses.rs) and the WS handshake (core
    // client.rs) only. The compact endpoint (codex-api endpoint/compact.rs)
    // passes `extra_headers` through and `compact_conversation_history` never
    // adds it, so a compact request carrying one (Aether's filler writes the
    // request id) is a shape no client produces: drop it there. Anywhere else
    // a foreign value is the Aether request id or a relay trace id, a
    // per-request random value no real client produces (prod v0.7.104:
    // 188/188 requests), so it is always rewritten to the outbound thread.
    if surface == CodexRuntimeIdentitySurface::HttpCompact {
        remove_header(headers, X_CLIENT_REQUEST_ID);
    } else {
        project_header(
            headers,
            X_CLIENT_REQUEST_ID,
            |_| true,
            &outbound.thread_id,
            surface == CodexRuntimeIdentitySurface::HttpResponses,
        );
    }
    if let Some((name, raw)) = header_entry(headers, X_CODEX_TURN_METADATA) {
        if let Some(rewritten) = rewrite_turn_metadata_blob_string(&raw, outbound, client) {
            headers.insert(name, rewritten);
        }
    }
    if !outbound.forwards_turn_state() {
        remove_header(headers, X_CODEX_TURN_STATE);
    }
    retain_known_headers(headers);
    // Official HTTP clients never send the short headers. Aether derives them
    // as a 16-hex fingerprint of the real session, and relays in front of real
    // clients forward `session_id` = the real thread id (prod v0.7.104), so an
    // explicit inbound value leaks just the same: strip both regardless of origin.
    for short in SHORT_HEADERS {
        remove_header(headers, short);
    }
}

fn rewrite_body(
    body: &mut Value,
    inbound: &InboundCodexRuntimeIdentity,
    outbound: &OutboundCodexRuntimeIdentity,
    client: OutboundClient,
) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    let rewrite_prompt_cache_key = if !inbound.prompt_cache_key_present {
        // Official default is `prompt_cache_key = session_id`; Aether fillers
        // would otherwise leave a UUIDv5 here.
        true
    } else {
        non_empty_str(object.get("prompt_cache_key")).is_some_and(|value| {
            inbound.matches_session(value) || value.starts_with(GUARDIAN_PROMPT_CACHE_PREFIX)
        })
    };
    if rewrite_prompt_cache_key {
        object.insert(
            "prompt_cache_key".to_string(),
            Value::String(outbound.session_id.clone()),
        );
    }

    let Some(client_metadata) = object
        .get_mut("client_metadata")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    set_if_present(client_metadata, "session_id", &outbound.session_id);
    set_if_present(client_metadata, "thread_id", &outbound.thread_id);
    set_if_present(client_metadata, X_CODEX_WINDOW_ID, &outbound.window_id);
    match outbound.turn_id.as_deref() {
        Some(turn_id) => {
            set_if_present(client_metadata, "turn_id", turn_id);
            // A root turn is its own root.
            set_if_present(client_metadata, "root_turn_id", turn_id);
        }
        None => {
            client_metadata.remove("turn_id");
            client_metadata.remove("root_turn_id");
        }
    }
    for key in FLAT_LEAK_KEYS {
        client_metadata.remove(*key);
    }
    if let Some(blob) = client_metadata.get_mut(X_CODEX_TURN_METADATA) {
        rewrite_codex_turn_metadata_value(blob, outbound, client);
    }
    if !outbound.forwards_turn_state() {
        client_metadata.remove(X_CODEX_TURN_STATE);
    }
    retain_known_keys(client_metadata, "client_metadata", flat_key_known);
}

/// Rewrites a serialized `x-codex-turn-metadata` blob (header or
/// `client_metadata` string) for the client the outbound `user_agent` names.
/// Returns `None` when it is not a JSON object.
pub(crate) fn rewrite_codex_turn_metadata_string(
    raw: &str,
    outbound: &OutboundCodexRuntimeIdentity,
    user_agent: Option<&str>,
) -> Option<String> {
    rewrite_turn_metadata_blob_string(raw, outbound, OutboundClient::from_user_agent(user_agent))
}

fn rewrite_turn_metadata_blob_string(
    raw: &str,
    outbound: &OutboundCodexRuntimeIdentity,
    client: OutboundClient,
) -> Option<String> {
    let mut parsed = serde_json::from_str::<Value>(raw).ok()?;
    let object = parsed.as_object_mut()?;
    rewrite_codex_turn_metadata_object(object, outbound, client);
    // Embedded in an HTTP header: keep every byte ASCII.
    serialize_ascii_json(&parsed)
}

fn rewrite_codex_turn_metadata_value(
    blob: &mut Value,
    outbound: &OutboundCodexRuntimeIdentity,
    client: OutboundClient,
) {
    match blob {
        Value::String(raw) => {
            if let Some(rewritten) = rewrite_turn_metadata_blob_string(raw, outbound, client) {
                *raw = rewritten;
            }
        }
        Value::Object(object) => rewrite_codex_turn_metadata_object(object, outbound, client),
        _ => {}
    }
}

fn rewrite_codex_turn_metadata_object(
    object: &mut Map<String, Value>,
    outbound: &OutboundCodexRuntimeIdentity,
    client: OutboundClient,
) {
    let os = client.os;
    // Inbound-tree markers, unknown keys and app-server-only keys under a
    // terminal user-agent go first, so the rebuilt blob below only ever
    // copies whitelisted pass-through keys.
    for key in BLOB_LEAK_KEYS {
        object.remove(*key);
    }
    if !client.app_server {
        for key in BLOB_APP_SERVER_KEYS {
            object.remove(*key);
        }
    }
    retain_known_keys(object, "turn_metadata", blob_key_known);
    // `sandbox` names the platform sandbox of the client's OS (codex-rs
    // `core/src/sandbox_tags.rs`); a Windows tag under a macOS user-agent is a
    // shape no client produces, so it follows the outbound OS wherever it is.
    if let Some(sandbox) = non_empty_str(object.get("sandbox")) {
        let projected = os.project_sandbox(Some(sandbox));
        object.insert("sandbox".to_string(), Value::String(projected.to_string()));
    }
    match non_empty_str(object.get("request_kind")).map(CodexRequestKind::parse) {
        Some(CodexRequestKind::Memory) => {
            // Official memory blobs carry no installation/session/thread/turn/
            // root_turn/window/window_number/context_window_id.
            for key in BLOB_IDENTITY_KEYS {
                object.remove(*key);
            }
            object.remove("root_turn_id");
        }
        Some(
            kind @ (CodexRequestKind::Turn
            | CodexRequestKind::Compaction
            | CodexRequestKind::Prewarm),
        ) => {
            // `has_request_identity` in codex-rs `turn_metadata_payload()`:
            // the blob carries the whole identity, and a current client
            // always sends every key `request_identity_blob` fills in.
            *object = request_identity_blob(object, outbound, kind, os);
        }
        None | Some(CodexRequestKind::Other) => {
            // `request_kind=None` blobs (`has_turn_identity` only) carry no
            // installation / window keys: rewrite what is present, add nothing.
            set_if_present(object, "session_id", &outbound.session_id);
            set_if_present(object, "thread_id", &outbound.thread_id);
            set_if_present(object, "window_id", &outbound.window_id);
            if object.contains_key("window_number") {
                object.insert(
                    "window_number".to_string(),
                    Value::from(outbound.window_number),
                );
            }
            if object.contains_key("context_window_id") {
                match outbound.context_window_id.as_deref() {
                    Some(context_window_id) => {
                        object.insert(
                            "context_window_id".to_string(),
                            Value::String(context_window_id.to_string()),
                        );
                    }
                    None => {
                        object.remove("context_window_id");
                    }
                }
            }
            // Folded subagent / feature threads present as the root user
            // thread, whose root turn is the turn itself.
            set_if_present(object, "agent_name", ROOT_AGENT_NAME);
            set_if_present(object, "thread_source", USER_THREAD_SOURCE);
            match outbound.turn_id.as_deref() {
                Some(turn_id) => {
                    set_if_present(object, "turn_id", turn_id);
                    set_if_present(object, "root_turn_id", turn_id);
                }
                None => {
                    object.remove("turn_id");
                    object.remove("root_turn_id");
                }
            }
        }
    }
}

/// The blob a current client sends on a request kind that carries the request
/// identity (turn / compaction / prewarm), rebuilt from the whitelisted
/// `source` in `CodexTurnMetadataPayload` field order: identity keys follow
/// the synthetic thread and its window; keys such a client always sends are
/// filled with the default-configuration value when the inbound client omitted
/// them (codex-tui ≤ 0.150 has no `window_number` / `context_window_id` /
/// `agent_name`, ≤ 0.147 no `sandbox_mode` or review flags); optional keys
/// (`root_turn_id`, `thread_source`, `turn_trigger`, `workspaces`, …) are kept
/// only when present (the app-server-only ones were removed by the caller
/// under a terminal user-agent). `installation_id` stays as the profile pass
/// left it.
fn request_identity_blob(
    source: &Map<String, Value>,
    outbound: &OutboundCodexRuntimeIdentity,
    kind: CodexRequestKind,
    os: OutboundClientOs,
) -> Map<String, Value> {
    fn copy_key(source: &Map<String, Value>, blob: &mut Map<String, Value>, key: &str) {
        if let Some(value) = source.get(key) {
            blob.insert(key.to_string(), value.clone());
        }
    }
    fn set_str(blob: &mut Map<String, Value>, key: &str, value: &str) {
        blob.insert(key.to_string(), Value::String(value.to_string()));
    }

    let mut blob = Map::new();
    copy_key(source, &mut blob, "installation_id");
    set_str(&mut blob, "session_id", &outbound.session_id);
    set_str(&mut blob, "thread_id", &outbound.thread_id);
    set_str(&mut blob, "agent_name", ROOT_AGENT_NAME);
    if let Some(turn_id) = outbound.turn_id.as_deref() {
        set_str(&mut blob, "turn_id", turn_id);
    }
    set_str(&mut blob, "window_id", &outbound.window_id);
    blob.insert(
        "window_number".to_string(),
        Value::from(outbound.window_number),
    );
    if let Some(context_window_id) = outbound.context_window_id.as_deref() {
        set_str(&mut blob, "context_window_id", context_window_id);
    }
    copy_key(source, &mut blob, "request_kind");
    // A root turn is its own root; both keys are optional for a current client.
    if source.contains_key("root_turn_id") {
        if let Some(turn_id) = outbound.turn_id.as_deref() {
            set_str(&mut blob, "root_turn_id", turn_id);
        }
    }
    if source.contains_key("thread_source") {
        set_str(&mut blob, "thread_source", USER_THREAD_SOURCE);
    }
    copy_key(source, &mut blob, "turn_trigger");
    let sandbox = os.project_sandbox(non_empty_str(source.get("sandbox")));
    set_str(&mut blob, "sandbox", sandbox);
    match source.get("sandbox_mode") {
        Some(sandbox_mode) => {
            blob.insert("sandbox_mode".to_string(), sandbox_mode.clone());
        }
        None => set_str(&mut blob, "sandbox_mode", default_sandbox_mode(sandbox)),
    }
    for flag in [
        "auto_review_enabled",
        "node_repl_auto_review_required",
        "node_repl_disabled",
    ] {
        blob.insert(
            flag.to_string(),
            source.get(flag).cloned().unwrap_or(Value::Bool(false)),
        );
    }
    copy_key(source, &mut blob, "workspaces");
    copy_key(source, &mut blob, "tool_namespaces_info");
    // The client's own `turn_started_at_unix_ms` is discarded: it is the
    // inbound real turn's start, and many real turns fold onto one outbound
    // turn, so copying it through leaves a single synthetic turn carrying a
    // different start on every request. A real client stamps the turn start at
    // `Session::start_task`, and the outbound turn UUIDv7 was minted at
    // exactly that moment, so the outbound turn id is the only source. The
    // startup prewarm is sent before any task, so it carries no stamp.
    if kind != CodexRequestKind::Prewarm {
        if let Some(unix_ms) = outbound.turn_id.as_deref().and_then(uuid_v7_unix_millis) {
            blob.insert("turn_started_at_unix_ms".to_string(), Value::from(unix_ms));
        }
    }
    copy_key(source, &mut blob, "history_ingest_requested");
    copy_key(source, &mut blob, "compaction");
    // Flattened `extra` entries (the Desktop `workspace_kind`) and any other
    // pass-through key serialize after the struct fields. The turn stamp is
    // excluded: it is derived above or deliberately absent, never copied.
    for (key, value) in source {
        if !blob.contains_key(key)
            && key != "turn_started_at_unix_ms"
            && BLOB_PASS_KEYS.contains(&key.as_str())
        {
            blob.insert(key.clone(), value.clone());
        }
    }
    blob
}

// ---------------------------------------------------------------------------
// Synthetic identity (requests without official identity)
// ---------------------------------------------------------------------------

const SEP: &[u8] = b"\0";

/// `hex(SHA256(domain, name, value, …)[0..16])` over the downstream identity
/// headers present on the request; raw values are never stored or logged.
fn downstream_fingerprint(headers: &HeaderMap) -> String {
    let mut parts: Vec<&[u8]> = vec![DOWNSTREAM_FP_DOMAIN];
    for name in DOWNSTREAM_IDENTITY_HEADERS {
        if let Some(value) = header_str(headers, name) {
            parts.extend([SEP, name.as_bytes(), SEP, value.as_bytes()]);
        }
    }
    hex_lower(&sha256(&parts)[..16])
}

/// `(index, text)` of every real user prompt in `input`, in order.
fn real_user_prompts(input: &Value) -> Vec<(usize, String)> {
    match input {
        Value::String(text) => prompt_text(text)
            .map(|text| vec![(0, text)])
            .unwrap_or_default(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let item = item.as_object()?;
                let is_message = match item.get("type") {
                    None => true,
                    Some(kind) => kind.as_str() == Some("message"),
                };
                if !is_message || non_empty_str(item.get("role")) != Some("user") {
                    return None;
                }
                prompt_text(&message_text(item.get("content")?)?).map(|text| (index, text))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Text of a Responses input message: a string or its text parts joined.
pub(crate) fn message_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let texts = parts
                .iter()
                .filter(|part| {
                    matches!(
                        non_empty_str(part.get("type")),
                        Some("input_text") | Some("text") | None
                    )
                })
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        _ => None,
    }
}

/// A real prompt: non-empty, not a wrapper the client injects around
/// instructions / environment / skills (`<tag>` first), not a compaction
/// summary.
pub(crate) fn prompt_text(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.starts_with(COMPACT_SUMMARY_PREFIX) || starts_with_wrapper_tag(text)
    {
        return None;
    }
    Some(text.to_string())
}

pub(crate) fn starts_with_wrapper_tag(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('<') else {
        return false;
    };
    let Some(end) = rest.find('>') else {
        return false;
    };
    let tag = &rest[..end];
    !tag.is_empty()
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

/// Blob of a synthetic request, in `CodexTurnMetadataPayload` field order
/// (codex-rs `core/src/responses_metadata.rs`; serde skips `None`). Fields a
/// real client only sets in some environments (`workspaces`, `turn_trigger`,
/// `tool_namespaces_info`) are omitted.
#[derive(Serialize)]
struct SyntheticTurnMetadata<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    installation_id: Option<&'a str>,
    session_id: &'a str,
    thread_id: &'a str,
    agent_name: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<&'a str>,
    window_id: &'a str,
    window_number: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_window_id: Option<&'a str>,
    request_kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    root_turn_id: Option<&'a str>,
    thread_source: &'static str,
    sandbox: &'static str,
    sandbox_mode: &'static str,
    auto_review_enabled: bool,
    node_repl_auto_review_required: bool,
    node_repl_disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_started_at_unix_ms: Option<u64>,
}

/// The client the outbound user-agent names, as far as the blob shape depends
/// on it: its OS (the `sandbox` tag) and whether it is an app-server host
/// (Desktop / VS Code / `codex_app`), the only clients that set
/// `BLOB_APP_SERVER_KEYS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutboundClient {
    os: OutboundClientOs,
    app_server: bool,
}

impl OutboundClient {
    fn from_user_agent(user_agent: Option<&str>) -> Self {
        let os = OutboundClientOs::from_user_agent(user_agent);
        // An unknown / absent user-agent is treated as a terminal client: the
        // pool's header profiles are terminal builds unless they say otherwise,
        // and dropping an optional key is never a shape no client produces.
        let app_server = user_agent.is_some_and(|agent| {
            let agent = agent.trim_start();
            !agent.is_empty()
                && !TERMINAL_ORIGINATOR_PREFIXES
                    .iter()
                    .any(|prefix| agent.starts_with(prefix))
        });
        Self { os, app_server }
    }
}

/// Operating system the outbound user-agent names — the only client-side fact
/// the `sandbox` tag depends on (codex-rs `core/src/sandbox_tags.rs`,
/// `sandboxing/src/manager.rs`). A default-configured client reports seatbelt
/// on macOS, the elevated Windows sandbox on Windows and seccomp elsewhere;
/// `none` when its policy needs no platform sandbox (`danger-full-access`) and
/// `external` with an external sandbox, on every OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutboundClientOs {
    MacOs,
    Windows,
    Other,
}

impl OutboundClientOs {
    fn from_user_agent(user_agent: Option<&str>) -> Self {
        match user_agent {
            Some(agent) if agent.contains("Mac OS") => Self::MacOs,
            Some(agent) if agent.contains("Windows") => Self::Windows,
            _ => Self::Other,
        }
    }

    /// Platform sandbox a default-configured client reports on this OS.
    fn platform_sandbox(self) -> &'static str {
        match self {
            Self::MacOs => "seatbelt",
            Self::Windows => "windows_elevated",
            Self::Other => "seccomp",
        }
    }

    /// The `sandbox` tag an inbound client's tag translates to on this OS: the
    /// OS-independent tags stay, a Windows client keeps its choice between the
    /// restricted-token and the elevated sandbox, and every other platform tag
    /// (or an unknown one) becomes this OS's platform sandbox.
    fn project_sandbox(self, inbound: Option<&str>) -> &'static str {
        match (self, inbound) {
            (_, Some("none")) => "none",
            (_, Some("external")) => "external",
            (Self::Windows, Some("windows_sandbox")) => "windows_sandbox",
            _ => self.platform_sandbox(),
        }
    }
}

/// `sandbox_mode` a client pairs with `sandbox` when the inbound blob has none:
/// `none` only arises from a policy without platform sandbox (prod, every
/// 0.150+ `none` row: `danger-full-access`); a platform sandbox runs the
/// default `workspace-write` policy.
fn default_sandbox_mode(sandbox: &str) -> &'static str {
    if sandbox == "none" {
        "danger-full-access"
    } else {
        "workspace-write"
    }
}

fn synthetic_turn_metadata_json(
    outbound: &OutboundCodexRuntimeIdentity,
    installation_id: Option<&str>,
    client: OutboundClient,
) -> Option<String> {
    let sandbox = client.os.platform_sandbox();
    let sandbox_mode = default_sandbox_mode(sandbox);
    let payload = SyntheticTurnMetadata {
        installation_id,
        session_id: &outbound.session_id,
        thread_id: &outbound.thread_id,
        agent_name: ROOT_AGENT_NAME,
        turn_id: outbound.turn_id.as_deref(),
        window_id: &outbound.window_id,
        window_number: outbound.window_number,
        context_window_id: outbound.context_window_id.as_deref(),
        request_kind: REQUEST_KIND_TURN,
        root_turn_id: outbound.turn_id.as_deref(),
        thread_source: USER_THREAD_SOURCE,
        sandbox,
        sandbox_mode,
        auto_review_enabled: false,
        node_repl_auto_review_required: false,
        node_repl_disabled: false,
        // A real client stamps the turn start; the outbound turn UUIDv7 was
        // minted at exactly that moment.
        turn_started_at_unix_ms: outbound.turn_id.as_deref().and_then(uuid_v7_unix_millis),
    };
    serialize_ascii_json(&serde_json::to_value(payload).ok()?)
}

/// Materializes the full official HTTP `/responses` shape on a request that
/// carried no identity: dash headers, `x-client-request-id = thread`,
/// `x-codex-window-id`, the turn-metadata header, `prompt_cache_key =
/// session`, and a flat `client_metadata` in `client_metadata()` key order.
/// The account profile pass already set user-agent / originator /
/// installation id; Aether's short headers are removed like on every
/// synthetic request.
fn materialize_http_responses(
    headers: &mut BTreeMap<String, String>,
    body: Option<&mut Value>,
    outbound: &OutboundCodexRuntimeIdentity,
    client: OutboundClient,
) {
    let installation_id = header_entry(headers, X_CODEX_INSTALLATION_ID).map(|(_, value)| value);
    let blob = synthetic_turn_metadata_json(outbound, installation_id.as_deref(), client);

    set_header(headers, SESSION_ID_HEADER, &outbound.session_id);
    set_header(headers, THREAD_ID_HEADER, &outbound.thread_id);
    set_header(headers, X_CODEX_WINDOW_ID, &outbound.window_id);
    set_header(headers, X_CLIENT_REQUEST_ID, &outbound.thread_id);
    match blob.as_deref() {
        Some(blob) => set_header(headers, X_CODEX_TURN_METADATA, blob),
        None => remove_header(headers, X_CODEX_TURN_METADATA),
    }
    remove_header(headers, X_CODEX_TURN_STATE);
    retain_known_headers(headers);
    for short in SHORT_HEADERS {
        remove_header(headers, short);
    }

    let Some(object) = body.and_then(Value::as_object_mut) else {
        return;
    };
    object.insert(
        "prompt_cache_key".to_string(),
        Value::String(outbound.session_id.clone()),
    );
    let previous = match object.remove("client_metadata") {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let mut client_metadata = Map::new();
    if let Some(installation_id) = installation_id {
        client_metadata.insert(
            X_CODEX_INSTALLATION_ID.to_string(),
            Value::String(installation_id),
        );
    }
    client_metadata.insert(
        "session_id".to_string(),
        Value::String(outbound.session_id.clone()),
    );
    client_metadata.insert(
        "thread_id".to_string(),
        Value::String(outbound.thread_id.clone()),
    );
    client_metadata.insert(
        X_CODEX_WINDOW_ID.to_string(),
        Value::String(outbound.window_id.clone()),
    );
    if let Some(turn_id) = outbound.turn_id.as_deref() {
        client_metadata.insert("turn_id".to_string(), Value::String(turn_id.to_string()));
        client_metadata.insert(
            "root_turn_id".to_string(),
            Value::String(turn_id.to_string()),
        );
    }
    if let Some(blob) = blob {
        client_metadata.insert(X_CODEX_TURN_METADATA.to_string(), Value::String(blob));
    }
    // Non-identity keys the request already had (guardian receipts, Aether
    // step control) stay; identity keys were rebuilt above.
    for (key, value) in previous {
        if !FLAT_IDENTITY_KEYS.contains(&key.as_str()) && !client_metadata.contains_key(&key) {
            client_metadata.insert(key, value);
        }
    }
    for key in FLAT_LEAK_KEYS {
        client_metadata.remove(*key);
    }
    retain_known_keys(&mut client_metadata, "client_metadata", flat_key_known);
    object.insert(
        "client_metadata".to_string(),
        Value::Object(client_metadata),
    );
}

// ---------------------------------------------------------------------------
// Whitelist
// ---------------------------------------------------------------------------

fn blob_key_known(key: &str) -> bool {
    [
        BLOB_IDENTITY_KEYS,
        BLOB_NORMALIZED_KEYS,
        BLOB_LEAK_KEYS,
        BLOB_PASS_KEYS,
    ]
    .iter()
    .any(|set| set.contains(&key))
}

fn flat_key_known(key: &str) -> bool {
    [FLAT_IDENTITY_KEYS, FLAT_LEAK_KEYS, FLAT_PASS_KEYS]
        .iter()
        .any(|set| set.contains(&key))
        || FLAT_CONTROL_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

/// Removes every key the whitelist does not know and reports it (name and
/// JSON type only; values are never logged).
fn retain_known_keys(
    object: &mut Map<String, Value>,
    surface: &'static str,
    known: fn(&str) -> bool,
) {
    let unknown = object
        .keys()
        .filter(|key| !known(key))
        .cloned()
        .collect::<Vec<_>>();
    for key in unknown {
        let value_type = object.get(&key).map(json_type_name).unwrap_or("null");
        report_unknown_metadata_key(surface, &key, value_type);
        object.remove(&key);
    }
}

/// Request headers under the Codex identity prefixes: forward the known real
/// client set, drop tree markers silently, drop and report everything else.
fn retain_known_headers(headers: &mut BTreeMap<String, String>) {
    let names = headers.keys().cloned().collect::<Vec<_>>();
    for name in names {
        let lower = name.trim().to_ascii_lowercase();
        if !HEADER_IDENTITY_PREFIXES
            .iter()
            .any(|prefix| lower.starts_with(prefix))
            || HEADER_PASS_KEYS.contains(&lower.as_str())
        {
            continue;
        }
        if !HEADER_STRIP_KEYS.contains(&lower.as_str()) {
            report_unknown_metadata_key("header", &lower, "string");
        }
        headers.remove(&name);
    }
}

/// `warn` on the first sighting of a (surface, key) per process, `debug`
/// afterwards, so a new client field is visible without flooding the log.
fn report_unknown_metadata_key(surface: &'static str, key: &str, value_type: &'static str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first_sighting = SEEN
        .get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(format!("{surface}\0{key}")))
        .unwrap_or(true);
    if first_sighting {
        warn!(
            event_name = "codex_rid_unknown_metadata_key",
            log_type = "event",
            surface,
            key,
            value_type,
            "unknown codex client metadata key removed from the synthetic outbound request; add it to the whitelist if benign"
        );
    } else {
        debug!(
            event_name = "codex_rid_unknown_metadata_key",
            log_type = "event",
            surface,
            key,
            value_type,
            "unknown codex client metadata key removed"
        );
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Hand-rolled UUIDv7 (RFC 9562): 48-bit unix millisecond timestamp, version
/// nibble 7, RFC variant, 72 random bits. The workspace `uuid` crate is locked
/// without the `v7` feature and CI builds with `--locked`.
///
/// The two missing random bits are deliberate: real codex mints thread/session
/// UUIDs with `Uuid::now_v7()`, whose `ContextV7` monotonic counter reseeds to a
/// 42-bit value and is then re-encoded by shifting the counter *around* the
/// 2-bit variant field (see uuid-1.x `Builder::from_unix_timestamp_millis` /
/// `v7.rs`). That shift leaves a permanent 2-bit zero gap at `bytes[7]` bits 2-3
/// (string index 17, which is therefore always one of `0,1,2,3`). Empirically,
/// 100% of `now_v7()` outputs clear those bits while a fully-random `bytes[7]`
/// sets them ~75% of the time — a single synthetic UUID would otherwise be a
/// structurally impossible shape and give the whole account away. We reproduce
/// the gap so the outbound IDs are indistinguishable from genuine codex output.
pub(crate) fn uuid_v7_at(unix_ms: u64) -> String {
    uuid_v7_from_parts(unix_ms, Uuid::new_v4().as_bytes())
}

/// Lays out a UUIDv7 from a 48-bit millisecond timestamp and 16 bytes of
/// entropy (only bytes 6..16 are used). Single source of the byte shape so every
/// synthetic ID, random or derived, carries the same version/variant/gap bits.
pub(crate) fn uuid_v7_from_parts(unix_ms: u64, random: &[u8; 16]) -> String {
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&unix_ms.to_be_bytes()[2..8]);
    bytes[6] = 0x70 | (random[6] & 0x0F);
    // Clear bits 2-3: the ContextV7 counter gap that real `now_v7()` always leaves.
    bytes[7] = random[7] & 0xF3;
    bytes[8] = 0x80 | (random[8] & 0x3F);
    bytes[9..].copy_from_slice(&random[9..]);
    Uuid::from_bytes(bytes).hyphenated().to_string()
}

pub(crate) fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn u64_prefix(digest: &[u8; 32]) -> u64 {
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(prefix)
}

/// Maps a digest onto `[ceil(configured / 2), configured]`. `configured` is
/// the operator-facing ceiling (`1..=64` threads / `1..=512` turns); a value
/// of `1` always yields `1`.
fn jittered_bound(configured: u32, digest: &[u8; 32]) -> u64 {
    let max = u64::from(configured.max(1));
    let min = max.div_ceil(2);
    min + u64_prefix(digest) % (max - min + 1)
}

/// `hex(SHA256(value)[0..16])`; inbound IDs never enter keys or logs verbatim.
pub(crate) fn hash16(value: &str) -> String {
    hex_lower(&sha256(&[value.as_bytes()])[..16])
}

fn unix_secs(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

pub(crate) fn unix_millis(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn header_entry(headers: &BTreeMap<String, String>, target: &str) -> Option<(String, String)> {
    headers
        .iter()
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(target))
        .map(|(name, value)| (name.clone(), value.clone()))
}

fn remove_header(headers: &mut BTreeMap<String, String>, target: &str) {
    let names = headers
        .keys()
        .filter(|name| name.trim().eq_ignore_ascii_case(target))
        .cloned()
        .collect::<Vec<_>>();
    for name in names {
        headers.remove(&name);
    }
}

/// Sets `target` (case-insensitively replacing any existing spelling).
fn set_header(headers: &mut BTreeMap<String, String>, target: &str, value: &str) {
    remove_header(headers, target);
    headers.insert(target.to_string(), value.to_string());
}

/// Rewrites `target` when present and `predicate` accepts its current value;
/// inserts it when absent only if `insert_missing`.
fn project_header(
    headers: &mut BTreeMap<String, String>,
    target: &str,
    predicate: impl Fn(&str) -> bool,
    value: &str,
    insert_missing: bool,
) {
    match header_entry(headers, target) {
        Some((name, current)) if predicate(&current) => {
            headers.insert(name, value.to_string());
        }
        Some(_) => {}
        None if insert_missing => {
            headers.insert(target.to_string(), value.to_string());
        }
        None => {}
    }
}

/// Unix milliseconds encoded in the first 48 bits of a UUIDv7 string.
pub(crate) fn uuid_v7_unix_millis(id: &str) -> Option<u64> {
    let uuid = Uuid::parse_str(id).ok()?;
    if uuid.get_version_num() != 7 {
        return None;
    }
    Some(
        uuid.as_bytes()[..6]
            .iter()
            .fold(0u64, |millis, byte| (millis << 8) | u64::from(*byte)),
    )
}

fn set_if_present(object: &mut Map<String, Value>, key: &str, value: &str) {
    if object.contains_key(key) {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_runtime_state::MemoryRuntimeStateConfig;
    use http::HeaderValue;
    use serde_json::json;
    use std::collections::HashSet;

    const PROVIDER: &str = "prov-1";
    const SELECTION: &str = "codex:account:acc-a";
    const FIXTURE_CONTEXT_WINDOW: &str = "0199094e-7b2b-7000-8000-0123456789ab";
    const MAC_UA: &str =
        "codex-tui/0.153.4 (Mac OS 26.2.0; arm64) Orca/1.4.185 (codex-tui; 0.153.4)";
    const WINDOWS_UA: &str = "codex_cli_rs/0.153.4 (Windows 10.0.26100; x86_64) WindowsTerminal";
    const LINUX_UA: &str = "codex_cli_rs/0.153.4 (Ubuntu 24.4.0; x86_64) unknown";
    // codex_vscode is the app-server (Desktop/IDE) originator; it is not a
    // terminal prefix, so its blobs keep the app-server-only keys.
    const DESKTOP_UA: &str =
        "codex_vscode/0.153.4 (Mac OS 26.2.0; arm64) vscode/1.104.0 (codex_vscode; 0.153.4)";

    fn v7_millis(id: &str) -> u64 {
        u64::from_str_radix(&id.replace('-', "")[..12], 16).unwrap()
    }

    fn memory_state() -> RuntimeState {
        RuntimeState::memory(MemoryRuntimeStateConfig::default())
    }

    fn config(threads: u32, turns: u32) -> CodexRuntimeIdentityConfig {
        CodexRuntimeIdentityConfig {
            expected_threads_per_day: threads,
            expected_turns_per_day: turns,
        }
    }

    fn scope(threads: u32, turns: u32) -> CodexRuntimeIdentityScope {
        CodexRuntimeIdentityScope::new(PROVIDER, SELECTION, config(threads, turns))
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn inbound(session: &str, thread: &str, turn: Option<&str>) -> InboundCodexRuntimeIdentity {
        InboundCodexRuntimeIdentity {
            session_id: Some(session.to_string()),
            thread_id: Some(thread.to_string()),
            turn_id: turn.map(str::to_string),
            window_id: Some(format!("{thread}:0")),
            request_kind: Some(CodexRequestKind::Turn),
            prompt_cache_key_present: true,
            previous_response_id_present: false,
            synthetic: None,
        }
    }

    fn rewrite(resolution: CodexRuntimeIdentityResolution) -> OutboundCodexRuntimeIdentity {
        match resolution {
            CodexRuntimeIdentityResolution::Rewrite(outbound) => outbound,
            CodexRuntimeIdentityResolution::Passthrough => panic!("expected rewrite"),
        }
    }

    fn is_uuid_v7(value: &str) -> bool {
        let Ok(uuid) = Uuid::parse_str(value) else {
            return false;
        };
        uuid.get_version_num() == 7 && uuid.get_variant() == uuid::Variant::RFC4122
    }

    fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    fn btree(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    fn outbound_fixture(
        turn: Option<&str>,
        source: OutboundTurnSource,
    ) -> OutboundCodexRuntimeIdentity {
        OutboundCodexRuntimeIdentity {
            session_id: "out-thread".to_string(),
            thread_id: "out-thread".to_string(),
            window_id: "out-thread:0".to_string(),
            window_number: 0,
            context_window_id: Some(FIXTURE_CONTEXT_WINDOW.to_string()),
            turn_id: turn.map(str::to_string),
            turn_source: source,
            inbound_root: "in-session".to_string(),
            inbound_turn_key: Some("in-turn".to_string()),
        }
    }

    // ----- config -----------------------------------------------------------

    #[test]
    fn config_disabled_or_missing_is_off() {
        assert_eq!(codex_runtime_identity_rewrite_enabled(None, PROVIDER), None);
        assert_eq!(
            codex_runtime_identity_rewrite_enabled(Some(&json!({})), PROVIDER),
            None
        );
        assert_eq!(
            codex_runtime_identity_rewrite_enabled(
                Some(&json!({ "codex_runtime_identity": null })),
                PROVIDER
            ),
            None
        );
        assert_eq!(
            codex_runtime_identity_rewrite_enabled(
                Some(&json!({ "codex_runtime_identity": { "enabled": false } })),
                PROVIDER
            ),
            None
        );
        assert_eq!(
            codex_runtime_identity_rewrite_enabled(
                Some(&json!({ "codex_runtime_identity": {
                    "enabled": false,
                    "expected_threads_per_day": 6,
                    "expected_turns_per_day": 48
                } })),
                PROVIDER
            ),
            None
        );
    }

    #[test]
    fn config_enabled_requires_bounds_and_has_no_defaults() {
        assert_eq!(
            codex_runtime_identity_rewrite_enabled(
                Some(&json!({ "codex_runtime_identity": {
                    "enabled": true,
                    "expected_threads_per_day": 6,
                    "expected_turns_per_day": 48
                } })),
                PROVIDER
            ),
            Some(config(6, 48))
        );
        // Enabled without bounds: invalid → off, never a hidden default.
        assert_eq!(
            codex_runtime_identity_rewrite_enabled(
                Some(&json!({ "codex_runtime_identity": { "enabled": true } })),
                PROVIDER
            ),
            None
        );
        assert!(
            validate_codex_runtime_identity_config(&json!({ "enabled": true }))
                .unwrap_err()
                .contains("expected_threads_per_day")
        );
    }

    #[test]
    fn config_validation_rejects_bad_shapes() {
        let err = validate_codex_runtime_identity_config(&json!("x")).unwrap_err();
        assert!(err.contains("JSON 对象"), "{err}");
        let err = validate_codex_runtime_identity_config(&json!({ "enabled": "yes" })).unwrap_err();
        assert!(err.contains("enabled"), "{err}");
        for threads in [0, 65] {
            let err = validate_codex_runtime_identity_config(&json!({
                "enabled": true,
                "expected_threads_per_day": threads,
                "expected_turns_per_day": 10
            }))
            .unwrap_err();
            assert!(err.contains("expected_threads_per_day"), "{err}");
        }
        for turns in [0, 513] {
            let err = validate_codex_runtime_identity_config(&json!({
                "enabled": true,
                "expected_threads_per_day": 4,
                "expected_turns_per_day": turns
            }))
            .unwrap_err();
            assert!(err.contains("expected_turns_per_day"), "{err}");
        }
        let err = validate_codex_runtime_identity_config(&json!({
            "enabled": true,
            "expected_threads_per_day": 1.5,
            "expected_turns_per_day": 10
        }))
        .unwrap_err();
        assert!(err.contains("expected_threads_per_day"), "{err}");
        // Disabled but with an out-of-range value is still rejected on write.
        let err = validate_codex_runtime_identity_config(&json!({
            "enabled": false,
            "expected_threads_per_day": 999
        }))
        .unwrap_err();
        assert!(err.contains("expected_threads_per_day"), "{err}");
        // Valid disabled shapes.
        validate_codex_runtime_identity_config(&json!({})).unwrap();
        validate_codex_runtime_identity_config(&json!({ "enabled": false })).unwrap();
        validate_codex_runtime_identity_config(&json!({
            "enabled": true,
            "expected_threads_per_day": 1,
            "expected_turns_per_day": 512
        }))
        .unwrap();
    }

    // ----- uuid v7 ----------------------------------------------------------

    #[test]
    fn uuid_v7_has_version_variant_and_timestamp() {
        let ms = 1_756_857_600_123u64;
        let value = uuid_v7_at(ms);
        assert!(is_uuid_v7(&value), "{value}");
        let uuid = Uuid::parse_str(&value).unwrap();
        let mut ts = [0u8; 8];
        ts[2..].copy_from_slice(&uuid.as_bytes()[..6]);
        assert_eq!(u64::from_be_bytes(ts), ms);
        assert_ne!(uuid_v7_at(ms), uuid_v7_at(ms));
    }

    #[test]
    fn uuid_v7_reproduces_context_v7_counter_gap() {
        // Real codex uses `Uuid::now_v7()`, whose `ContextV7` re-encoding leaves a
        // permanent 2-bit zero gap at byte 7 bits 2-3 (string index 17 in 0..=3).
        // A single UUID with those bits set would be a structurally impossible
        // codex shape, so every mint must clear them.
        for _ in 0..4096 {
            let uuid = Uuid::parse_str(&uuid_v7_at(1_756_857_600_123u64)).unwrap();
            let byte7 = uuid.as_bytes()[7];
            assert_eq!(byte7 & 0x0C, 0, "byte7 counter gap not cleared: {uuid}");
        }
    }

    // ----- scope ------------------------------------------------------------

    #[test]
    fn scope_fingerprint_is_stable_and_jitter_bounded() {
        let a = scope(4, 8);
        let b = scope(4, 8);
        assert_eq!(a.selection_fp, b.selection_fp);
        assert_eq!(a.selection_fp.len(), 32);
        assert!(a.account_jitter_secs < DAY_WINDOW_SECS);
        let other = CodexRuntimeIdentityScope::new(PROVIDER, "codex:account:acc-b", config(4, 8));
        assert_ne!(a.selection_fp, other.selection_fp);
        // Keys never contain the raw selection key and share the account tree
        // prefix. The roster and ledger are account-level (no day/thread in
        // the name); the open-turn key is per outbound thread.
        for key in [
            a.thread_roster_key(),
            a.turn_ledger_key(),
            a.open_turn_key("out-thread"),
        ] {
            assert!(!key.contains(SELECTION), "{key} leaked the selection key");
            assert!(
                key.starts_with("ap:prov-1:codex_rid:"),
                "{key} not in the account tree"
            );
        }
        assert!(a.thread_roster_key().ends_with(":threads"));
        assert!(a.turn_ledger_key().ends_with(":turns"));
        assert!(a.open_turn_key("out-thread").ends_with(":open:out-thread"));
    }

    #[test]
    fn daily_bounds_jitter_per_account_and_day_within_band() {
        let a = scope(32, 256);
        let b = CodexRuntimeIdentityScope::new(PROVIDER, "codex:account:acc-b", config(32, 256));
        let mut a_thread_bounds = HashSet::new();
        let mut a_turn_bounds = HashSet::new();
        let mut differs_from_b = false;
        for day in 20_000..20_060u64 {
            let ta = a.thread_bound(day);
            let ua = a.turn_bound(day);
            assert!((16..=32).contains(&ta), "day {day} thread bound {ta}");
            assert!((128..=256).contains(&ua), "day {day} turn bound {ua}");
            assert_eq!(ta, a.thread_bound(day), "bound must be deterministic");
            assert_eq!(ua, a.turn_bound(day), "bound must be deterministic");
            a_thread_bounds.insert(ta);
            a_turn_bounds.insert(ua);
            differs_from_b |= ta != b.thread_bound(day) || ua != b.turn_bound(day);
        }
        assert!(
            a_thread_bounds.len() > 1,
            "thread bound never varied across days"
        );
        assert!(
            a_turn_bounds.len() > 1,
            "turn bound never varied across days"
        );
        assert!(
            differs_from_b,
            "two accounts never disagreed on a daily bound"
        );
        // The turn ceiling is account-level, not per thread: `turn_bound`
        // takes only the day.
        // A ceiling of 1 stays 1, on both axes.
        let one = scope(1, 1);
        for day in 0..10u64 {
            assert_eq!(one.thread_bound(day), 1);
            assert_eq!(one.turn_bound(day), 1);
        }
        // Band edges for small ceilings: N=2 → {1,2}, N=3 → {2,3}.
        assert!((1..=2).contains(&scope(2, 2).thread_bound(7)));
        assert!((2..=3).contains(&scope(3, 3).thread_bound(7)));
        assert!((1..=2).contains(&scope(2, 2).turn_bound(7)));
        assert!((2..=3).contains(&scope(3, 3).turn_bound(7)));
    }

    #[test]
    fn scope_ttl_is_constant_window_plus_grace() {
        // Every state key lives for the trailing-24h ceiling window plus a 12h
        // grace, independent of `now`, and is slid on each activity. This
        // keeps a roster or ledger member present for as long as it can still
        // count against a 24h window.
        let s = scope(4, 8);
        assert_eq!(s.ttl().as_secs(), DAY_WINDOW_SECS + TTL_GRACE_SECS);
        assert_eq!(scope(1, 1).ttl(), s.ttl());
    }

    // ----- inbound extraction ----------------------------------------------

    #[test]
    fn inbound_precedence_blob_then_flat_then_header() {
        let body = json!({
            "prompt_cache_key": "in-session",
            "previous_response_id": "resp_1",
            "client_metadata": {
                "session_id": "flat-session",
                "thread_id": "flat-thread",
                "turn_id": "flat-turn",
                "x-codex-window-id": "flat-thread:0",
                "x-codex-turn-metadata": json!({
                    "session_id": "blob-session",
                    "thread_id": "blob-thread",
                    "request_kind": "turn"
                }).to_string()
            }
        });
        let headers = header_map(&[
            ("session-id", "hdr-session"),
            ("thread-id", "hdr-thread"),
            ("x-codex-window-id", "hdr-thread:3"),
        ]);
        let inbound = InboundCodexRuntimeIdentity::from_request(Some(&body), Some(&headers));
        assert_eq!(inbound.session_id.as_deref(), Some("blob-session"));
        assert_eq!(inbound.thread_id.as_deref(), Some("blob-thread"));
        assert_eq!(inbound.turn_id.as_deref(), Some("flat-turn"));
        assert_eq!(inbound.window_id.as_deref(), Some("flat-thread:0"));
        assert_eq!(inbound.request_kind, Some(CodexRequestKind::Turn));
        assert!(inbound.prompt_cache_key_present);
        assert!(inbound.previous_response_id_present);
        assert_eq!(inbound.root(), Some("blob-session"));
        assert_eq!(inbound.turn_key().as_deref(), Some("flat-turn"));

        let header_only = InboundCodexRuntimeIdentity::from_request(None, Some(&headers));
        assert_eq!(header_only.session_id.as_deref(), Some("hdr-session"));
        assert_eq!(header_only.window_id.as_deref(), Some("hdr-thread:3"));
        assert!(!header_only.prompt_cache_key_present);
        assert!(header_only
            .turn_key()
            .unwrap()
            .starts_with("hdr-session\0hdr-thread\0"));

        let memory = InboundCodexRuntimeIdentity::from_request(
            None,
            Some(&header_map(&[
                ("thread-id", "t"),
                ("x-codex-turn-metadata", r#"{"request_kind":"memory"}"#),
            ])),
        );
        assert!(memory.is_memory());
        assert_eq!(memory.root(), Some("t"));

        let empty = InboundCodexRuntimeIdentity::from_request(Some(&json!({"input": []})), None);
        assert_eq!(empty.root(), None);
        assert_eq!(empty.turn_key(), None);
    }

    // ----- resolution -------------------------------------------------------

    #[tokio::test]
    async fn no_inbound_root_is_passthrough() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let resolution = resolve_outbound_codex_runtime_identity(
            &store,
            &scope(4, 8),
            &InboundCodexRuntimeIdentity::default(),
            None,
            at(1_756_857_600),
        )
        .await;
        assert_eq!(resolution, CodexRuntimeIdentityResolution::Passthrough);
    }

    #[tokio::test]
    async fn threads_mint_by_arrival_then_reuse_least_recently_active() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        // A generous turn budget so thread assignment is what is under test.
        let s = scope(3, 512);
        let start = at(1_756_857_600);
        let bound = usize::try_from(s.thread_bound(s.day_id(start))).unwrap();
        assert!((2..=3).contains(&bound));
        let mut first = Vec::new();
        for i in 0..24usize {
            let now = start + Duration::from_secs(i as u64);
            let inbound = inbound(&format!("s{i}"), &format!("t{i}"), Some(&format!("u{i}")));
            let out = rewrite(
                resolve_outbound_codex_runtime_identity(&store, &s, &inbound, None, now).await,
            );
            assert!(is_uuid_v7(&out.thread_id), "{}", out.thread_id);
            assert_eq!(out.session_id, out.thread_id);
            assert_eq!(out.window_id, format!("{}:0", out.thread_id));
            assert!(out.turn_id.as_deref().is_some_and(is_uuid_v7));
            // Every root within the budget mints its own turn, whether it opens
            // a fresh thread or folds a new inbound turn onto an existing one.
            assert_eq!(out.turn_source, OutboundTurnSource::Minted);
            if i < bound {
                assert!(
                    first
                        .iter()
                        .all(|previous: &OutboundCodexRuntimeIdentity| previous.thread_id
                            != out.thread_id),
                    "root {i} should have minted a new thread"
                );
                assert_eq!(v7_millis(&out.thread_id), unix_millis(now));
            } else {
                assert_eq!(
                    out.thread_id,
                    first[i - bound].thread_id,
                    "root {i} should reuse the least recently active thread"
                );
            }
            first.push(out);
        }
        let threads: HashSet<&str> = first.iter().map(|out| out.thread_id.as_str()).collect();
        assert_eq!(threads.len(), bound);
        // A root always keeps its own thread (root freeze), even after other
        // roots folded onto it and moved its turns on.
        for (i, previous) in first.iter().enumerate() {
            let again = rewrite(
                resolve_outbound_codex_runtime_identity(
                    &store,
                    &s,
                    &inbound(&format!("s{i}"), &format!("t{i}"), Some(&format!("u{i}"))),
                    None,
                    start + Duration::from_secs(600),
                )
                .await,
            );
            assert_eq!(
                again.thread_id, previous.thread_id,
                "root {i} changed thread"
            );
        }
    }

    #[tokio::test]
    async fn account_ceilings_hold_below_the_official_thresholds() {
        // The whole point of the fix: both ceilings can sit well under the
        // official ~100/day risk-control threshold and are never exceeded over
        // any 24h window, no matter how much traffic arrives.
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(5, 40);
        let now = at(1_756_857_600);
        let day = s.day_id(now);
        let thread_bound = usize::try_from(s.thread_bound(day)).unwrap();
        let turn_bound = usize::try_from(s.turn_bound(day)).unwrap();
        assert!((3..=5).contains(&thread_bound) && thread_bound < 8);
        assert!((20..=40).contains(&turn_bound) && turn_bound < 64);
        let mut threads = HashSet::new();
        let mut turns = HashSet::new();
        for i in 0..80usize {
            for j in 0..4usize {
                let inbound = inbound(
                    &format!("s{i}"),
                    &format!("t{i}"),
                    Some(&format!("u{i}-{j}")),
                );
                let out = rewrite(
                    resolve_outbound_codex_runtime_identity(&store, &s, &inbound, None, now).await,
                );
                threads.insert(out.thread_id);
                if let Some(turn) = out.turn_id {
                    turns.insert(turn);
                }
            }
        }
        // Hard caps: the realized counts never exceed the per-day ceilings, and
        // both stay under the official thresholds.
        assert!(threads.len() <= thread_bound, "{} threads", threads.len());
        assert_eq!(
            turns.len(),
            turn_bound,
            "turn ceiling is a hard account cap"
        );
        assert!(threads.len() < 8 && turns.len() < 64);
        // A thread cannot exist without a turn, so the turn budget also bounds
        // how many threads can ever open.
        assert!(threads.len() <= turns.len());
    }

    #[tokio::test]
    async fn turn_ceiling_of_one_mints_exactly_one_turn() {
        // The extreme case the operator can now dial to: one turn per day. Every
        // inbound turn, new or chained, steers into the single minted turn, so
        // the account never shows a second turn id.
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(1, 1);
        let now = at(1_756_857_600);
        assert_eq!(s.turn_bound(s.day_id(now)), 1);
        let first = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u1")),
                None,
                now,
            )
            .await,
        );
        assert_eq!(first.turn_source, OutboundTurnSource::Minted);
        let only_turn = first.turn_id.clone().unwrap();
        let mut turns = HashSet::from([only_turn.clone()]);
        for i in 0..12usize {
            let mut inbound = inbound(&format!("s{i}"), &format!("t{i}"), Some(&format!("v{i}")));
            if i % 2 == 0 {
                inbound.previous_response_id_present = true;
            }
            let out = rewrite(
                resolve_outbound_codex_runtime_identity(&store, &s, &inbound, None, now).await,
            );
            assert_eq!(out.thread_id, first.thread_id, "one thread only");
            assert_eq!(out.turn_id.as_deref(), Some(only_turn.as_str()));
            assert_eq!(out.turn_source, OutboundTurnSource::Steered);
            assert!(!out.forwards_turn_state(), "steered turn-state stripped");
            turns.insert(out.turn_id.unwrap());
        }
        assert_eq!(turns.len(), 1, "exactly one turn id all day");
    }

    #[tokio::test]
    async fn no_turn_id_revisit_within_a_thread_under_interleaved_roots() {
        // Two conversations folded onto one outbound thread (ceiling 1). Their
        // turns must read as one forward-only sequence: once the thread leaves a
        // turn id behind, no later request may bring it back, even a replay.
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(1, 512);
        let now = at(1_756_857_600);
        // (label, session, thread, turn, chained)
        let steps = [
            ("a1", "sa", "ta", "ua1"),
            ("b1", "sb", "tb", "ub1"),
            ("a1-replay", "sa", "ta", "ua1"),
            ("a2", "sa", "ta", "ua2"),
            ("b1-replay", "sb", "tb", "ub1"),
            ("b2", "sb", "tb", "ub2"),
        ];
        let mut trace: Vec<(&str, String, OutboundTurnSource, String)> = Vec::new();
        for (label, session, thread, turn) in steps {
            let out = rewrite(
                resolve_outbound_codex_runtime_identity(
                    &store,
                    &s,
                    &inbound(session, thread, Some(turn)),
                    None,
                    now,
                )
                .await,
            );
            trace.push((
                label,
                out.thread_id.clone(),
                out.turn_source,
                out.turn_id.clone().unwrap(),
            ));
        }
        // One outbound thread throughout.
        let thread = trace[0].1.clone();
        assert!(trace.iter().all(|step| step.1 == thread));
        // A replay never revisits its own earlier turn once the thread moved on:
        // it steers into whatever the thread's open turn is now.
        let turn_of = |label: &str| {
            trace
                .iter()
                .find(|step| step.0 == label)
                .map(|step| step.3.clone())
                .unwrap()
        };
        assert_ne!(turn_of("a1-replay"), turn_of("a1"), "a1 was revisited");
        assert_eq!(turn_of("a1-replay"), turn_of("b1"), "should steer forward");
        assert_eq!(
            trace.iter().find(|s| s.0 == "a1-replay").unwrap().2,
            OutboundTurnSource::Steered
        );
        assert_ne!(turn_of("b1-replay"), turn_of("b1"), "b1 was revisited");
        assert_eq!(turn_of("b1-replay"), turn_of("a2"), "should steer forward");
        // The wire sequence, collapsed to runs of equal ids, uses every id in
        // exactly one run: a real thread never returns to a turn it left.
        let mut runs: Vec<&String> = Vec::new();
        for (_, _, _, turn) in &trace {
            if runs.last().map(|last| *last != turn).unwrap_or(true) {
                runs.push(turn);
            }
        }
        let distinct: HashSet<&String> = runs.iter().copied().collect();
        assert_eq!(runs.len(), distinct.len(), "a turn id came back: {trace:?}");
    }

    #[tokio::test]
    async fn resolved_turn_source_gates_turn_state_forwarding() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(1, 512);
        let now = at(1_756_857_600);
        let resolve = |inbound: InboundCodexRuntimeIdentity| {
            let store = &store;
            let s = &s;
            async move {
                rewrite(
                    resolve_outbound_codex_runtime_identity(store, s, &inbound, None, now).await,
                )
            }
        };
        // Minted: a fresh turn, upstream issues its own token.
        let minted = resolve(inbound("sa", "ta", Some("ua1"))).await;
        assert_eq!(minted.turn_source, OutboundTurnSource::Minted);
        assert!(!minted.forwards_turn_state());
        // Frozen: the same inbound turn while it is still the thread's open one.
        let frozen = resolve(inbound("sa", "ta", Some("ua1"))).await;
        assert_eq!(frozen.turn_source, OutboundTurnSource::Frozen);
        assert_eq!(frozen.turn_id, minted.turn_id);
        assert!(frozen.forwards_turn_state());
        // Fold a second root, moving the open turn on.
        let folded = resolve(inbound("sb", "tb", Some("ub1"))).await;
        assert_eq!(folded.turn_source, OutboundTurnSource::Minted);
        // Now the first turn is superseded: a replay steers and strips state.
        let steered = resolve(inbound("sa", "ta", Some("ua1"))).await;
        assert_eq!(steered.turn_source, OutboundTurnSource::Steered);
        assert_ne!(steered.turn_id, minted.turn_id);
        assert!(!steered.forwards_turn_state());
        // Memory: no turn at all, state forwards (nothing turn-specific to leak).
        let mut memory = inbound("sa", "ta", None);
        memory.request_kind = Some(CodexRequestKind::Memory);
        let memory = resolve(memory).await;
        assert_eq!(memory.turn_source, OutboundTurnSource::None);
        assert!(memory.forwards_turn_state());
    }

    #[tokio::test]
    async fn roster_is_a_trailing_24h_window_not_a_calendar_day() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(2, 512);
        let t0 = at(1_756_857_600);
        let bound = usize::try_from(s.thread_bound(s.day_id(t0))).unwrap();
        let mut active = Vec::new();
        for i in 0..bound {
            let out = rewrite(
                resolve_outbound_codex_runtime_identity(
                    &store,
                    &s,
                    &inbound(&format!("s{i}"), &format!("t{i}"), Some("u1")),
                    None,
                    t0 + Duration::from_secs(i as u64),
                )
                .await,
            );
            active.push(out.thread_id);
        }
        let active: HashSet<String> = active.into_iter().collect();
        assert_eq!(active.len(), bound);
        // A new root inside the window reuses one of the active threads.
        let inside = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s-in", "t-in", Some("u1")),
                None,
                t0 + Duration::from_secs(100),
            )
            .await,
        );
        assert!(active.contains(&inside.thread_id), "reused a live thread");
        // More than 24h later the old threads have aged out of the window: a
        // new root mints a fresh thread instead of reusing an expired one.
        // Past the window relative to the last activity (the reuse at +100s),
        // so every live thread has aged out.
        let later = t0 + Duration::from_secs(DAY_WINDOW_SECS + 200);
        let fresh = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s-late", "t-late", Some("u1")),
                None,
                later,
            )
            .await,
        );
        assert!(
            !active.contains(&fresh.thread_id),
            "stale thread reused past the 24h window"
        );
        assert_eq!(v7_millis(&fresh.thread_id), unix_millis(later));
    }

    #[tokio::test]
    async fn root_freeze_survives_day_rollover_and_turn_freeze_too() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(8, 64);
        let day0 = at(1_756_857_600);
        let inbound_turn = inbound("s", "t", Some("u1"));
        let first = rewrite(
            resolve_outbound_codex_runtime_identity(&store, &s, &inbound_turn, None, day0).await,
        );
        // Next day (< the 36h key lifetime): same inbound root/turn keeps both IDs.
        let day1 = day0 + Duration::from_secs(DAY_WINDOW_SECS);
        assert_ne!(s.day_id(day0), s.day_id(day1));
        let second = rewrite(
            resolve_outbound_codex_runtime_identity(&store, &s, &inbound_turn, None, day1).await,
        );
        assert_eq!(second.thread_id, first.thread_id);
        assert_eq!(second.turn_id, first.turn_id);
        assert_eq!(second.turn_source, OutboundTurnSource::Frozen);
        // A new inbound turn on the frozen root mints under the frozen thread
        // even though the day changed.
        let third = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u2")),
                None,
                day1,
            )
            .await,
        );
        assert_eq!(third.thread_id, first.thread_id);
        assert_ne!(third.turn_id, first.turn_id);
        assert_eq!(third.turn_source, OutboundTurnSource::Minted);
    }

    #[tokio::test]
    async fn chained_request_continues_the_threads_open_turn() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(8, 64);
        let now = at(1_756_857_600);
        let first = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u1")),
                None,
                now,
            )
            .await,
        );
        // A chained request with an unknown inbound turn continues the open
        // turn rather than minting: it steers, and its turn-state is stripped.
        let mut chained = inbound("s", "t", Some("u-unknown"));
        chained.previous_response_id_present = true;
        let second =
            rewrite(resolve_outbound_codex_runtime_identity(&store, &s, &chained, None, now).await);
        assert_eq!(second.thread_id, first.thread_id);
        assert_eq!(second.turn_id, first.turn_id);
        assert_eq!(second.turn_source, OutboundTurnSource::Steered);
        assert!(!second.forwards_turn_state());
        // Not chained: a new inbound turn mints a new outbound turn.
        let third = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u-new")),
                None,
                now,
            )
            .await,
        );
        assert_ne!(third.turn_id, first.turn_id);
        assert_eq!(third.turn_source, OutboundTurnSource::Minted);
    }

    #[tokio::test]
    async fn memory_requests_share_thread_but_carry_no_turn() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(8, 64);
        let now = at(1_756_857_600);
        let turn = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u1")),
                None,
                now,
            )
            .await,
        );
        let mut memory = inbound("s", "t", None);
        memory.request_kind = Some(CodexRequestKind::Memory);
        let out =
            rewrite(resolve_outbound_codex_runtime_identity(&store, &s, &memory, None, now).await);
        assert_eq!(out.thread_id, turn.thread_id);
        assert_eq!(out.turn_id, None);
        assert_eq!(out.turn_source, OutboundTurnSource::None);
        assert!(out.forwards_turn_state());
    }

    #[tokio::test]
    async fn ws_snapshot_stays_authoritative_for_its_bound_turn() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(8, 64);
        let now = at(1_756_857_600);
        let snapshot = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u1")),
                None,
                now,
            )
            .await,
        );
        // Same inbound turn: the snapshot answers without the store (an
        // outage falls back to it) and keeps identical IDs.
        let unavailable = CodexRuntimeIdentityStore::unavailable(&state);
        let same = rewrite(
            resolve_outbound_codex_runtime_identity(
                &unavailable,
                &s,
                &inbound("s", "t", Some("u1")),
                Some(&snapshot),
                now,
            )
            .await,
        );
        assert_eq!(same.thread_id, snapshot.thread_id);
        assert_eq!(same.turn_id, snapshot.turn_id);
        assert_eq!(same.turn_source, OutboundTurnSource::Snapshot);
        // Another turn on the same root mints a new open turn on the thread
        // (moving it on). The bound connection's own step still keeps its turn
        // (a WS stream cannot be steered mid-flight).
        let moved_on = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("v-fold")),
                None,
                now,
            )
            .await,
        );
        assert_eq!(moved_on.thread_id, snapshot.thread_id);
        assert_ne!(moved_on.turn_id, snapshot.turn_id);
        assert_eq!(moved_on.turn_source, OutboundTurnSource::Minted);
        let step = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u1")),
                Some(&snapshot),
                now,
            )
            .await,
        );
        assert_eq!(step.thread_id, snapshot.thread_id);
        assert_eq!(step.turn_id, snapshot.turn_id);
        assert_eq!(step.turn_source, OutboundTurnSource::Snapshot);
        // A new inbound turn: thread from snapshot, turn minted under it.
        let next = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u2")),
                Some(&snapshot),
                now,
            )
            .await,
        );
        assert_eq!(next.thread_id, snapshot.thread_id);
        assert_ne!(next.turn_id, snapshot.turn_id);
        assert_eq!(next.turn_source, OutboundTurnSource::Minted);
        // Snapshot for another root is ignored.
        let other = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("other2", "t3", Some("u1")),
                Some(&snapshot),
                now,
            )
            .await,
        );
        assert_eq!(other.inbound_root, "other2");
    }

    #[tokio::test]
    async fn store_unavailable_falls_back_to_passthrough_or_snapshot() {
        let state = memory_state();
        let unavailable = CodexRuntimeIdentityStore::unavailable(&state);
        let s = scope(8, 64);
        let now = at(1_756_857_600);
        let resolution = resolve_outbound_codex_runtime_identity(
            &unavailable,
            &s,
            &inbound("s", "t", Some("u1")),
            None,
            now,
        )
        .await;
        assert_eq!(resolution, CodexRuntimeIdentityResolution::Passthrough);

        let snapshot = outbound_fixture(Some("snap-turn"), OutboundTurnSource::Minted);
        let mut inbound_new_turn = inbound("in-session", "t", Some("u-new"));
        inbound_new_turn.previous_response_id_present = false;
        let out = rewrite(
            resolve_outbound_codex_runtime_identity(
                &unavailable,
                &s,
                &inbound_new_turn,
                Some(&snapshot),
                now,
            )
            .await,
        );
        assert_eq!(out.thread_id, snapshot.thread_id);
        assert_eq!(out.turn_id.as_deref(), Some("snap-turn"));
        assert_eq!(out.turn_source, OutboundTurnSource::Snapshot);
        assert_eq!(out.inbound_turn_key.as_deref(), Some("u-new"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_new_roots_never_mint_past_the_daily_bound() {
        let state = std::sync::Arc::new(memory_state());
        let s = scope(4, 64);
        let now = at(1_756_857_600);
        let bound = usize::try_from(s.thread_bound(s.day_id(now))).unwrap();
        let mut handles = Vec::new();
        for i in 0..24 {
            let state = state.clone();
            let s = s.clone();
            handles.push(tokio::spawn(async move {
                let store = CodexRuntimeIdentityStore::new(&state);
                let inbound = inbound(&format!("s{i}"), &format!("t{i}"), Some("u1"));
                let out = rewrite(
                    resolve_outbound_codex_runtime_identity(&store, &s, &inbound, None, now).await,
                );
                (i, out.thread_id)
            }));
        }
        let mut by_root = BTreeMap::new();
        for handle in handles {
            let (i, thread) = handle.await.unwrap();
            by_root.insert(i, thread);
        }
        let threads: HashSet<&String> = by_root.values().collect();
        assert!(
            threads.len() <= bound,
            "{} threads for bound {bound}",
            threads.len()
        );
        assert!(threads.len() > 1);
        // Every root keeps the thread it was assigned.
        let store = CodexRuntimeIdentityStore::new(&state);
        for (i, thread) in &by_root {
            let again = rewrite(
                resolve_outbound_codex_runtime_identity(
                    &store,
                    &s,
                    &inbound(&format!("s{i}"), &format!("t{i}"), Some("u1")),
                    None,
                    now + Duration::from_secs(1),
                )
                .await,
            );
            assert_eq!(&again.thread_id, thread);
        }
    }

    #[tokio::test]
    async fn concurrent_mints_converge_on_one_identity() {
        let state = std::sync::Arc::new(memory_state());
        let s = scope(8, 64);
        let now = at(1_756_857_600);
        let mut handles = Vec::new();
        for _ in 0..16 {
            let state = state.clone();
            let s = s.clone();
            handles.push(tokio::spawn(async move {
                let store = CodexRuntimeIdentityStore::new(&state);
                rewrite(
                    resolve_outbound_codex_runtime_identity(
                        &store,
                        &s,
                        &inbound("s", "t", Some("u1")),
                        None,
                        now,
                    )
                    .await,
                )
            }));
        }
        let mut threads = HashSet::new();
        let mut turns = HashSet::new();
        for handle in handles {
            let out = handle.await.unwrap();
            threads.insert(out.thread_id);
            turns.insert(out.turn_id.unwrap());
        }
        assert_eq!(threads.len(), 1);
        assert_eq!(turns.len(), 1);
    }

    // ----- rewrite ----------------------------------------------------------

    #[test]
    fn headers_rewrite_only_values_equal_to_inbound_ids() {
        let inbound = inbound("in-session", "in-thread", Some("in-turn"));
        let outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Frozen);
        let mut headers = btree(&[
            ("session-id", "in-session"),
            ("thread-id", "in-thread"),
            ("x-codex-window-id", "in-thread:2"),
            ("x-client-request-id", "in-thread"),
            ("x-codex-parent-thread-id", "parent"),
            ("x-openai-subagent", "explore"),
            ("x-codex-turn-state", "token"),
            ("x-codex-installation-id", "inst"),
            ("session_id", "abcd1234abcd1234"),
            ("conversation_id", "abcd1234abcd1234"),
            (
                "x-codex-turn-metadata",
                r#"{"installation_id":"inst","session_id":"in-session","thread_id":"in-thread","turn_id":"in-turn","window_id":"in-thread:2","request_kind":"turn","parent_thread_id":"parent","subagent_kind":"explore"}"#,
            ),
        ]);
        apply_outbound_codex_runtime_identity(
            &mut headers,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::Headers,
            None,
        );
        assert_eq!(headers["session-id"], "out-thread");
        assert_eq!(headers["thread-id"], "out-thread");
        assert_eq!(headers["x-codex-window-id"], "out-thread:0");
        assert_eq!(headers["x-client-request-id"], "out-thread");
        assert_eq!(headers["x-codex-installation-id"], "inst");
        assert!(!headers.contains_key("x-codex-parent-thread-id"));
        assert!(!headers.contains_key("x-openai-subagent"));
        assert!(
            headers.contains_key("x-codex-turn-state"),
            "frozen turn forwards state"
        );
        assert!(!headers.contains_key("session_id"));
        assert!(!headers.contains_key("conversation_id"));
        let blob: Value = serde_json::from_str(&headers["x-codex-turn-metadata"]).unwrap();
        assert_eq!(blob["installation_id"], "inst");
        assert_eq!(blob["session_id"], "out-thread");
        assert_eq!(blob["thread_id"], "out-thread");
        assert_eq!(blob["turn_id"], "out-turn");
        assert_eq!(blob["window_id"], "out-thread:0");
        assert_eq!(blob["request_kind"], "turn");
        assert!(blob.get("parent_thread_id").is_none());
        assert!(blob.get("subagent_kind").is_none());

        // Dash / window values that are not the inbound IDs stay (foreign
        // values); x-client-request-id is always the outbound thread.
        let mut foreign = btree(&[
            ("session-id", "someone-else"),
            ("x-client-request-id", "trace-abc"),
            ("x-codex-window-id", "foreign:1"),
        ]);
        apply_outbound_codex_runtime_identity(
            &mut foreign,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::Headers,
            None,
        );
        assert_eq!(foreign["session-id"], "someone-else");
        assert_eq!(foreign["x-client-request-id"], "out-thread");
        assert_eq!(foreign["x-codex-window-id"], "foreign:1");
    }

    #[test]
    fn minted_turn_strips_turn_state_and_short_headers() {
        let inbound = inbound("in-session", "in-thread", Some("in-turn"));
        let outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Minted);
        let mut headers = btree(&[
            ("x-codex-turn-state", "token"),
            ("session_id", "client-set"),
            ("conversation_id", "derived"),
        ]);
        apply_outbound_codex_runtime_identity(
            &mut headers,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::Headers,
            None,
        );
        assert!(!headers.contains_key("x-codex-turn-state"));
        assert!(!headers.contains_key("session_id"));
        assert!(!headers.contains_key("conversation_id"));
    }

    #[test]
    fn body_rewrite_keeps_flat_and_blob_consistent() {
        let inbound = inbound("in-session", "in-thread", Some("in-turn"));
        let outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Minted);
        let mut body = json!({
            "prompt_cache_key": "in-session",
            "client_metadata": {
                "x-codex-installation-id": "inst",
                "session_id": "in-session",
                "thread_id": "in-thread",
                "x-codex-window-id": "in-thread:0",
                "turn_id": "in-turn",
                "x-codex-parent-thread-id": "parent",
                "x-openai-subagent": "explore",
                "parent_turn_id": "pt",
                "root_turn_id": "rt",
                "x-codex-turn-state": "token",
                "x-codex-turn-metadata": json!({
                    "installation_id": "inst",
                    "session_id": "in-session",
                    "thread_id": "in-thread",
                    "turn_id": "in-turn",
                    "window_id": "in-thread:0",
                    "request_kind": "turn",
                    "forked_from_thread_id": "fork",
                    "thread_source": "subagent",
                    "sandbox": "seatbelt",
                    "workspaces": ["/tmp/项目"]
                }).to_string()
            }
        });
        let mut headers = BTreeMap::new();
        apply_outbound_codex_runtime_identity(
            &mut headers,
            Some(&mut body),
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::WsStepBody,
            Some(WINDOWS_UA),
        );
        assert_eq!(body["prompt_cache_key"], "out-thread");
        let meta = body["client_metadata"].as_object().unwrap();
        assert_eq!(meta["x-codex-installation-id"], "inst");
        assert_eq!(meta["session_id"], "out-thread");
        assert_eq!(meta["thread_id"], "out-thread");
        assert_eq!(meta["x-codex-window-id"], "out-thread:0");
        assert_eq!(meta["turn_id"], "out-turn");
        for key in FLAT_LEAK_KEYS {
            assert!(!meta.contains_key(*key), "{key} leaked");
        }
        assert_eq!(
            meta["root_turn_id"], "out-turn",
            "root turn is its own root"
        );
        assert!(!meta.contains_key("x-codex-turn-state"));
        let raw = meta["x-codex-turn-metadata"].as_str().unwrap();
        assert!(raw.is_ascii(), "{raw}");
        let blob: Value = serde_json::from_str(raw).unwrap();
        assert_eq!(blob["installation_id"], "inst");
        assert_eq!(blob["session_id"], "out-thread");
        assert_eq!(blob["thread_id"], "out-thread");
        assert_eq!(blob["turn_id"], "out-turn");
        assert_eq!(blob["window_id"], "out-thread:0");
        // The blob is what the client named by the (Windows) handshake
        // user-agent sends: its sandbox tag, and every key a current client
        // always carries.
        assert_eq!(blob["sandbox"], "windows_elevated");
        assert_eq!(blob["sandbox_mode"], "workspace-write");
        assert_eq!(blob["agent_name"], "/root");
        assert_eq!(blob["window_number"], 0);
        assert_eq!(blob["context_window_id"], FIXTURE_CONTEXT_WINDOW);
        assert_eq!(blob["auto_review_enabled"], false);
        assert_eq!(blob["workspaces"][0], "/tmp/项目");
        assert_eq!(
            blob["thread_source"], "user",
            "folded thread presents as user"
        );
        for key in BLOB_LEAK_KEYS {
            assert!(blob.get(*key).is_none(), "{key} leaked");
        }
        // Serialized string must not contain the inbound IDs anywhere.
        let serialized = body.to_string();
        for leaked in ["in-session", "in-thread", "in-turn", "parent", "fork"] {
            assert!(
                !serialized.contains(leaked),
                "{leaked} leaked: {serialized}"
            );
        }
    }

    #[test]
    fn body_rewrite_prompt_cache_key_rules() {
        let outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Frozen);
        // Missing in original → Aether filler value replaced with outbound session.
        let mut missing = inbound("in-session", "in-thread", Some("in-turn"));
        missing.prompt_cache_key_present = false;
        let mut body = json!({ "prompt_cache_key": "1b4e28ba-2fa1-5d3e-9c2c-000000000000" });
        apply_outbound_codex_runtime_identity(
            &mut BTreeMap::new(),
            Some(&mut body),
            &missing,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        assert_eq!(body["prompt_cache_key"], "out-thread");
        // Explicit foreign value stays.
        let present = inbound("in-session", "in-thread", Some("in-turn"));
        let mut body = json!({ "prompt_cache_key": "my-own-key" });
        apply_outbound_codex_runtime_identity(
            &mut BTreeMap::new(),
            Some(&mut body),
            &present,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        assert_eq!(body["prompt_cache_key"], "my-own-key");
        // guardian: prefix is an Aether-derived session key.
        let mut body = json!({ "prompt_cache_key": "guardian:in-session" });
        apply_outbound_codex_runtime_identity(
            &mut BTreeMap::new(),
            Some(&mut body),
            &present,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        assert_eq!(body["prompt_cache_key"], "out-thread");
        // Equal to inbound thread → outbound session.
        let mut body = json!({ "prompt_cache_key": "in-thread" });
        apply_outbound_codex_runtime_identity(
            &mut BTreeMap::new(),
            Some(&mut body),
            &present,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        assert_eq!(body["prompt_cache_key"], "out-thread");
    }

    #[test]
    fn memory_blob_drops_identity_but_flat_and_headers_keep_thread() {
        let mut inbound = inbound("in-session", "in-thread", None);
        inbound.request_kind = Some(CodexRequestKind::Memory);
        let outbound = outbound_fixture(None, OutboundTurnSource::None);
        let mut body = json!({
            "prompt_cache_key": "in-session",
            "client_metadata": {
                "x-codex-installation-id": "inst",
                "session_id": "in-session",
                "thread_id": "in-thread",
                "x-codex-window-id": "in-thread:0",
                "turn_id": "stale-turn",
                "x-codex-turn-metadata": json!({
                    "request_kind": "memory",
                    "installation_id": "inst",
                    "session_id": "in-session",
                    "thread_id": "in-thread",
                    "window_id": "in-thread:0",
                    "thread_source": "memory_consolidation"
                }).to_string()
            }
        });
        let mut headers = btree(&[
            ("session-id", "in-session"),
            ("thread-id", "in-thread"),
            ("x-codex-window-id", "in-thread:0"),
            ("x-codex-installation-id", "inst"),
            (
                "x-codex-turn-metadata",
                r#"{"request_kind":"memory","installation_id":"inst","session_id":"in-session","thread_id":"in-thread","window_id":"in-thread:0"}"#,
            ),
        ]);
        apply_outbound_codex_runtime_identity(
            &mut headers,
            Some(&mut body),
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        assert_eq!(headers["session-id"], "out-thread");
        assert_eq!(headers["thread-id"], "out-thread");
        assert_eq!(headers["x-codex-window-id"], "out-thread:0");
        assert_eq!(headers["x-codex-installation-id"], "inst");
        let header_blob: Value = serde_json::from_str(&headers["x-codex-turn-metadata"]).unwrap();
        assert_eq!(header_blob, json!({ "request_kind": "memory" }));
        let meta = body["client_metadata"].as_object().unwrap();
        assert_eq!(meta["session_id"], "out-thread");
        assert_eq!(meta["thread_id"], "out-thread");
        assert_eq!(meta["x-codex-window-id"], "out-thread:0");
        assert!(!meta.contains_key("turn_id"));
        let blob: Value =
            serde_json::from_str(meta["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
        assert_eq!(
            blob,
            json!({ "request_kind": "memory", "thread_source": "memory_consolidation" })
        );
    }

    #[test]
    fn blob_rewrite_does_not_add_missing_keys_and_handles_object_form() {
        let outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Frozen);
        // Prewarm/no-kind blob without window/installation keeps its shape.
        let rewritten = rewrite_codex_turn_metadata_string(
            r#"{"session_id":"a","thread_id":"b","turn_id":"c"}"#,
            &outbound,
            None,
        )
        .unwrap();
        let blob: Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(
            blob,
            json!({ "session_id": "out-thread", "thread_id": "out-thread", "turn_id": "out-turn" })
        );
        assert_eq!(
            rewrite_codex_turn_metadata_string("not json", &outbound, None),
            None
        );
        assert_eq!(
            rewrite_codex_turn_metadata_string("[1]", &outbound, None),
            None
        );
        // Object form in the body is rewritten in place.
        let mut body = json!({
            "client_metadata": {
                "x-codex-turn-metadata": { "thread_id": "in", "parent_turn_id": "p" }
            }
        });
        apply_outbound_codex_runtime_identity(
            &mut BTreeMap::new(),
            Some(&mut body),
            &inbound("in", "in", Some("u")),
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        assert_eq!(
            body["client_metadata"]["x-codex-turn-metadata"],
            json!({ "thread_id": "out-thread" })
        );
    }

    #[test]
    fn blob_rewrite_follows_thread_window_and_normalizes_tree_keys() {
        // codex-tui >= 0.153 fields follow the synthetic thread's own window
        // state: `window_id == "{thread}:{window_number}"` and one context
        // window id per (thread, window). Inbound values never pass through.
        let mut outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Frozen);
        outbound.window_number = 3;
        outbound.window_id = "out-thread:3".to_string();
        let rewritten = rewrite_codex_turn_metadata_string(
            r#"{"session_id":"in","thread_id":"in","turn_id":"t","window_id":"in:71","window_number":71,"context_window_id":"01a06ee4-8a47-79a0-b871-dfca8c798e84","request_kind":"turn","agent_name":"/root/final_check","thread_source":"subagent","root_turn_id":"other","forked_from_ordinal_exclusive":4,"workspace_kind":"project","model":"gpt-5","compaction":{"phase":"mid_turn"}}"#,
            &outbound,
            None,
        )
        .unwrap();
        let blob: Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(blob["window_id"], "out-thread:3");
        assert_eq!(blob["window_number"], 3);
        assert_eq!(blob["context_window_id"], FIXTURE_CONTEXT_WINDOW);
        assert_eq!(blob["agent_name"], "/root");
        assert_eq!(blob["thread_source"], "user");
        assert_eq!(blob["root_turn_id"], "out-turn");
        assert!(
            blob.get("forked_from_ordinal_exclusive").is_none(),
            "fork leaked"
        );
        assert!(
            blob.get("workspace_kind").is_none(),
            "app-server-only key dropped under a terminal user-agent"
        );
        assert_eq!(blob["compaction"]["phase"], "mid_turn");
        assert!(blob.get("model").is_none(), "unknown key must be stripped");
        assert!(
            !rewritten.contains("01a06ee4"),
            "real context window leaked"
        );

        // Store outage on a fresh thread: no context id → key removed, not leaked.
        outbound.context_window_id = None;
        let degraded = rewrite_codex_turn_metadata_string(
            r#"{"thread_id":"in","window_number":2,"context_window_id":"x","request_kind":"turn"}"#,
            &outbound,
            None,
        )
        .unwrap();
        let degraded: Value = serde_json::from_str(&degraded).unwrap();
        assert_eq!(degraded["window_number"], 3);
        assert!(degraded.get("context_window_id").is_none());

        // No request kind: official blobs carry no window keys, none are added.
        let older = rewrite_codex_turn_metadata_string(
            r#"{"session_id":"in","thread_id":"in","window_id":"in:2"}"#,
            &outbound,
            None,
        )
        .unwrap();
        let older: Value = serde_json::from_str(&older).unwrap();
        assert_eq!(older["window_id"], "out-thread:3");
        assert!(older.get("window_number").is_none());
        assert!(older.get("context_window_id").is_none());

        // Memory blobs: no identity and no window keys at all.
        let memory = rewrite_codex_turn_metadata_string(
            r#"{"request_kind":"memory","thread_id":"in","window_number":5,"context_window_id":"x","root_turn_id":"r"}"#,
            &outbound,
            None,
        )
        .unwrap();
        let memory: Value = serde_json::from_str(&memory).unwrap();
        assert_eq!(memory, json!({ "request_kind": "memory" }));
    }

    #[test]
    fn request_identity_blob_matches_current_client_shape_and_outbound_os() {
        // codex-tui 0.147 turn blob: no agent_name / window_number /
        // context_window_id / sandbox_mode / review flags. The outbound
        // user-agent is a 0.153 build, whose blob always has them, in
        // `CodexTurnMetadataPayload` field order.
        let mut outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Frozen);
        outbound.window_number = 2;
        outbound.window_id = "out-thread:2".to_string();
        let older = r#"{"installation_id":"inst","session_id":"in","thread_id":"in","turn_id":"t","window_id":"in:3","request_kind":"turn","thread_source":"user","sandbox":"none","workspaces":{"/w":{}},"turn_started_at_unix_ms":1756857600123}"#;
        let rewritten = rewrite_codex_turn_metadata_string(older, &outbound, Some(MAC_UA)).unwrap();
        let blob: Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(
            blob.as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            [
                "installation_id",
                "session_id",
                "thread_id",
                "agent_name",
                "turn_id",
                "window_id",
                "window_number",
                "context_window_id",
                "request_kind",
                "thread_source",
                "sandbox",
                "sandbox_mode",
                "auto_review_enabled",
                "node_repl_auto_review_required",
                "node_repl_disabled",
                "workspaces",
            ],
            "the client's own turn stamp is not copied; a frozen turn id is not a UUIDv7, so none is derived"
        );
        assert_eq!(blob["installation_id"], "inst", "profile pass owns it");
        assert_eq!(blob["session_id"], "out-thread");
        assert_eq!(blob["turn_id"], "out-turn");
        assert_eq!(blob["agent_name"], "/root");
        assert_eq!(blob["window_id"], "out-thread:2");
        assert_eq!(
            blob["window_number"], 2,
            "inbound `:3` suffix never survives"
        );
        assert_eq!(blob["context_window_id"], FIXTURE_CONTEXT_WINDOW);
        assert_eq!(blob["thread_source"], "user");
        assert_eq!(blob["sandbox"], "none", "OS-independent tag stays");
        assert_eq!(
            blob["sandbox_mode"], "danger-full-access",
            "the only policy a real client pairs with `none`"
        );
        assert_eq!(blob["auto_review_enabled"], false);
        assert_eq!(blob["node_repl_auto_review_required"], false);
        assert_eq!(blob["node_repl_disabled"], false);
        assert_eq!(blob["workspaces"]["/w"], json!({}));
        assert!(
            blob.get("turn_started_at_unix_ms").is_none(),
            "a frozen turn with no UUIDv7 outbound turn id carries no stamp"
        );
        assert!(
            blob.get("root_turn_id").is_none(),
            "optional key not invented"
        );
        assert!(!rewritten.contains("in:3"));

        // No turn stamp inbound: a real client stamps the turn start, which is
        // the outbound turn's own UUIDv7 timestamp.
        let v7_turn = uuid_v7_at(1_756_857_600_000);
        let mut stamped = outbound.clone();
        stamped.turn_id = Some(v7_turn.clone());
        let filled = rewrite_codex_turn_metadata_string(
            r#"{"thread_id":"in","request_kind":"turn","sandbox":"seccomp"}"#,
            &stamped,
            Some(MAC_UA),
        )
        .unwrap();
        let filled: Value = serde_json::from_str(&filled).unwrap();
        assert_eq!(filled["turn_id"], v7_turn);
        assert_eq!(filled["turn_started_at_unix_ms"], 1_756_857_600_000u64);

        // A client stamp inbound is ignored: many real turns fold onto one
        // outbound turn, so the stamp must always come from the outbound turn
        // id, never the client, or one synthetic turn carries many starts.
        let overridden = rewrite_codex_turn_metadata_string(
            r#"{"thread_id":"in","request_kind":"turn","turn_started_at_unix_ms":1756857600123}"#,
            &stamped,
            Some(MAC_UA),
        )
        .unwrap();
        let overridden: Value = serde_json::from_str(&overridden).unwrap();
        assert_eq!(overridden["turn_started_at_unix_ms"], 1_756_857_600_000u64);

        assert_eq!(
            filled["sandbox"], "seatbelt",
            "Linux tag under a macOS user-agent"
        );
        assert_eq!(filled["sandbox_mode"], "workspace-write");

        // The startup prewarm is sent before any task stamps the turn start
        // (codex-rs `Session::start_task`): a real prewarm blob has every
        // other current-client key but no `turn_started_at_unix_ms`.
        let prewarm = rewrite_codex_turn_metadata_string(
            r#"{"session_id":"in","thread_id":"in","turn_id":"t","window_id":"in:0","request_kind":"prewarm"}"#,
            &stamped,
            Some(LINUX_UA),
        )
        .unwrap();
        let prewarm: Value = serde_json::from_str(&prewarm).unwrap();
        assert_eq!(prewarm["window_number"], 2);
        assert_eq!(prewarm["agent_name"], "/root");
        assert_eq!(prewarm["sandbox"], "seccomp");
        assert_eq!(prewarm["sandbox_mode"], "workspace-write");
        assert_eq!(prewarm["node_repl_disabled"], false);
        assert!(
            prewarm.get("turn_started_at_unix_ms").is_none(),
            "prewarm precedes the task stamp"
        );

        // Compaction carries the same identity set and keeps values the
        // client did send (policy, flags). Its user-agent is a terminal
        // build, so the app-server-only `workspace_kind` is dropped and
        // `compaction` is the last key.
        let compaction = rewrite_codex_turn_metadata_string(
            r#"{"session_id":"in","thread_id":"in","turn_id":"t","window_id":"in:0","request_kind":"compaction","workspace_kind":"project","sandbox":"windows_elevated","sandbox_mode":"read-only","auto_review_enabled":true,"compaction":{"phase":"mid_turn"}}"#,
            &outbound,
            Some(LINUX_UA),
        )
        .unwrap();
        let compaction: Value = serde_json::from_str(&compaction).unwrap();
        let keys = compaction
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(*keys.last().unwrap(), "compaction");
        assert!(
            !keys.contains(&"workspace_kind".to_string()),
            "app-server-only key dropped under a terminal user-agent"
        );
        assert_eq!(compaction["window_number"], 2);
        assert_eq!(compaction["agent_name"], "/root");
        assert_eq!(
            compaction["sandbox"], "seccomp",
            "Windows tag under a Linux user-agent"
        );
        assert_eq!(compaction["sandbox_mode"], "read-only");
        assert_eq!(compaction["auto_review_enabled"], true);
        assert_eq!(compaction["node_repl_disabled"], false);
        assert_eq!(compaction["compaction"]["phase"], "mid_turn");

        // Sandbox projection: OS-independent tags stay, Windows keeps its own
        // two tags, everything else becomes the outbound OS's platform sandbox.
        for (user_agent, inbound, expected) in [
            (Some(MAC_UA), Some("seccomp"), "seatbelt"),
            (Some(MAC_UA), Some("windows_sandbox"), "seatbelt"),
            (Some(WINDOWS_UA), Some("seatbelt"), "windows_elevated"),
            (Some(WINDOWS_UA), Some("windows_sandbox"), "windows_sandbox"),
            (
                Some(WINDOWS_UA),
                Some("windows_elevated"),
                "windows_elevated",
            ),
            (Some(LINUX_UA), Some("windows_elevated"), "seccomp"),
            (Some(LINUX_UA), Some("bubblewrap"), "seccomp"),
            (Some(WINDOWS_UA), Some("none"), "none"),
            (Some(LINUX_UA), Some("external"), "external"),
            (None, None, "seccomp"),
            (Some(MAC_UA), None, "seatbelt"),
        ] {
            let os = OutboundClientOs::from_user_agent(user_agent);
            assert_eq!(
                os.project_sandbox(inbound),
                expected,
                "{user_agent:?} {inbound:?}"
            );
        }

        // No request kind: `sandbox` still follows the OS, the key set stays
        // the client's own (official `request_kind=None` blobs have no window).
        let no_kind = rewrite_codex_turn_metadata_string(
            r#"{"session_id":"in","thread_id":"in","turn_id":"t","sandbox":"seatbelt","sandbox_mode":"workspace-write"}"#,
            &outbound,
            Some(WINDOWS_UA),
        )
        .unwrap();
        let no_kind: Value = serde_json::from_str(&no_kind).unwrap();
        assert_eq!(no_kind["sandbox"], "windows_elevated");
        assert!(no_kind.get("window_number").is_none());
        assert!(no_kind.get("agent_name").is_none());

        // Header surface reads the user-agent from the headers themselves.
        let mut headers = btree(&[
            ("user-agent", WINDOWS_UA),
            (
                "x-codex-turn-metadata",
                r#"{"session_id":"in","thread_id":"in","turn_id":"t","window_id":"in:0","request_kind":"turn","sandbox":"seatbelt"}"#,
            ),
        ]);
        apply_outbound_codex_runtime_identity(
            &mut headers,
            None,
            &inbound("in", "in", Some("t")),
            &outbound,
            CodexRuntimeIdentitySurface::Headers,
            None,
        );
        let header_blob: Value = serde_json::from_str(&headers["x-codex-turn-metadata"]).unwrap();
        assert_eq!(header_blob["sandbox"], "windows_elevated");
        assert_eq!(header_blob["window_number"], 2);
    }

    #[test]
    fn app_server_only_blob_keys_survive_only_under_an_app_server_user_agent() {
        // `turn_trigger` and `workspace_kind` are sent only by the app-server
        // (Desktop/IDE) client. A terminal build never emits them, so they are
        // kept in their official positions only under an app-server user-agent
        // and dropped under a terminal or unknown one.
        let outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Frozen);
        let inbound = r#"{"session_id":"in","thread_id":"in","turn_id":"t","window_id":"in:0","request_kind":"turn","thread_source":"user","turn_trigger":"user_input","workspace_kind":"project"}"#;

        // App-server user-agent: both keys kept, in their official positions.
        let kept =
            rewrite_codex_turn_metadata_string(inbound, &outbound, Some(DESKTOP_UA)).unwrap();
        let kept: Value = serde_json::from_str(&kept).unwrap();
        assert_eq!(kept["turn_trigger"], "user_input");
        assert_eq!(kept["workspace_kind"], "project");
        let keys: Vec<String> = kept.as_object().unwrap().keys().cloned().collect();
        let trigger = keys.iter().position(|k| k == "turn_trigger").unwrap();
        let sandbox = keys.iter().position(|k| k == "sandbox").unwrap();
        assert!(trigger < sandbox, "turn_trigger precedes sandbox");
        assert_eq!(keys.last().unwrap(), "workspace_kind", "flat extra is last");

        // Every terminal originator, and an unknown/absent user-agent, drop both.
        for ua in [Some(MAC_UA), Some(WINDOWS_UA), Some(LINUX_UA), None] {
            let dropped = rewrite_codex_turn_metadata_string(inbound, &outbound, ua).unwrap();
            let dropped: Value = serde_json::from_str(&dropped).unwrap();
            assert!(dropped.get("turn_trigger").is_none(), "{ua:?}");
            assert!(dropped.get("workspace_kind").is_none(), "{ua:?}");
        }
    }

    #[test]
    fn whitelist_strips_unknown_keys_on_every_surface() {
        let inbound = inbound("in-session", "in-thread", Some("in-turn"));
        let outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Frozen);
        let mut body = json!({
            "client_metadata": {
                "session_id": "in-session",
                "thread_id": "in-thread",
                "turn_id": "in-turn",
                "guardian_ticket_requested": "true",
                "ws_request_header_x_openai_internal_codex_responses_lite": "true",
                "sub2api_step_correlation_id": "corr",
                "x-codex-brand-new-flat-key": "leak",
                "x-codex-turn-metadata": json!({
                    "thread_id": "in-thread",
                    "request_kind": "turn",
                    "reasoning_effort": "high"
                }).to_string()
            }
        });
        let mut headers = btree(&[
            ("x-codex-beta-features", "a,b"),
            ("x-codex-routing-hint", "model=gpt-5;tier=x"),
            ("x-openai-internal-codex-residency", "us"),
            ("x-openai-internal-codex-responses-lite", "true"),
            ("X-Codex-Brand-New-Header", "leak"),
            ("x-oai-attestation", "att"),
            ("openai-beta", "responses_websockets=2026-02-06"),
            ("session-id", "in-session"),
        ]);
        apply_outbound_codex_runtime_identity(
            &mut headers,
            Some(&mut body),
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        let meta = body["client_metadata"].as_object().unwrap();
        assert_eq!(meta["guardian_ticket_requested"], "true");
        assert_eq!(
            meta["ws_request_header_x_openai_internal_codex_responses_lite"],
            "true"
        );
        assert_eq!(meta["sub2api_step_correlation_id"], "corr");
        assert!(!meta.contains_key("x-codex-brand-new-flat-key"));
        let blob: Value =
            serde_json::from_str(meta["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
        assert_eq!(blob["thread_id"], "out-thread");
        assert_eq!(blob["request_kind"], "turn");
        assert!(
            blob.get("reasoning_effort").is_none(),
            "unknown key must be stripped"
        );
        assert_eq!(blob["window_number"], 0, "turn blob is a current client's");
        assert_eq!(headers["x-codex-beta-features"], "a,b");
        assert_eq!(headers["x-codex-routing-hint"], "model=gpt-5;tier=x");
        assert_eq!(headers["x-openai-internal-codex-residency"], "us");
        assert_eq!(headers["x-openai-internal-codex-responses-lite"], "true");
        assert_eq!(headers["openai-beta"], "responses_websockets=2026-02-06");
        assert_eq!(headers["session-id"], "out-thread");
        assert!(!headers.contains_key("X-Codex-Brand-New-Header"));
        assert!(!headers.contains_key("x-oai-attestation"));
        assert!(!body.to_string().contains("leak"));
    }

    // ----- synthetic identity (no official ids) ------------------------------

    /// A relay-shaped `/responses` body: wrapper messages first, then the real
    /// prompts (assistant replies between them), then `tail` items.
    fn synthetic_body(prompts: &[&str], tail: &[Value]) -> Value {
        let mut input = vec![
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>/w</cwd>\n</environment_context>"}]}),
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":"<user_instructions>\nbe brief\n</user_instructions>"}]}),
        ];
        for (index, prompt) in prompts.iter().enumerate() {
            input.push(json!({"type":"message","role":"user","content":[{"type":"input_text","text":prompt}]}));
            if index + 1 < prompts.len() {
                input.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}));
            }
        }
        input.extend(tail.iter().cloned());
        json!({
            "model": "gpt-5.3-codex",
            "instructions": "You are Codex",
            "input": input,
            "store": false,
            "stream": true,
            "prompt_cache_key": "5f2c7e1a-9b3d-5c4e-8a1f-000000000001"
        })
    }

    fn synthesized(body: &Value, headers: &HeaderMap) -> InboundCodexRuntimeIdentity {
        let mut inbound = InboundCodexRuntimeIdentity::from_request(Some(body), Some(headers));
        assert!(inbound.synthesize_missing_root(Some(body), headers));
        inbound
    }

    #[test]
    fn synthetic_root_follows_first_prompt_and_turn_follows_latest_prompt() {
        let relay = header_map(&[("cafecode-uid", "u1"), ("authorization", "Bearer sk-relay")]);
        let turn1 = synthesized(&synthetic_body(&["fix the tests"], &[]), &relay);
        let turn1_followup = synthesized(
            &synthetic_body(
                &["fix the tests"],
                &[
                    json!({"type":"function_call","name":"shell","arguments":"{}","call_id":"c1"}),
                    json!({"type":"function_call_output","call_id":"c1","output":"ok"}),
                ],
            ),
            &relay,
        );
        let turn2 = synthesized(
            &synthetic_body(&["fix the tests", "now the docs"], &[]),
            &relay,
        );
        assert!(turn1.is_synthetic());
        assert!(turn1.root().is_some() && turn1.turn_key().is_some());
        assert_eq!(turn1.root(), turn1_followup.root());
        assert_eq!(
            turn1.turn_key(),
            turn1_followup.turn_key(),
            "tool follow-up is the same turn"
        );
        assert_eq!(
            turn1.root(),
            turn2.root(),
            "same conversation keeps the thread"
        );
        assert_ne!(
            turn1.turn_key(),
            turn2.turn_key(),
            "a new prompt is a new turn"
        );
        assert_eq!(
            turn1.root().map(str::len),
            Some(32),
            "16-byte hex, no prompt text"
        );

        // Another downstream user with the same prompt is another thread.
        let other = synthesized(
            &synthetic_body(&["fix the tests"], &[]),
            &header_map(&[("cafecode-uid", "u2"), ("authorization", "Bearer sk-relay")]),
        );
        assert_ne!(other.root(), turn1.root());

        // Wrapper-only input (or a compaction summary) has no prompt: one
        // thread per downstream caller, still synthetic.
        let wrapper_only = synthesized(&synthetic_body(&[], &[]), &relay);
        assert!(wrapper_only.is_synthetic());
        assert_ne!(wrapper_only.root(), turn1.root());
        let summary = format!("{COMPACT_SUMMARY_PREFIX}. Summary: …");
        let summary_only = synthesized(&synthetic_body(&[summary.as_str()], &[]), &relay);
        assert_eq!(summary_only.root(), wrapper_only.root());

        // A chained request has no stable history: same fallback.
        let mut chained_body = synthetic_body(&["fix the tests"], &[]);
        chained_body["previous_response_id"] = json!("resp_1");
        let chained = synthesized(&chained_body, &relay);
        assert_eq!(chained.root(), wrapper_only.root());

        // Official identity present: nothing synthesized, official root wins.
        let mut official = inbound("in-session", "in-thread", Some("in-turn"));
        assert!(!official.synthesize_missing_root(Some(&synthetic_body(&["x"], &[])), &relay));
        assert_eq!(official.root(), Some("in-session"));
        assert!(!official.is_synthetic());

        // No `input`: no synthesis, still passthrough.
        let mut none = InboundCodexRuntimeIdentity::default();
        assert!(!none.synthesize_missing_root(Some(&json!({"model":"m"})), &relay));
        assert!(none.root().is_none());
    }

    #[tokio::test]
    async fn synthetic_request_materializes_official_http_shape() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let relay = header_map(&[("cafecode-uid", "u1")]);
        let mut body = synthetic_body(&["fix the tests"], &[]);
        let inbound = synthesized(&body, &relay);
        let outbound = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &scope(4, 8),
                &inbound,
                None,
                at(1_756_857_600),
            )
            .await,
        );
        assert!(is_uuid_v7(&outbound.thread_id));
        assert_eq!(outbound.session_id, outbound.thread_id);
        assert_eq!(outbound.turn_source, OutboundTurnSource::Minted);
        let turn_id = outbound.turn_id.clone().expect("turn minted");

        let mut headers = btree(&[
            (
                "user-agent",
                "codex-tui/0.150.1 (Mac OS 26.2.0; arm64) Orca/1.4.185 (codex-tui; 0.150.1)",
            ),
            ("originator", "codex-tui"),
            ("x-codex-installation-id", "inst"),
            (
                "x-client-request-id",
                "3f1c9a2e-0b7d-4c1a-9e2f-aaaaaaaaaaaa",
            ),
            ("session_id", "5f2c7e1a9b3d5c4e"),
            ("conversation_id", "5f2c7e1a9b3d5c4e"),
            ("x-codex-turn-state", "stale"),
        ]);
        apply_outbound_codex_runtime_identity(
            &mut headers,
            Some(&mut body),
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        assert_eq!(headers["session-id"], outbound.thread_id);
        assert_eq!(headers["thread-id"], outbound.thread_id);
        assert_eq!(headers["x-client-request-id"], outbound.thread_id);
        assert_eq!(
            headers["x-codex-window-id"],
            format!("{}:0", outbound.thread_id)
        );
        assert_eq!(headers["x-codex-installation-id"], "inst");
        assert!(!headers.contains_key("session_id"));
        assert!(!headers.contains_key("conversation_id"));
        assert!(!headers.contains_key("x-codex-turn-state"));
        assert!(!headers.contains_key("x-codex-beta-features"));

        let header_blob: Value = serde_json::from_str(&headers["x-codex-turn-metadata"]).unwrap();
        let keys = header_blob
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "installation_id",
                "session_id",
                "thread_id",
                "agent_name",
                "turn_id",
                "window_id",
                "window_number",
                "context_window_id",
                "request_kind",
                "root_turn_id",
                "thread_source",
                "sandbox",
                "sandbox_mode",
                "auto_review_enabled",
                "node_repl_auto_review_required",
                "node_repl_disabled",
                "turn_started_at_unix_ms",
            ]
        );
        assert_eq!(header_blob["installation_id"], "inst");
        assert_eq!(header_blob["session_id"], outbound.thread_id);
        assert_eq!(header_blob["thread_id"], outbound.thread_id);
        assert_eq!(header_blob["agent_name"], "/root");
        assert_eq!(header_blob["turn_id"], turn_id);
        assert_eq!(
            header_blob["window_id"],
            format!("{}:0", outbound.thread_id)
        );
        assert_eq!(header_blob["window_number"], 0);
        assert!(is_uuid_v7(
            header_blob["context_window_id"].as_str().unwrap()
        ));
        assert_eq!(header_blob["request_kind"], "turn");
        assert_eq!(header_blob["root_turn_id"], turn_id);
        assert_eq!(header_blob["thread_source"], "user");
        assert_eq!(header_blob["sandbox"], "seatbelt");
        assert_eq!(header_blob["sandbox_mode"], "workspace-write");
        assert_eq!(header_blob["auto_review_enabled"], false);
        assert_eq!(header_blob["node_repl_auto_review_required"], false);
        assert_eq!(header_blob["node_repl_disabled"], false);
        assert_eq!(header_blob["turn_started_at_unix_ms"], v7_millis(&turn_id));

        // Body: prompt_cache_key = session; flat metadata in official
        // `client_metadata()` key order; blob identical to the header.
        assert_eq!(body["prompt_cache_key"], outbound.thread_id);
        let meta = body["client_metadata"].as_object().unwrap();
        assert_eq!(
            meta.keys().cloned().collect::<Vec<_>>(),
            [
                "x-codex-installation-id",
                "session_id",
                "thread_id",
                "x-codex-window-id",
                "turn_id",
                "root_turn_id",
                "x-codex-turn-metadata",
            ]
        );
        assert_eq!(meta["x-codex-installation-id"], "inst");
        assert_eq!(meta["turn_id"], turn_id);
        assert_eq!(meta["root_turn_id"], turn_id);
        assert_eq!(
            meta["x-codex-turn-metadata"],
            headers["x-codex-turn-metadata"]
        );
        assert_eq!(
            body["input"].as_array().map(Vec::len),
            Some(3),
            "input untouched"
        );

        // Nothing of the relay's or Aether's markers survives.
        let serialized = format!("{headers:?}{body}");
        assert!(!serialized.contains("5f2c7e1a"));
        assert!(!serialized.contains("aaaaaaaaaaaa"));

        // Retry of the same turn: identical identity from the store.
        let again = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &scope(4, 8),
                &inbound,
                None,
                at(1_756_857_700),
            )
            .await,
        );
        assert_eq!(again.thread_id, outbound.thread_id);
        assert_eq!(again.turn_id, outbound.turn_id);

        // Linux / Windows user-agents report their own sandbox.
        let mut linux = btree(&[("user-agent", "codex-tui/0.150.1 (Ubuntu 22.4.0; x86_64)")]);
        apply_outbound_codex_runtime_identity(
            &mut linux,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        let linux_blob: Value = serde_json::from_str(&linux["x-codex-turn-metadata"]).unwrap();
        assert_eq!(linux_blob["sandbox"], "seccomp");
        assert!(
            linux_blob.get("installation_id").is_none(),
            "no header, no key"
        );
        let mut windows = btree(&[(
            "user-agent",
            "Codex Desktop/0.150.0 (Windows 10.0.26200; x86_64)",
        )]);
        apply_outbound_codex_runtime_identity(
            &mut windows,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        let windows_blob: Value = serde_json::from_str(&windows["x-codex-turn-metadata"]).unwrap();
        assert_eq!(windows_blob["sandbox"], "windows_elevated");

        // Compact / header-only surfaces never materialize a synthetic identity.
        for surface in [
            CodexRuntimeIdentitySurface::HttpCompact,
            CodexRuntimeIdentitySurface::Headers,
            CodexRuntimeIdentitySurface::WsStepBody,
        ] {
            let mut untouched = btree(&[("session_id", "5f2c7e1a9b3d5c4e")]);
            let mut untouched_body = json!({"prompt_cache_key": "keep"});
            apply_outbound_codex_runtime_identity(
                &mut untouched,
                Some(&mut untouched_body),
                &inbound,
                &outbound,
                surface,
                None,
            );
            assert_eq!(untouched["session_id"], "5f2c7e1a9b3d5c4e", "{surface:?}");
            assert_eq!(untouched_body["prompt_cache_key"], "keep", "{surface:?}");
        }
    }

    #[test]
    fn http_rewrite_inserts_missing_official_headers_on_responses_and_compact() {
        let inbound = inbound("in-session", "in-thread", Some("in-turn"));
        let outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Frozen);
        // A relay stripped session-id / thread-id / x-client-request-id /
        // window in front of a real client but kept the blob.
        let blob = r#"{"session_id":"in-session","thread_id":"in-thread","turn_id":"in-turn","window_id":"in-thread:3","request_kind":"turn"}"#;
        let mut headers = btree(&[("x-codex-turn-metadata", blob)]);
        apply_outbound_codex_runtime_identity(
            &mut headers,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::HttpResponses,
            None,
        );
        assert_eq!(headers["session-id"], "out-thread");
        assert_eq!(headers["thread-id"], "out-thread");
        assert_eq!(headers["x-client-request-id"], "out-thread");
        assert_eq!(headers["x-codex-window-id"], "out-thread:0");

        // Header-only surfaces (search / chat / image) keep rewrite-only
        // semantics.
        let mut search = btree(&[("x-codex-turn-metadata", blob)]);
        apply_outbound_codex_runtime_identity(
            &mut search,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::Headers,
            None,
        );
        assert!(!search.contains_key("session-id"));
        assert!(!search.contains_key("x-client-request-id"));

        // Official compact carries session-id / thread-id / x-codex-window-id
        // but never x-client-request-id; Aether's filler writes the request id
        // there, so it is dropped rather than rewritten.
        let mut compact = btree(&[
            ("thread-id", "in-thread"),
            (
                "x-client-request-id",
                "3f1c9a2e-0b7d-4c1a-9e2f-bbbbbbbbbbbb",
            ),
        ]);
        apply_outbound_codex_runtime_identity(
            &mut compact,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::HttpCompact,
            None,
        );
        assert_eq!(compact["thread-id"], "out-thread");
        assert_eq!(compact["session-id"], "out-thread");
        assert_eq!(compact["x-codex-window-id"], "out-thread:0");
        assert!(!compact.contains_key("x-client-request-id"));
    }

    #[test]
    fn synthetic_prompt_extraction_skips_wrappers_and_reads_string_forms() {
        let prompts = real_user_prompts(&json!([
            {"role":"user","content":"<user_instructions>\nx\n</user_instructions>"},
            {"type":"message","role":"developer","content":"not a user"},
            {"type":"message","role":"user","content":"  plain string prompt  "},
            {"type":"function_call_output","call_id":"c","output":"ignored"},
            {"type":"message","role":"user","content":[{"type":"input_image","image_url":"data:"},{"type":"input_text","text":"with image"}]},
            {"type":"message","role":"user","content":[{"type":"input_text","text":""}]},
            {"type":"message","role":"user","content":"<turn_aborted>\nstop\n</turn_aborted>"},
            {"type":"message","role":"user","content":"<not-a-wrapper> because of the dash"}
        ]));
        assert_eq!(
            prompts,
            vec![
                (2, "plain string prompt".to_string()),
                (4, "with image".to_string()),
                (7, "<not-a-wrapper> because of the dash".to_string()),
            ]
        );
        assert_eq!(real_user_prompts(&json!("hi")), vec![(0, "hi".to_string())]);
        assert!(real_user_prompts(&json!(42)).is_empty());
        assert_eq!(uuid_v7_unix_millis("not-a-uuid"), None);
        assert_eq!(
            uuid_v7_unix_millis("3f1c9a2e-0b7d-4c1a-9e2f-aaaaaaaaaaaa"),
            None,
            "v4 has no timestamp"
        );
    }

    #[tokio::test]
    async fn thread_window_advances_on_compaction_and_mints_context_lazily() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(1, 64);
        let t0 = at(1_756_857_600);

        // Window 0: context id minted with the first request on the thread.
        let first = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u1")),
                None,
                t0,
            )
            .await,
        );
        let thread = first.thread_id.clone();
        assert_eq!(first.window_number, 0);
        assert_eq!(first.window_id, format!("{thread}:0"));
        let ctx0 = first.context_window_id.clone().expect("context minted");
        assert!(is_uuid_v7(&ctx0));
        assert_eq!(v7_millis(&ctx0), 1_756_857_600_000);
        assert_ne!(ctx0, thread);

        // Stable across requests of the same window.
        let again = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u2")),
                None,
                at(1_756_857_650),
            )
            .await,
        );
        assert_eq!(again.thread_id, thread);
        assert_eq!(again.window_number, 0);
        assert_eq!(again.context_window_id.as_deref(), Some(ctx0.as_str()));

        // The compaction request itself still carries window 0 ...
        let mut compaction = inbound("s", "t", Some("u3"));
        compaction.request_kind = Some(CodexRequestKind::Compaction);
        let during = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &compaction,
                None,
                at(1_756_857_700),
            )
            .await,
        );
        assert_eq!(during.window_number, 0);
        assert_eq!(during.window_id, format!("{thread}:0"));
        assert_eq!(during.context_window_id.as_deref(), Some(ctx0.as_str()));
        // ... and leaves the thread on window 1 without a context id yet.
        let stored = state.kv_get(&s.window_key(&thread)).await.unwrap().unwrap();
        assert_eq!(
            ThreadWindow::parse(&stored).unwrap(),
            ThreadWindow {
                number: 1,
                context_window_id: None
            }
        );

        // Next request on the thread: window 1, new context id timestamped now.
        let after = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u4")),
                None,
                at(1_756_857_760),
            )
            .await,
        );
        assert_eq!(after.window_number, 1);
        assert_eq!(after.window_id, format!("{thread}:1"));
        let ctx1 = after.context_window_id.clone().expect("context minted");
        assert!(is_uuid_v7(&ctx1));
        assert_ne!(ctx1, ctx0);
        assert_eq!(v7_millis(&ctx1), 1_756_857_760_000);

        // Another real session folded onto the same thread sees the same window
        // (one thread, one window), and a memory request projects `:0` anyway.
        let other = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s2", "t2", Some("v1")),
                None,
                at(1_756_857_800),
            )
            .await,
        );
        assert_eq!(other.thread_id, thread);
        assert_eq!(other.window_number, 1);
        assert_eq!(other.context_window_id.as_deref(), Some(ctx1.as_str()));
        let mut memory = inbound("s", "t", None);
        memory.request_kind = Some(CodexRequestKind::Memory);
        let memory = rewrite(
            resolve_outbound_codex_runtime_identity(&store, &s, &memory, None, at(1_756_857_810))
                .await,
        );
        assert_eq!(memory.window_number, 1);
        assert_eq!(memory.window_id, format!("{thread}:0"));
        assert_eq!(memory.turn_id, None);

        // WS snapshot taken at window 0 keeps session/thread but reads the live
        // window per step; a store outage falls back to the snapshot's window.
        let step = rewrite(
            resolve_outbound_codex_runtime_identity(
                &store,
                &s,
                &inbound("s", "t", Some("u1")),
                Some(&first),
                at(1_756_857_820),
            )
            .await,
        );
        assert_eq!(step.thread_id, thread);
        assert_eq!(step.window_number, 1);
        assert_eq!(step.window_id, format!("{thread}:1"));
        assert_eq!(step.turn_source, OutboundTurnSource::Snapshot);
        let unavailable = CodexRuntimeIdentityStore::unavailable(&state);
        let degraded = rewrite(
            resolve_outbound_codex_runtime_identity(
                &unavailable,
                &s,
                &inbound("s", "t", Some("u1")),
                Some(&first),
                at(1_756_857_830),
            )
            .await,
        );
        assert_eq!(degraded.window_number, 0);
        assert_eq!(degraded.window_id, format!("{thread}:0"));
        assert_eq!(degraded.context_window_id.as_deref(), Some(ctx0.as_str()));
    }

    #[test]
    fn surface_headers_leaves_body_alone_and_ws_body_leaves_headers_alone() {
        let inbound = inbound("in-session", "in-thread", Some("in-turn"));
        let outbound = outbound_fixture(Some("out-turn"), OutboundTurnSource::Frozen);
        let mut body = json!({ "prompt_cache_key": "in-session" });
        let mut headers = btree(&[("session-id", "in-session")]);
        apply_outbound_codex_runtime_identity(
            &mut headers,
            Some(&mut body),
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::Headers,
            None,
        );
        assert_eq!(body["prompt_cache_key"], "in-session");
        assert_eq!(headers["session-id"], "out-thread");

        let mut body = json!({ "prompt_cache_key": "in-session" });
        let mut headers = btree(&[("session-id", "in-session")]);
        apply_outbound_codex_runtime_identity(
            &mut headers,
            Some(&mut body),
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::WsStepBody,
            None,
        );
        assert_eq!(body["prompt_cache_key"], "out-thread");
        assert_eq!(headers["session-id"], "in-session");
    }
}
