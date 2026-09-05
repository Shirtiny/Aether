//! Codex pool outbound runtime identity synthesis.
//!
//! Inbound official Codex identity (`session_id` / `thread_id` / `turn_id` /
//! `window_id`) keeps owning sticky routing, WebSocket binding and usage
//! settlement. When `pool_advanced.codex_runtime_identity.enabled` is `true`
//! on a Codex pool provider, the identity that the *selected account* shows
//! upstream is replaced, after key selection, by a per-account, per-day
//! synthetic tree negotiated through the shared runtime state:
//!
//! - N thread slots per account per day window, M turn slots per synthetic
//!   thread per day window (`expected_threads_per_day` / `expected_turns_per_day`)
//! - the same inbound root keeps the same outbound thread across HTTP compact,
//!   Search, WebSocket reconnects and day rollovers while its freeze is alive
//! - the same inbound turn keeps the same outbound turn while its freeze is alive
//! - all UUIDs are UUIDv7 minted at first binding through `SET NX`
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
const TTL_GRACE_SECS: u64 = 43_200;

const SELECTION_FP_DOMAIN: &[u8] = b"aether:codex:rid:sel:v1";
const JITTER_DOMAIN: &[u8] = b"aether:codex:rid:jitter:v1";
const THREAD_SLOT_DOMAIN: &[u8] = b"aether:codex:rid:thread:v1";
const TURN_SLOT_DOMAIN: &[u8] = b"aether:codex:rid:turn:v1";

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
/// `root_turn_id` → the outbound turn (root turns are their own root).
const BLOB_NORMALIZED_KEYS: &[&str] = &["agent_name", "thread_source", "root_turn_id"];
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
/// Desktop `workspace_kind` extra observed in production).
const BLOB_PASS_KEYS: &[&str] = &[
    "request_kind",
    "compaction",
    "turn_trigger",
    "sandbox",
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
const COMPACT_SUMMARY_PREFIX: &str = "Another language model started to solve this problem";
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

    /// Remaining seconds in the jittered day window plus a 12h grace.
    pub(crate) fn ttl(&self, now: SystemTime) -> Duration {
        let remaining = DAY_WINDOW_SECS - (self.shifted_secs(now) % DAY_WINDOW_SECS);
        Duration::from_secs(remaining + TTL_GRACE_SECS)
    }

    fn thread_slot(&self, day_id: u64, inbound_root: &str) -> u64 {
        let digest = sha256(&[
            THREAD_SLOT_DOMAIN,
            b"\0",
            self.selection_key.as_bytes(),
            b"\0",
            day_id.to_string().as_bytes(),
            b"\0",
            inbound_root.as_bytes(),
        ]);
        u64_prefix(&digest) % u64::from(self.config.expected_threads_per_day)
    }

    fn turn_slot(&self, day_id: u64, outbound_thread_id: &str, inbound_turn_key: &str) -> u64 {
        let digest = sha256(&[
            TURN_SLOT_DOMAIN,
            b"\0",
            self.selection_key.as_bytes(),
            b"\0",
            day_id.to_string().as_bytes(),
            b"\0",
            outbound_thread_id.as_bytes(),
            b"\0",
            inbound_turn_key.as_bytes(),
        ]);
        u64_prefix(&digest) % u64::from(self.config.expected_turns_per_day)
    }

    fn key_prefix(&self) -> String {
        format!("ap:{}:codex_rid:{}", self.provider_id, self.selection_fp)
    }

    fn thread_slot_key(&self, day_id: u64, slot: u64) -> String {
        format!("{}:{day_id}:thread:{slot}", self.key_prefix())
    }

    fn turn_slot_key(&self, day_id: u64, outbound_thread_id: &str, slot: u64) -> String {
        format!(
            "{}:{day_id}:turn:{outbound_thread_id}:{slot}",
            self.key_prefix()
        )
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
        self.official_root()
            .or_else(|| self.synthetic.as_ref().map(|synthetic| synthetic.root.as_str()))
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
            (Some((_, first)), Some((last_index, last)))
                if !self.previous_response_id_present =>
            {
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
    /// Read from the per-turn freeze (or written by a concurrent peer).
    Frozen,
    /// Freshly bound to a turn slot by this request.
    Minted,
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
    /// earlier request. Forward it only when this request's outbound turn is
    /// the same one; never attach it to a freshly minted turn.
    pub(crate) fn forwards_turn_state(&self) -> bool {
        !matches!(self.turn_source, OutboundTurnSource::Minted)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_inbound_turn_hash: Option<String>,
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

    /// Single-key `SET NX`; the loser reads the winner's value back.
    async fn get_or_mint(
        &self,
        key: &str,
        ttl: Duration,
        now: SystemTime,
    ) -> Result<String, String> {
        if let Some(existing) = self.get(key).await? {
            return Ok(existing);
        }
        let minted = uuid_v7_at(unix_millis(now));
        if self.set_if_absent(key, &minted, ttl).await? {
            return Ok(minted);
        }
        Ok(self.get(key).await?.unwrap_or(minted))
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
    let ttl = scope.ttl(now);
    let root_hash = hash16(root);

    if let Some(snapshot) = snapshot {
        let (turn_id, turn_source) = match turn_key {
            None => (None, OutboundTurnSource::None),
            Some(key)
                if snapshot.turn_id.is_some()
                    && snapshot.inbound_turn_key.as_deref() == Some(key) =>
            {
                (snapshot.turn_id.clone(), OutboundTurnSource::Snapshot)
            }
            Some(key) => {
                resolve_turn(
                    store,
                    scope,
                    &root_hash,
                    &snapshot.thread_id,
                    key,
                    day_id,
                    ttl,
                    now,
                    None,
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
        Some(raw) => RootFreeze::parse(&raw).map(|freeze| (raw, freeze)),
        None => None,
    };
    if let Some((raw, _)) = frozen.as_ref() {
        // Sliding TTL: an active session never changes thread mid-flight.
        let _ = store.expire_if_value(&freeze_key, raw, ttl).await?;
    }
    if frozen.is_none() {
        if chained {
            debug!(
                event_name = "codex_rid_chain_freeze_miss",
                log_type = "event",
                provider_id = %scope.provider_id,
                selection_fp = %scope.selection_fp,
                inbound_root_hash = %root_hash,
                "chained request has no root freeze; minting by slot"
            );
        }
        let slot = scope.thread_slot(day_id, root);
        let thread_id = store
            .get_or_mint(&scope.thread_slot_key(day_id, slot), ttl, now)
            .await?;
        let fresh = RootFreeze {
            session_id: thread_id.clone(),
            thread_id: thread_id.clone(),
            window_id: format!("{thread_id}:0"),
            day_id,
            last_turn_id: None,
            last_inbound_turn_hash: None,
        };
        let raw = fresh.to_json();
        frozen = if store.set_if_absent(&freeze_key, &raw, ttl).await? {
            Some((raw, fresh))
        } else {
            match store.get(&freeze_key).await? {
                Some(existing) => RootFreeze::parse(&existing)
                    .map(|freeze| (existing, freeze))
                    .or(Some((raw, fresh))),
                None => Some((raw, fresh)),
            }
        };
    }
    let (raw_freeze, freeze) = frozen.expect("root freeze resolved above");

    let (turn_id, turn_source) = match turn_key {
        None => (None, OutboundTurnSource::None),
        Some(key) => {
            resolve_turn(
                store,
                scope,
                &root_hash,
                &freeze.thread_id,
                key,
                day_id,
                ttl,
                now,
                Some((&freeze_key, &raw_freeze, &freeze, chained)),
            )
            .await?
        }
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

/// Reads (and lazily initializes) the synthetic thread's context window, then
/// advances it when this request is a compaction.
///
/// * A window whose `context_window_id` is still unset (thread just minted, or
///   the previous request was a compaction) mints one `now`: like the real
///   client, the new window id is timestamped after the compaction finished,
///   not together with the thread.
/// * The compaction request itself still carries the current window; the
///   advance is a compare-and-set to `number + 1` with no context id, so the
///   next request on the thread mints it. A concurrent writer wins silently.
/// * Sliding TTL like the root freeze; an active thread never regresses.
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

#[allow(clippy::too_many_arguments)]
async fn resolve_turn(
    store: &CodexRuntimeIdentityStore<'_>,
    scope: &CodexRuntimeIdentityScope,
    root_hash: &str,
    outbound_thread_id: &str,
    inbound_turn_key: &str,
    day_id: u64,
    ttl: Duration,
    now: SystemTime,
    root_freeze: Option<(&str, &str, &RootFreeze, bool)>,
) -> Result<(Option<String>, OutboundTurnSource), String> {
    let turn_hash = hash16(inbound_turn_key);
    let turn_freeze_key = scope.turn_freeze_key(root_hash, &turn_hash);
    if let Some(existing) = store.get(&turn_freeze_key).await? {
        let _ = store
            .expire_if_value(&turn_freeze_key, &existing, ttl)
            .await?;
        return Ok((Some(existing), OutboundTurnSource::Frozen));
    }

    // A chained (`previous_response_id`) request whose per-turn freeze is gone
    // continues the last turn recorded on its root instead of minting.
    if let Some((_, _, freeze, true)) = root_freeze {
        if let Some(last_turn_id) = freeze.last_turn_id.as_deref() {
            let _ = store
                .set_if_absent(&turn_freeze_key, last_turn_id, ttl)
                .await?;
            return Ok((Some(last_turn_id.to_string()), OutboundTurnSource::Frozen));
        }
    }

    let slot = scope.turn_slot(day_id, outbound_thread_id, inbound_turn_key);
    let minted = store
        .get_or_mint(
            &scope.turn_slot_key(day_id, outbound_thread_id, slot),
            ttl,
            now,
        )
        .await?;
    let (turn_id, turn_source) = if store.set_if_absent(&turn_freeze_key, &minted, ttl).await? {
        (minted, OutboundTurnSource::Minted)
    } else {
        match store.get(&turn_freeze_key).await? {
            Some(existing) => (existing, OutboundTurnSource::Frozen),
            None => (minted, OutboundTurnSource::Minted),
        }
    };

    if let Some((freeze_key, raw_freeze, freeze, _)) = root_freeze {
        let mut updated = freeze.clone();
        updated.last_turn_id = Some(turn_id.clone());
        updated.last_inbound_turn_hash = Some(turn_hash);
        // Best effort compare-and-set; a concurrent writer wins silently.
        let _ = store
            .set_if_value(freeze_key, raw_freeze, &updated.to_json(), ttl)
            .await?;
    }
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
/// Only existing keys are rewritten; missing keys are never added (the
/// official blob shape depends on `request_kind`), except the headers an
/// official HTTP `/responses` client sends unconditionally. Inbound-tree keys
/// (parent / fork / subagent) are removed, `agent_name` / `thread_source` /
/// `root_turn_id` are normalized to the root user-thread shape, and any key
/// outside the per-surface whitelist is removed and reported.
///
/// A synthetic inbound (no official identity) has nothing to rewrite: the
/// HTTP `/responses` surface materializes the full official shape instead.
pub(crate) fn apply_outbound_codex_runtime_identity(
    headers: &mut BTreeMap<String, String>,
    body: Option<&mut Value>,
    inbound: &InboundCodexRuntimeIdentity,
    outbound: &OutboundCodexRuntimeIdentity,
    surface: CodexRuntimeIdentitySurface,
) {
    if inbound.is_synthetic() {
        if surface == CodexRuntimeIdentitySurface::HttpResponses {
            materialize_http_responses(headers, body, outbound);
        }
        return;
    }
    if surface != CodexRuntimeIdentitySurface::WsStepBody {
        rewrite_headers(headers, inbound, outbound, surface);
    }
    if surface != CodexRuntimeIdentitySurface::Headers {
        if let Some(body) = body {
            rewrite_body(body, inbound, outbound);
        }
    }
}

fn rewrite_headers(
    headers: &mut BTreeMap<String, String>,
    inbound: &InboundCodexRuntimeIdentity,
    outbound: &OutboundCodexRuntimeIdentity,
    surface: CodexRuntimeIdentitySurface,
) {
    // Official HTTP `/responses` clients send session-id, thread-id,
    // x-codex-window-id and x-client-request-id unconditionally (codex-api
    // `build_session_headers`, `endpoint/responses.rs`,
    // `compatibility_headers`). A relay that strips them in front of a real
    // client (prod v0.7.104: the dominant downstream) leaves a shape no client
    // produces, so on that surface missing ones are inserted; every other
    // surface only rewrites what is present.
    let insert_missing = surface == CodexRuntimeIdentitySurface::HttpResponses;
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
    // Official HTTP and WS clients always send x-client-request-id = thread_id
    // (codex-api endpoint/responses.rs, core client.rs). Anything else here is
    // the Aether request id or a relay trace id: a per-request random value no
    // real client produces (prod v0.7.104: 188/188 requests).
    project_header(
        headers,
        X_CLIENT_REQUEST_ID,
        |_| true,
        &outbound.thread_id,
        insert_missing,
    );
    if let Some((name, raw)) = header_entry(headers, X_CODEX_TURN_METADATA) {
        if let Some(rewritten) = rewrite_codex_turn_metadata_string(&raw, outbound) {
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
        rewrite_codex_turn_metadata_value(blob, outbound);
    }
    if !outbound.forwards_turn_state() {
        client_metadata.remove(X_CODEX_TURN_STATE);
    }
    retain_known_keys(client_metadata, "client_metadata", flat_key_known);
}

/// Rewrites a serialized `x-codex-turn-metadata` blob (header or
/// `client_metadata` string). Returns `None` when it is not a JSON object.
pub(crate) fn rewrite_codex_turn_metadata_string(
    raw: &str,
    outbound: &OutboundCodexRuntimeIdentity,
) -> Option<String> {
    let mut parsed = serde_json::from_str::<Value>(raw).ok()?;
    let object = parsed.as_object_mut()?;
    rewrite_codex_turn_metadata_object(object, outbound);
    // Embedded in an HTTP header: keep every byte ASCII.
    serialize_ascii_json(&parsed)
}

fn rewrite_codex_turn_metadata_value(blob: &mut Value, outbound: &OutboundCodexRuntimeIdentity) {
    match blob {
        Value::String(raw) => {
            if let Some(rewritten) = rewrite_codex_turn_metadata_string(raw, outbound) {
                *raw = rewritten;
            }
        }
        Value::Object(object) => rewrite_codex_turn_metadata_object(object, outbound),
        _ => {}
    }
}

fn rewrite_codex_turn_metadata_object(
    object: &mut Map<String, Value>,
    outbound: &OutboundCodexRuntimeIdentity,
) {
    let memory = non_empty_str(object.get("request_kind")).map(CodexRequestKind::parse)
        == Some(CodexRequestKind::Memory);
    if memory {
        // Official memory blobs carry no installation/session/thread/turn/
        // root_turn/window/window_number/context_window_id.
        for key in BLOB_IDENTITY_KEYS {
            object.remove(*key);
        }
        object.remove("root_turn_id");
    } else {
        set_if_present(object, "session_id", &outbound.session_id);
        set_if_present(object, "thread_id", &outbound.thread_id);
        set_if_present(object, "window_id", &outbound.window_id);
        // codex-tui >= 0.153: `window_id == "{thread}:{window_number}"` and one
        // `context_window_id` per (thread, window). Both follow the synthetic
        // thread's own window state; the inbound values never pass through.
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
        match outbound.turn_id.as_deref() {
            Some(turn_id) => set_if_present(object, "turn_id", turn_id),
            None => {
                object.remove("turn_id");
            }
        }
        // Folded subagent / feature threads present as the root user thread,
        // whose root turn is the turn itself.
        set_if_present(object, "agent_name", ROOT_AGENT_NAME);
        set_if_present(object, "thread_source", USER_THREAD_SOURCE);
        match outbound.turn_id.as_deref() {
            Some(turn_id) => set_if_present(object, "root_turn_id", turn_id),
            None => {
                object.remove("root_turn_id");
            }
        }
    }
    for key in BLOB_LEAK_KEYS {
        object.remove(*key);
    }
    retain_known_keys(object, "turn_metadata", blob_key_known);
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
fn message_text(content: &Value) -> Option<String> {
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
fn prompt_text(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty()
        || text.starts_with(COMPACT_SUMMARY_PREFIX)
        || starts_with_wrapper_tag(text)
    {
        return None;
    }
    Some(text.to_string())
}

fn starts_with_wrapper_tag(text: &str) -> bool {
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

/// `sandbox` / `sandbox_mode` a default-configured client reports on the OS
/// of the outbound user-agent (codex-rs `core/src/sandbox_tags.rs`,
/// `sandboxing/src/manager.rs`): seatbelt on macOS, the elevated Windows
/// sandbox on Windows, seccomp elsewhere, all in the default workspace-write
/// policy.
fn sandbox_tags_for_user_agent(user_agent: Option<&str>) -> (&'static str, &'static str) {
    match user_agent {
        Some(agent) if agent.contains("Mac OS") => ("seatbelt", "workspace-write"),
        Some(agent) if agent.contains("Windows") => ("windows_elevated", "workspace-write"),
        _ => ("seccomp", "workspace-write"),
    }
}

fn synthetic_turn_metadata_json(
    outbound: &OutboundCodexRuntimeIdentity,
    installation_id: Option<&str>,
    user_agent: Option<&str>,
) -> Option<String> {
    let (sandbox, sandbox_mode) = sandbox_tags_for_user_agent(user_agent);
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
        turn_started_at_unix_ms: outbound
            .turn_id
            .as_deref()
            .and_then(uuid_v7_unix_millis),
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
) {
    let installation_id = header_entry(headers, X_CODEX_INSTALLATION_ID).map(|(_, value)| value);
    let user_agent = header_entry(headers, USER_AGENT_HEADER).map(|(_, value)| value);
    let blob = synthetic_turn_metadata_json(
        outbound,
        installation_id.as_deref(),
        user_agent.as_deref(),
    );

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
fn uuid_v7_from_parts(unix_ms: u64, random: &[u8; 16]) -> String {
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&unix_ms.to_be_bytes()[2..8]);
    bytes[6] = 0x70 | (random[6] & 0x0F);
    // Clear bits 2-3: the ContextV7 counter gap that real `now_v7()` always leaves.
    bytes[7] = random[7] & 0xF3;
    bytes[8] = 0x80 | (random[8] & 0x3F);
    bytes[9..].copy_from_slice(&random[9..]);
    Uuid::from_bytes(bytes).hyphenated().to_string()
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
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

/// `hex(SHA256(value)[0..16])`; inbound IDs never enter keys or logs verbatim.
pub(crate) fn hash16(value: &str) -> String {
    hex_lower(&sha256(&[value.as_bytes()])[..16])
}

fn unix_secs(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn unix_millis(now: SystemTime) -> u64 {
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
fn uuid_v7_unix_millis(id: &str) -> Option<u64> {
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
        // Keys never contain the raw selection key.
        assert!(!a.thread_slot_key(1, 0).contains(SELECTION));
        assert!(a.thread_slot_key(1, 0).starts_with("ap:prov-1:codex_rid:"));
    }

    #[test]
    fn scope_ttl_covers_rest_of_window_plus_grace() {
        let s = scope(4, 8);
        let now = at(1_756_857_600);
        let ttl = s.ttl(now).as_secs();
        assert!(ttl > TTL_GRACE_SECS && ttl <= DAY_WINDOW_SECS + TTL_GRACE_SECS);
        let day = s.day_id(now);
        let later = at(1_756_857_600 + ttl - TTL_GRACE_SECS);
        assert_eq!(s.day_id(later), day + 1);
        assert_eq!(s.day_id(at(1_756_857_600 + ttl - TTL_GRACE_SECS - 1)), day);
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
    async fn thread_slots_bound_distinct_roots_and_stay_stable() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(3, 64);
        let now = at(1_756_857_600);
        let mut threads = HashSet::new();
        let mut first = Vec::new();
        for i in 0..40 {
            let inbound = inbound(&format!("s{i}"), &format!("t{i}"), Some(&format!("u{i}")));
            let out = rewrite(
                resolve_outbound_codex_runtime_identity(&store, &s, &inbound, None, now).await,
            );
            assert!(is_uuid_v7(&out.thread_id), "{}", out.thread_id);
            assert_eq!(out.session_id, out.thread_id);
            assert_eq!(out.window_id, format!("{}:0", out.thread_id));
            assert!(out.turn_id.as_deref().is_some_and(is_uuid_v7));
            assert_eq!(out.turn_source, OutboundTurnSource::Minted);
            threads.insert(out.thread_id.clone());
            first.push(out);
        }
        assert!(threads.len() <= 3, "{} threads", threads.len());
        assert!(threads.len() > 1, "expected spread over slots");
        // Stability: same inbound → same outbound (thread and turn), now Frozen.
        for (i, previous) in first.iter().enumerate() {
            let inbound = inbound(&format!("s{i}"), &format!("t{i}"), Some(&format!("u{i}")));
            let again = rewrite(
                resolve_outbound_codex_runtime_identity(
                    &store,
                    &s,
                    &inbound,
                    None,
                    now + Duration::from_secs(60),
                )
                .await,
            );
            assert_eq!(again.thread_id, previous.thread_id);
            assert_eq!(again.turn_id, previous.turn_id);
            assert_eq!(again.turn_source, OutboundTurnSource::Frozen);
        }
    }

    #[tokio::test]
    async fn turn_slots_bound_per_thread_and_never_cross_threads() {
        let state = memory_state();
        let store = CodexRuntimeIdentityStore::new(&state);
        let s = scope(2, 4);
        let now = at(1_756_857_600);
        let mut turns_by_thread: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        let mut thread_by_turn: BTreeMap<String, String> = BTreeMap::new();
        for i in 0..30 {
            for j in 0..6 {
                let inbound = inbound(
                    &format!("s{i}"),
                    &format!("t{i}"),
                    Some(&format!("u{i}-{j}")),
                );
                let out = rewrite(
                    resolve_outbound_codex_runtime_identity(&store, &s, &inbound, None, now).await,
                );
                let turn = out.turn_id.clone().unwrap();
                turns_by_thread
                    .entry(out.thread_id.clone())
                    .or_default()
                    .insert(turn.clone());
                let owner = thread_by_turn
                    .entry(turn)
                    .or_insert_with(|| out.thread_id.clone());
                assert_eq!(owner, &out.thread_id, "turn shared across threads");
            }
        }
        assert!(turns_by_thread.len() <= 2);
        for (thread, turns) in &turns_by_thread {
            assert!(
                turns.len() <= 4,
                "thread {thread} has {} turns",
                turns.len()
            );
        }
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
        // Next day: same inbound root/turn keeps both IDs.
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
    async fn chained_request_reuses_last_turn_when_turn_freeze_missing() {
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
        let mut chained = inbound("s", "t", Some("u-unknown"));
        chained.previous_response_id_present = true;
        let second =
            rewrite(resolve_outbound_codex_runtime_identity(&store, &s, &chained, None, now).await);
        assert_eq!(second.thread_id, first.thread_id);
        assert_eq!(second.turn_id, first.turn_id);
        assert_eq!(second.turn_source, OutboundTurnSource::Frozen);
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
    async fn ws_snapshot_keeps_thread_and_turn_for_same_inbound_turn() {
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
        // Same inbound turn: no store needed, identical IDs.
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
        // New inbound turn: thread from snapshot, turn minted under it.
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
                &inbound("other", "t2", Some("u1")),
                Some(&snapshot),
                now,
            )
            .await,
        );
        assert_eq!(other.inbound_root, "other");
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
                    "sandbox": "workspace-write",
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
        assert_eq!(blob["sandbox"], "workspace-write");
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
        )
        .unwrap();
        let blob: Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(
            blob,
            json!({ "session_id": "out-thread", "thread_id": "out-thread", "turn_id": "out-turn" })
        );
        assert_eq!(
            rewrite_codex_turn_metadata_string("not json", &outbound),
            None
        );
        assert_eq!(rewrite_codex_turn_metadata_string("[1]", &outbound), None);
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
        assert_eq!(blob["workspace_kind"], "project", "known extra kept");
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
        )
        .unwrap();
        let degraded: Value = serde_json::from_str(&degraded).unwrap();
        assert_eq!(degraded["window_number"], 3);
        assert!(degraded.get("context_window_id").is_none());

        // Never added when absent (older clients).
        let older = rewrite_codex_turn_metadata_string(
            r#"{"session_id":"in","thread_id":"in","window_id":"in:2"}"#,
            &outbound,
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
        )
        .unwrap();
        let memory: Value = serde_json::from_str(&memory).unwrap();
        assert_eq!(memory, json!({ "request_kind": "memory" }));
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
        assert_eq!(
            blob,
            json!({ "thread_id": "out-thread", "request_kind": "turn" })
        );
        assert_eq!(headers["x-codex-beta-features"], "a,b");
        assert_eq!(headers["x-codex-routing-hint"], "model=gpt-5;tier=x");
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
        let turn2 = synthesized(&synthetic_body(&["fix the tests", "now the docs"], &[]), &relay);
        assert!(turn1.is_synthetic());
        assert!(turn1.root().is_some() && turn1.turn_key().is_some());
        assert_eq!(turn1.root(), turn1_followup.root());
        assert_eq!(turn1.turn_key(), turn1_followup.turn_key(), "tool follow-up is the same turn");
        assert_eq!(turn1.root(), turn2.root(), "same conversation keeps the thread");
        assert_ne!(turn1.turn_key(), turn2.turn_key(), "a new prompt is a new turn");
        assert_eq!(turn1.root().map(str::len), Some(32), "16-byte hex, no prompt text");

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
            ("x-client-request-id", "3f1c9a2e-0b7d-4c1a-9e2f-aaaaaaaaaaaa"),
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
        );
        assert_eq!(headers["session-id"], outbound.thread_id);
        assert_eq!(headers["thread-id"], outbound.thread_id);
        assert_eq!(headers["x-client-request-id"], outbound.thread_id);
        assert_eq!(headers["x-codex-window-id"], format!("{}:0", outbound.thread_id));
        assert_eq!(headers["x-codex-installation-id"], "inst");
        assert!(!headers.contains_key("session_id"));
        assert!(!headers.contains_key("conversation_id"));
        assert!(!headers.contains_key("x-codex-turn-state"));
        assert!(!headers.contains_key("x-codex-beta-features"));

        let header_blob: Value = serde_json::from_str(&headers["x-codex-turn-metadata"]).unwrap();
        let keys = header_blob.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
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
        assert_eq!(header_blob["window_id"], format!("{}:0", outbound.thread_id));
        assert_eq!(header_blob["window_number"], 0);
        assert!(is_uuid_v7(header_blob["context_window_id"].as_str().unwrap()));
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
        assert_eq!(meta["x-codex-turn-metadata"], headers["x-codex-turn-metadata"]);
        assert_eq!(body["input"].as_array().map(Vec::len), Some(3), "input untouched");

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
        );
        let linux_blob: Value = serde_json::from_str(&linux["x-codex-turn-metadata"]).unwrap();
        assert_eq!(linux_blob["sandbox"], "seccomp");
        assert!(linux_blob.get("installation_id").is_none(), "no header, no key");
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
            );
            assert_eq!(untouched["session_id"], "5f2c7e1a9b3d5c4e", "{surface:?}");
            assert_eq!(untouched_body["prompt_cache_key"], "keep", "{surface:?}");
        }
    }

    #[test]
    fn http_rewrite_inserts_missing_official_headers_only_on_responses_surface() {
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
        );
        assert_eq!(headers["session-id"], "out-thread");
        assert_eq!(headers["thread-id"], "out-thread");
        assert_eq!(headers["x-client-request-id"], "out-thread");
        assert_eq!(headers["x-codex-window-id"], "out-thread:0");

        // Header-only surfaces (search / chat / image) and compact keep
        // rewrite-only semantics.
        let mut search = btree(&[("x-codex-turn-metadata", blob)]);
        apply_outbound_codex_runtime_identity(
            &mut search,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::Headers,
        );
        assert!(!search.contains_key("session-id"));
        assert!(!search.contains_key("x-client-request-id"));
        let mut compact = btree(&[("thread-id", "in-thread")]);
        apply_outbound_codex_runtime_identity(
            &mut compact,
            None,
            &inbound,
            &outbound,
            CodexRuntimeIdentitySurface::HttpCompact,
        );
        assert_eq!(compact["thread-id"], "out-thread");
        assert!(!compact.contains_key("session-id"));
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
        );
        assert_eq!(body["prompt_cache_key"], "out-thread");
        assert_eq!(headers["session-id"], "in-session");
    }
}
