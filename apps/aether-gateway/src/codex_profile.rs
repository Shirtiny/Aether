use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Offset, TimeZone};
use chrono_tz::Tz;

use aether_contracts::{
    codex_default_transport_profile_extra, CODEX_DEFAULT_TLS_JA3, CODEX_DEFAULT_TLS_JA3_HASH,
    TRANSPORT_BACKEND_REQWEST_DEFAULT_TLS, TRANSPORT_HTTP_MODE_AUTO, TRANSPORT_POOL_SCOPE_KEY,
    TRANSPORT_PROFILE_CODEX_LEGACY_REQWEST_RUSTLS_AUTO,
    TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::codex_environment_context::process_environment_timezone;
use crate::codex_runtime_identity::OutboundClientOs;

pub(crate) const CODEX_CLIENT_PROFILE_KEY: &str = "codex_client_profile";
pub(crate) const CODEX_TRANSPORT_PROFILE_KEY: &str = "transport_profile";
const CODEX_PROFILE_SCHEMA_VERSION: u64 = 1;
const X_CODEX_INSTALLATION_ID: &str = "x-codex-installation-id";
const X_CODEX_TURN_METADATA: &str = "x-codex-turn-metadata";
/// codex-rs sends its build version in this header on every OpenAI request.
const CLIENT_VERSION_HEADER: &str = "version";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexConcreteAccountProfile {
    pub(crate) user_agent: String,
    pub(crate) originator: String,
    pub(crate) installation_id: String,
    pub(crate) workspace_identity: CodexWorkspaceIdentity,
    pub(crate) fingerprint_hash: String,
}

/// The developer one pool account presents behind the `workspaces` map of the
/// turn metadata blob.
///
/// codex-rs (`core/src/turn_metadata.rs` `git_workspaces`) keys the map by the
/// repository root path and fills `associated_remote_urls`,
/// `latest_git_commit_hash` and `has_changes` from the local checkout. All of
/// it names the downstream user (home directory, private GitHub owner and
/// repository, exact commit), so the pool replaces it with one stable
/// developer per account: the user name and remote owner below, the OS layout
/// of the profile user-agent and a small per-account set of repositories, of
/// which at most two are worked on during any one developer-day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexWorkspaceIdentity {
    /// Local account name: `<user>` in `/Users/<user>/…` or `C:\Users\<user>\…`.
    pub(crate) user_name: String,
    /// GitHub owner of every synthetic `origin` remote.
    pub(crate) remote_owner: String,
    pub(crate) source: CodexWorkspaceIdentitySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexWorkspaceIdentitySource {
    /// Derived from the e-mail stored in the key's Codex OAuth auth config.
    AuthEmail,
    /// The auth config carries no usable e-mail; a name picked by the
    /// selection hash. Upgraded to `AuthEmail` once an e-mail appears.
    Fallback,
}

impl CodexWorkspaceIdentitySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::AuthEmail => "auth_email",
            Self::Fallback => "fallback",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "auth_email" => Some(Self::AuthEmail),
            "fallback" => Some(Self::Fallback),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexProfileRequestBodyPolicy {
    NormalizeClientMetadata,
    StripClientMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexProfileMaterialization {
    Existing,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexProfileMaterializationOutcome {
    pub(crate) fingerprint: Value,
    pub(crate) materialization: CodexProfileMaterialization,
}

pub(crate) struct CodexProfileMaterializeInput<'a> {
    pub(crate) provider_type: &'a str,
    pub(crate) fingerprint: Option<&'a Value>,
    pub(crate) auth_config_raw: Option<&'a str>,
    pub(crate) key_id: &'a str,
    pub(crate) key_name: &'a str,
    pub(crate) user_agent: &'a str,
    pub(crate) originator: &'a str,
    pub(crate) now_unix_secs: u64,
}

pub(crate) fn codex_default_transport_profile() -> Value {
    default_codex_transport_profile()
}

pub(crate) fn materialize_codex_key_fingerprint(
    input: CodexProfileMaterializeInput<'_>,
) -> Option<CodexProfileMaterializationOutcome> {
    if !input.provider_type.trim().eq_ignore_ascii_case("codex") {
        return None;
    }
    let user_agent = input.user_agent.trim();
    let originator = input.originator.trim();
    if user_agent.is_empty() || originator.is_empty() {
        return None;
    }

    let selection =
        codex_profile_selection_identity(input.auth_config_raw, input.key_name, input.key_id);
    let mut root = input
        .fingerprint
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let existing_profile = root
        .get(CODEX_CLIENT_PROFILE_KEY)
        .and_then(Value::as_object)
        .cloned();
    let reusable_existing_profile = existing_profile
        .as_ref()
        .filter(|profile| codex_profile_selection_matches(profile, &selection));
    let existing_installation_id =
        reusable_existing_profile.and_then(codex_profile_installation_id_from_object);
    let materialization = if existing_installation_id.is_some() {
        CodexProfileMaterialization::Existing
    } else {
        CodexProfileMaterialization::Generated
    };
    let installation_id = existing_installation_id
        .unwrap_or_else(|| deterministic_installation_id_for_selection(&selection));
    let materialized_user_agent = reusable_existing_profile
        .and_then(codex_profile_user_agent_from_object)
        .unwrap_or_else(|| user_agent.to_string());
    let materialized_originator = reusable_existing_profile
        .and_then(codex_profile_originator_from_object)
        .unwrap_or_else(|| originator.to_string());
    let workspace_identity = select_codex_workspace_identity(
        reusable_existing_profile.and_then(codex_profile_workspace_identity_from_object),
        codex_auth_email(input.auth_config_raw).as_deref(),
        &selection,
    );
    normalize_codex_default_transport_profile(&mut root);
    let transport_profile_id = root
        .get(CODEX_TRANSPORT_PROFILE_KEY)
        .and_then(transport_profile_id_from_value)
        .unwrap_or(TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO);
    let transport_tls_fingerprint_hash =
        transport_tls_fingerprint_hash_from_root(&root, transport_profile_id);
    let fingerprint_hash = codex_concrete_profile_hash(
        &materialized_user_agent,
        &materialized_originator,
        &installation_id,
        &workspace_identity,
        transport_profile_id,
        transport_tls_fingerprint_hash.as_deref(),
    );

    let created_at = reusable_existing_profile
        .and_then(|profile| profile.get("created_at_unix_secs"))
        .and_then(Value::as_u64)
        .unwrap_or(input.now_unix_secs);
    let frozen_at = reusable_existing_profile
        .and_then(|profile| profile.get("frozen_at_unix_secs"))
        .and_then(Value::as_u64)
        .unwrap_or(input.now_unix_secs);
    let account_profile_id = reusable_existing_profile
        .and_then(|profile| profile.get("account_profile_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| codex_account_profile_id(&selection.selection_key_hash));

    root.insert(
        CODEX_CLIENT_PROFILE_KEY.to_string(),
        json!({
            "schema_version": CODEX_PROFILE_SCHEMA_VERSION,
            "account_profile_id": account_profile_id,
            "selection_key_kind": selection.selection_key_kind,
            "selection_key_hash": selection.selection_key_hash,
            "client_headers": {
                "user_agent": materialized_user_agent,
                "originator": materialized_originator,
            },
            "install_identity": {
                "installation_id": installation_id,
            },
            "workspace_identity": {
                "user_name": workspace_identity.user_name,
                "remote_owner": workspace_identity.remote_owner,
                "source": workspace_identity.source.as_str(),
            },
            "transport_profile_id": transport_profile_id,
            "transport_tls_fingerprint_hash": transport_tls_fingerprint_hash,
            "fingerprint_hash": fingerprint_hash,
            "created_at_unix_secs": created_at,
            "updated_at_unix_secs": input.now_unix_secs,
            "frozen_at_unix_secs": frozen_at,
        }),
    );

    root.entry(CODEX_TRANSPORT_PROFILE_KEY.to_string())
        .or_insert_with(default_codex_transport_profile);

    Some(CodexProfileMaterializationOutcome {
        fingerprint: Value::Object(root),
        materialization,
    })
}

/// Returns the `(user_agent, originator)` pair persisted in a key's codex
/// profile fingerprint, if both are present. Unlike
/// `resolve_codex_concrete_account_profile` this does not check the selection
/// identity: it is used before a re-login when the account behind the key is
/// not known yet, so the login advertises the identity the key already uses.
pub(crate) fn codex_profile_persisted_client_headers(
    fingerprint: Option<&Value>,
) -> Option<(String, String)> {
    let profile = fingerprint
        .and_then(Value::as_object)
        .and_then(|object| object.get(CODEX_CLIENT_PROFILE_KEY))
        .and_then(Value::as_object)?;
    let user_agent = codex_profile_user_agent_from_object(profile)?;
    let originator = codex_profile_originator_from_object(profile)?;
    Some((user_agent, originator))
}

pub(crate) fn resolve_codex_concrete_account_profile(
    fingerprint: Option<&Value>,
    auth_config_raw: Option<&str>,
    key_id: &str,
    key_name: &str,
    user_agent: &str,
    originator: &str,
) -> Option<CodexConcreteAccountProfile> {
    let user_agent = user_agent.trim();
    let originator = originator.trim();
    if user_agent.is_empty() || originator.is_empty() {
        return None;
    }
    let profile = fingerprint
        .and_then(Value::as_object)
        .and_then(|object| object.get(CODEX_CLIENT_PROFILE_KEY))
        .and_then(Value::as_object);
    let selection = codex_profile_selection_identity(auth_config_raw, key_name, key_id);
    let reusable_profile =
        profile.filter(|profile| codex_profile_selection_matches(profile, &selection));
    let installation_id = reusable_profile
        .and_then(codex_profile_installation_id_from_object)
        .unwrap_or_else(|| deterministic_installation_id_for_selection(&selection));
    let materialized_user_agent = reusable_profile
        .and_then(codex_profile_user_agent_from_object)
        .unwrap_or_else(|| user_agent.to_string());
    let materialized_originator = reusable_profile
        .and_then(codex_profile_originator_from_object)
        .unwrap_or_else(|| originator.to_string());
    // Same rule as materialization, so a legacy profile without a persisted
    // workspace identity presents the value its next refresh will persist.
    let workspace_identity = select_codex_workspace_identity(
        reusable_profile.and_then(codex_profile_workspace_identity_from_object),
        codex_auth_email(auth_config_raw).as_deref(),
        &selection,
    );
    let transport_profile_id = fingerprint
        .and_then(Value::as_object)
        .and_then(|object| object.get(CODEX_TRANSPORT_PROFILE_KEY))
        .and_then(transport_profile_id_from_value)
        .unwrap_or(TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO);
    let transport_tls_fingerprint_hash = fingerprint
        .and_then(Value::as_object)
        .and_then(|root| transport_tls_fingerprint_hash_from_root(root, transport_profile_id));
    let fingerprint_hash = codex_concrete_profile_hash(
        &materialized_user_agent,
        &materialized_originator,
        &installation_id,
        &workspace_identity,
        transport_profile_id,
        transport_tls_fingerprint_hash.as_deref(),
    );

    Some(CodexConcreteAccountProfile {
        user_agent: materialized_user_agent,
        originator: materialized_originator,
        installation_id,
        workspace_identity,
        fingerprint_hash,
    })
}

pub(crate) fn apply_codex_concrete_account_profile_to_request(
    provider_request_headers: &mut BTreeMap<String, String>,
    provider_request_body: &mut Value,
    profile: &CodexConcreteAccountProfile,
) {
    apply_codex_concrete_account_profile_to_request_with_body_policy(
        provider_request_headers,
        provider_request_body,
        profile,
        CodexProfileRequestBodyPolicy::NormalizeClientMetadata,
    );
}

pub(crate) fn apply_codex_concrete_account_profile_to_request_with_body_policy(
    provider_request_headers: &mut BTreeMap<String, String>,
    provider_request_body: &mut Value,
    profile: &CodexConcreteAccountProfile,
    body_policy: CodexProfileRequestBodyPolicy,
) {
    apply_codex_concrete_account_profile_to_request_at(
        provider_request_headers,
        provider_request_body,
        profile,
        body_policy,
        unix_now_secs(),
    );
}

/// One clock reading per request: the header blob and the body blob must
/// carry the same synthetic commit hash, exactly like a client that
/// serializes one payload into both places.
fn apply_codex_concrete_account_profile_to_request_at(
    provider_request_headers: &mut BTreeMap<String, String>,
    provider_request_body: &mut Value,
    profile: &CodexConcreteAccountProfile,
    body_policy: CodexProfileRequestBodyPolicy,
    now_unix_secs: u64,
) {
    apply_codex_client_identity_headers(
        provider_request_headers,
        &profile.user_agent,
        &profile.originator,
    );

    normalize_turn_metadata_in_headers(provider_request_headers, profile, now_unix_secs);
    match body_policy {
        CodexProfileRequestBodyPolicy::NormalizeClientMetadata => {
            normalize_turn_metadata_in_body(provider_request_body, profile, now_unix_secs);
        }
        CodexProfileRequestBodyPolicy::StripClientMetadata => {
            strip_codex_client_metadata_from_body(provider_request_body);
        }
    }
}

pub(crate) fn apply_codex_concrete_account_profile_to_search_headers(
    provider_request_headers: &mut BTreeMap<String, String>,
    profile: &CodexConcreteAccountProfile,
) {
    apply_codex_concrete_account_profile_to_search_headers_at(
        provider_request_headers,
        profile,
        unix_now_secs(),
    );
}

fn apply_codex_concrete_account_profile_to_search_headers_at(
    provider_request_headers: &mut BTreeMap<String, String>,
    profile: &CodexConcreteAccountProfile,
    now_unix_secs: u64,
) {
    apply_codex_client_identity_headers(
        provider_request_headers,
        &profile.user_agent,
        &profile.originator,
    );
    remove_header_case_insensitive(provider_request_headers, X_CODEX_INSTALLATION_ID);

    // Standalone Search carries turn metadata as a header and does not use the
    // Responses client_metadata body contract. Preserve the Search payload while
    // keeping its installation and workspace identity aligned with the
    // selected pool account.
    if let Some((header_name, metadata)) =
        remove_header_case_insensitive(provider_request_headers, X_CODEX_TURN_METADATA)
    {
        if let Some(rewritten) =
            rewrite_turn_metadata_for_profile_string(&metadata, profile, now_unix_secs)
        {
            provider_request_headers.insert(header_name, rewritten);
        }
    }
}

/// Client build version as codex-rs reports it in the `version` header.
///
/// codex-rs sends `version: <CARGO_PKG_VERSION>` on every OpenAI request
/// (responses, compact, search, websocket handshake) and formats the
/// user-agent as `<originator>/<version> (<os> <ver>; <arch>) <terminal>`.
/// The originator may contain spaces (`Codex Desktop/0.153.1 (...)`), so the
/// version is read from the first `/`-bearing token of the product segment
/// that precedes the first parenthesis, not from the first whitespace token.
pub(crate) fn codex_client_version_from_user_agent(user_agent: &str) -> Option<String> {
    let product = user_agent.split('(').next().unwrap_or(user_agent);
    let token = product
        .split_whitespace()
        .find(|token| token.contains('/'))?;
    let (_, version) = token.split_once('/')?;
    let version = version.trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// Writes the client identity headers one codex-rs build sends together:
/// `user-agent`, `originator` and `version`. The version always follows the
/// outbound user-agent; leaving the inbound value in place would pair one
/// build number with another build's user-agent, a shape no single client
/// produces. When the user-agent carries no parsable version the header is
/// dropped rather than left inconsistent.
pub(crate) fn apply_codex_client_identity_headers(
    provider_request_headers: &mut BTreeMap<String, String>,
    user_agent: &str,
    originator: &str,
) {
    provider_request_headers.insert("user-agent".to_string(), user_agent.to_string());
    provider_request_headers.insert("originator".to_string(), originator.to_string());
    remove_header_case_insensitive(provider_request_headers, CLIENT_VERSION_HEADER);
    if let Some(version) = codex_client_version_from_user_agent(user_agent) {
        provider_request_headers.insert(CLIENT_VERSION_HEADER.to_string(), version);
    }
}

pub(crate) fn apply_codex_concrete_account_profile_to_body_with_policy(
    provider_request_body: &mut Value,
    profile: &CodexConcreteAccountProfile,
    body_policy: CodexProfileRequestBodyPolicy,
) {
    match body_policy {
        CodexProfileRequestBodyPolicy::NormalizeClientMetadata => {
            normalize_turn_metadata_in_body(provider_request_body, profile, unix_now_secs());
        }
        CodexProfileRequestBodyPolicy::StripClientMetadata => {
            strip_codex_client_metadata_from_body(provider_request_body);
        }
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

fn normalize_turn_metadata_in_headers(
    provider_request_headers: &mut BTreeMap<String, String>,
    profile: &CodexConcreteAccountProfile,
    now_unix_secs: u64,
) {
    set_header_value_case_insensitive(
        provider_request_headers,
        X_CODEX_INSTALLATION_ID,
        &profile.installation_id,
    );
    if let Some((header_name, metadata)) =
        remove_header_case_insensitive(provider_request_headers, X_CODEX_TURN_METADATA)
    {
        let rewritten = rewrite_turn_metadata_for_profile_string(&metadata, profile, now_unix_secs)
            .unwrap_or(metadata);
        provider_request_headers.insert(header_name, rewritten);
    }
}

pub(crate) fn strip_codex_client_metadata_from_body(provider_request_body: &mut Value) {
    let Some(body) = provider_request_body.as_object_mut() else {
        return;
    };
    body.remove("client_metadata");
}

fn normalize_turn_metadata_in_body(
    provider_request_body: &mut Value,
    profile: &CodexConcreteAccountProfile,
    now_unix_secs: u64,
) {
    let Some(body) = provider_request_body.as_object_mut() else {
        return;
    };

    let metadata = body
        .entry("client_metadata".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !metadata.is_object() {
        *metadata = Value::Object(Map::new());
    }
    let Some(metadata) = metadata.as_object_mut() else {
        return;
    };

    metadata.insert(
        X_CODEX_INSTALLATION_ID.to_string(),
        Value::String(profile.installation_id.clone()),
    );
    let Some(turn_metadata) = metadata.get_mut(X_CODEX_TURN_METADATA) else {
        return;
    };
    rewrite_turn_metadata_for_profile_value(turn_metadata, profile, now_unix_secs);
}

fn rewrite_turn_metadata_for_profile_value(
    value: &mut Value,
    profile: &CodexConcreteAccountProfile,
    now_unix_secs: u64,
) -> bool {
    match value {
        Value::String(raw) => {
            let Some(rewritten) =
                rewrite_turn_metadata_for_profile_string(raw, profile, now_unix_secs)
            else {
                return false;
            };
            *raw = rewritten;
            true
        }
        Value::Object(object) => {
            rewrite_turn_metadata_object_for_profile(object, profile, now_unix_secs);
            true
        }
        _ => false,
    }
}

/// The profile pass owns two blob keys: `installation_id` (the account's
/// frozen install) and `workspaces` (the account's synthetic developer).
/// Every other key is left for the runtime identity pass, which runs after
/// this one on every surface and copies `workspaces` as it finds it here.
fn rewrite_turn_metadata_object_for_profile(
    object: &mut Map<String, Value>,
    profile: &CodexConcreteAccountProfile,
    now_unix_secs: u64,
) {
    object.insert(
        "installation_id".to_string(),
        Value::String(profile.installation_id.clone()),
    );
    if let Some(workspaces) = object.get_mut("workspaces") {
        if let Some(synthetic) = synthesize_codex_workspaces(workspaces, profile, now_unix_secs) {
            *workspaces = synthetic;
        }
    }
}

struct AsciiJsonFormatter;

impl serde_json::ser::Formatter for AsciiJsonFormatter {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let mut run_start = 0;
        for (index, character) in fragment.char_indices() {
            if character.is_ascii() && character != '\u{7f}' {
                continue;
            }
            writer.write_all(fragment[run_start..index].as_bytes())?;
            write_json_unicode_escape(writer, character)?;
            run_start = index + character.len_utf8();
        }
        writer.write_all(fragment[run_start..].as_bytes())
    }
}

fn write_json_unicode_escape<W>(writer: &mut W, character: char) -> io::Result<()>
where
    W: ?Sized + io::Write,
{
    let scalar = character as u32;
    if scalar <= u16::MAX as u32 {
        return write_json_unicode_escape_unit(writer, scalar as u16);
    }
    let surrogate = scalar - 0x1_0000;
    write_json_unicode_escape_unit(writer, 0xd800 | (surrogate >> 10) as u16)?;
    write_json_unicode_escape_unit(writer, 0xdc00 | (surrogate & 0x3ff) as u16)
}

fn write_json_unicode_escape_unit<W>(writer: &mut W, unit: u16) -> io::Result<()>
where
    W: ?Sized + io::Write,
{
    const HEX: &[u8; 16] = b"0123456789abcdef";
    writer.write_all(&[
        b'\\',
        b'u',
        HEX[((unit >> 12) & 0xf) as usize],
        HEX[((unit >> 8) & 0xf) as usize],
        HEX[((unit >> 4) & 0xf) as usize],
        HEX[(unit & 0xf) as usize],
    ])
}

pub(crate) fn serialize_ascii_json(value: &Value) -> Option<String> {
    let mut encoded = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut encoded, AsciiJsonFormatter);
    value.serialize(&mut serializer).ok()?;
    debug_assert!(encoded.is_ascii());
    String::from_utf8(encoded).ok()
}

fn rewrite_turn_metadata_for_profile_string(
    raw: &str,
    profile: &CodexConcreteAccountProfile,
    now_unix_secs: u64,
) -> Option<String> {
    let mut parsed = serde_json::from_str::<Value>(raw).ok()?;
    let object = parsed.as_object_mut()?;
    rewrite_turn_metadata_object_for_profile(object, profile, now_unix_secs);
    // This JSON is embedded in an HTTP header. Preserve Unicode semantics
    // while keeping every serialized byte header-safe.
    serialize_ascii_json(&parsed)
}

pub(crate) fn normalize_codex_turn_metadata_for_profile(
    raw: &str,
    profile: &CodexConcreteAccountProfile,
) -> Option<String> {
    rewrite_turn_metadata_for_profile_string(raw, profile, unix_now_secs())
}

// ---------------------------------------------------------------------------
// Synthetic workspaces
// ---------------------------------------------------------------------------

const WORKSPACE_DOMAIN: &[u8] = b"aether:codex:workspace:v1";

/// Repository names a developer plausibly keeps checked out. Shared by every
/// account; the per-account subset, layout and owner differ.
const WORKSPACE_REPO_NAMES: &[&str] = &[
    "api-gateway",
    "web-app",
    "dashboard",
    "backend",
    "frontend",
    "mobile-app",
    "data-pipeline",
    "ml-experiments",
    "infra",
    "docs",
    "cli-tools",
    "auth-service",
    "payment-service",
    "notification-service",
    "admin-portal",
    "landing-page",
    "design-system",
    "sdk",
    "scripts",
    "playground",
    "monorepo",
    "platform",
    "core",
    "analytics",
    "search-service",
    "chat-app",
    "todo-app",
    "blog",
    "portfolio",
    "ecommerce",
    "inventory",
    "crm",
    "scheduler",
    "worker",
    "ingest",
    "etl",
    "warehouse",
    "reporting",
    "billing",
    "gateway",
    "proxy",
    "edge",
    "orchestrator",
    "agent",
    "bot",
    "automation",
    "devops",
    "k8s-config",
    "terraform",
    "ansible",
    "helm-charts",
    "ci-templates",
    "webhooks",
    "integrations",
    "connectors",
    "plugins",
    "extensions",
    "themes",
    "ui-kit",
    "components",
    "storybook",
    "e2e-tests",
    "load-tests",
    "benchmarks",
    "migrations",
];

/// Directory under the home directory where the repositories live.
const WORKSPACE_UNIX_PARENT_DIRS: &[&str] = &[
    "Projects",
    "Developer",
    "code",
    "src",
    "dev",
    "work",
    "repos",
    "workspace",
];
const WORKSPACE_WINDOWS_PARENT_DIRS: &[&str] =
    &["Projects", "source\\repos", "dev", "code", "repos", "work"];

/// Given names for accounts whose auth config carries no e-mail.
const WORKSPACE_FALLBACK_USER_NAMES: &[&str] = &[
    "alex", "sam", "chris", "jordan", "taylor", "morgan", "casey", "jamie", "riley", "drew",
    "kevin", "david", "daniel", "michael", "james", "ryan", "tom", "ben", "matt", "nick", "eric",
    "jason", "kyle", "adam", "mark", "paul", "peter", "steve", "andrew", "brian", "josh", "luke",
];

/// How many distinct repositories one account cycles through over time
/// (inclusive bounds). At most [`WORKSPACE_ACTIVE_REPOS_PER_DAY`] of them are
/// worked on during any one developer-day.
const WORKSPACE_MIN_REPOS: u64 = 3;
const WORKSPACE_MAX_REPOS: u64 = 8;
/// A developer touches at most this many repositories per day: the main
/// project and one side project.
const WORKSPACE_ACTIVE_REPOS_PER_DAY: usize = 2;
/// The main project stays the same for this many days (inclusive bounds).
const WORKSPACE_PRIMARY_MIN_DAYS: u64 = 3;
const WORKSPACE_PRIMARY_MAX_DAYS: u64 = 10;
/// The side project rotates every one to three days (inclusive bounds).
const WORKSPACE_SECONDARY_MIN_DAYS: u64 = 1;
const WORKSPACE_SECONDARY_MAX_DAYS: u64 = 3;
/// Share of inbound repository roots that land on the main project:
/// `WORKSPACE_PRIMARY_LANE_WEIGHT` out of `WORKSPACE_LANE_WEIGHTS_TOTAL`.
const WORKSPACE_PRIMARY_LANE_WEIGHT: u64 = 2;
const WORKSPACE_LANE_WEIGHTS_TOTAL: u64 = 3;
/// A developer-day starts this long after local midnight (inclusive bounds),
/// so the active repository set changes between 03:00 and 06:00 gateway
/// local time rather than exactly at midnight.
const WORKSPACE_DAY_START_MIN_SECS: u64 = 3 * 3_600;
const WORKSPACE_DAY_START_MAX_SECS: u64 = 6 * 3_600;
const SECS_PER_DAY: u64 = 86_400;
/// Per-repository commit cadence: a commit lands every few hours to every two
/// days, each repository on its own period and phase.
const WORKSPACE_COMMIT_PERIODS_SECS: &[u64] = &[3 * 3_600, 6 * 3_600, 12 * 3_600, 86_400, 172_800];
/// Right after a commit the checkout is clean for a while (this share of the
/// commit period, inclusive bounds, in permille), then the developer is
/// editing again and `has_changes` is true until the next commit.
const WORKSPACE_CLEAN_WINDOW_MIN_PERMILLE: u64 = 50;
const WORKSPACE_CLEAN_WINDOW_MAX_PERMILLE: u64 = 250;
const MAX_WORKSPACE_USER_NAME_LEN: usize = 20;
/// GitHub's user name limit.
const MAX_WORKSPACE_REMOTE_OWNER_LEN: usize = 39;

fn workspace_digest(seed: &str, label: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(WORKSPACE_DOMAIN);
    hasher.update([0]);
    hasher.update(seed.as_bytes());
    hasher.update([0]);
    hasher.update(label.as_bytes());
    for part in parts {
        hasher.update([0]);
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn digest_index(digest: &[u8; 32], modulus: u64) -> u64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes) % modulus.max(1)
}

/// Uniform pick from an inclusive range, seeded by the digest.
fn digest_range(digest: &[u8; 32], min: u64, max: u64) -> u64 {
    min + digest_index(digest, max.saturating_sub(min) + 1)
}

fn pick<'a>(list: &[&'a str], digest: &[u8; 32]) -> &'a str {
    list[digest_index(digest, list.len() as u64) as usize]
}

/// Per-account choices that must not vary between requests: OS layout, home
/// directory, repository parent directory, remote URL style, repository set,
/// day boundary and rotation cadence. Seeded by the frozen `installation_id`,
/// so the developer is as stable as the install it belongs to. Time-dependent
/// values (which repositories are active today, the current commit, whether
/// the checkout is dirty) are pure functions of the layout and the request
/// instant, so the header and the body of one request agree and every
/// request made in the same window agrees with the previous one.
struct CodexWorkspaceLayout<'a> {
    seed: &'a str,
    user_name: &'a str,
    remote_owner: &'a str,
    os: OutboundClientOs,
    parent_dir: &'static str,
    /// Distinct repository names, one per slot.
    repos: Vec<&'static str>,
    ssh_remote: bool,
    /// Gateway time zone the developer-day is counted in.
    tz: Tz,
    day_start_secs: u64,
    primary_period_days: u64,
    primary_phase_days: u64,
    secondary_period_days: u64,
    secondary_phase_days: u64,
}

impl<'a> CodexWorkspaceLayout<'a> {
    fn for_profile(profile: &'a CodexConcreteAccountProfile) -> Self {
        Self::for_profile_in(profile, process_environment_timezone().tz)
    }

    fn for_profile_in(profile: &'a CodexConcreteAccountProfile, tz: Tz) -> Self {
        let seed = profile.installation_id.as_str();
        let os = OutboundClientOs::from_user_agent(Some(profile.user_agent.as_str()));
        let parent_dir = match os {
            OutboundClientOs::Windows => pick(
                WORKSPACE_WINDOWS_PARENT_DIRS,
                &workspace_digest(seed, "parent-dir", &[]),
            ),
            OutboundClientOs::MacOs | OutboundClientOs::Other => pick(
                WORKSPACE_UNIX_PARENT_DIRS,
                &workspace_digest(seed, "parent-dir", &[]),
            ),
        };
        let repo_slots = digest_range(
            &workspace_digest(seed, "repo-slots", &[]),
            WORKSPACE_MIN_REPOS,
            WORKSPACE_MAX_REPOS,
        );
        // Distinct names: a slot whose pick collides walks forward to the next
        // free name, so the account really has `repo_slots` repositories.
        let mut repos: Vec<&'static str> = Vec::with_capacity(repo_slots as usize);
        for slot in 0..repo_slots {
            let start = digest_index(
                &workspace_digest(seed, "repo", &[&slot.to_be_bytes()]),
                WORKSPACE_REPO_NAMES.len() as u64,
            ) as usize;
            let name = (0..WORKSPACE_REPO_NAMES.len())
                .map(|step| WORKSPACE_REPO_NAMES[(start + step) % WORKSPACE_REPO_NAMES.len()])
                .find(|candidate| !repos.contains(candidate))
                .unwrap_or(WORKSPACE_REPO_NAMES[start]);
            repos.push(name);
        }
        let ssh_remote = workspace_digest(seed, "remote-scheme", &[])[0] & 1 == 1;
        let day_start_secs = digest_range(
            &workspace_digest(seed, "day-start", &[]),
            WORKSPACE_DAY_START_MIN_SECS,
            WORKSPACE_DAY_START_MAX_SECS,
        );
        let primary_period_days = digest_range(
            &workspace_digest(seed, "primary-period", &[]),
            WORKSPACE_PRIMARY_MIN_DAYS,
            WORKSPACE_PRIMARY_MAX_DAYS,
        );
        let primary_phase_days = digest_index(
            &workspace_digest(seed, "primary-phase", &[]),
            primary_period_days,
        );
        let secondary_period_days = digest_range(
            &workspace_digest(seed, "secondary-period", &[]),
            WORKSPACE_SECONDARY_MIN_DAYS,
            WORKSPACE_SECONDARY_MAX_DAYS,
        );
        let secondary_phase_days = digest_index(
            &workspace_digest(seed, "secondary-phase", &[]),
            secondary_period_days,
        );
        Self {
            seed,
            user_name: profile.workspace_identity.user_name.as_str(),
            remote_owner: profile.workspace_identity.remote_owner.as_str(),
            os,
            parent_dir,
            repos,
            ssh_remote,
            tz,
            day_start_secs,
            primary_period_days,
            primary_phase_days,
            secondary_period_days,
            secondary_phase_days,
        }
    }

    /// Developer-day index: days since the epoch in the gateway time zone,
    /// with the day boundary shifted to the account's start-of-day hour.
    fn local_day_index(&self, now_unix_secs: u64) -> u64 {
        let offset_secs = self
            .tz
            .timestamp_opt(now_unix_secs.min(i64::MAX as u64) as i64, 0)
            .single()
            .map(|at| i64::from(at.offset().fix().local_minus_utc()))
            .unwrap_or(0);
        let local = (now_unix_secs as i64 + offset_secs - self.day_start_secs as i64).max(0) as u64;
        local / SECS_PER_DAY
    }

    /// The two repositories the developer works on during the day that
    /// contains `now`: the main project (rotates every few days) and a side
    /// project (rotates every day or so, never the main one).
    fn active_repos(&self, now_unix_secs: u64) -> (&'static str, &'static str) {
        let day = self.local_day_index(now_unix_secs);
        let slots = self.repos.len() as u64;
        let primary_epoch = (day + self.primary_phase_days) / self.primary_period_days;
        let primary = digest_index(
            &workspace_digest(self.seed, "primary", &[&primary_epoch.to_be_bytes()]),
            slots,
        );
        let secondary_epoch = (day + self.secondary_phase_days) / self.secondary_period_days;
        let mut secondary = digest_index(
            &workspace_digest(self.seed, "secondary", &[&secondary_epoch.to_be_bytes()]),
            slots,
        );
        if secondary == primary {
            secondary = (secondary + 1) % slots;
        }
        (self.repos[primary as usize], self.repos[secondary as usize])
    }

    /// Maps an inbound repository root onto one of today's two repositories.
    /// The same inbound root lands on the same lane every time, so one
    /// downstream thread keeps one workspace for as long as that lane's
    /// repository stays active; when the developer moves on to another
    /// project, so does the thread.
    fn repo_for_inbound_root(&self, inbound_root: &str, now_unix_secs: u64) -> &'static str {
        let (primary, secondary) = self.active_repos(now_unix_secs);
        let lane = digest_index(
            &workspace_digest(self.seed, "lane", &[inbound_root.as_bytes()]),
            WORKSPACE_LANE_WEIGHTS_TOTAL,
        );
        if lane < WORKSPACE_PRIMARY_LANE_WEIGHT {
            primary
        } else {
            secondary
        }
    }

    fn repo_root(&self, repo: &str) -> String {
        match self.os {
            OutboundClientOs::MacOs => {
                format!("/Users/{}/{}/{repo}", self.user_name, self.parent_dir)
            }
            OutboundClientOs::Windows => {
                format!("C:\\Users\\{}\\{}\\{repo}", self.user_name, self.parent_dir)
            }
            OutboundClientOs::Other => {
                format!("/home/{}/{}/{repo}", self.user_name, self.parent_dir)
            }
        }
    }

    /// Both shapes are what codex-rs `SanitizedGitUrl` lets through: the SSH
    /// `git@` user is preserved, HTTPS carries no userinfo.
    fn remote_url(&self, repo: &str) -> String {
        if self.ssh_remote {
            format!("git@github.com:{}/{repo}.git", self.remote_owner)
        } else {
            format!("https://github.com/{}/{repo}.git", self.remote_owner)
        }
    }

    /// Each repository commits on its own period and phase.
    fn commit_cadence(&self, repo: &str) -> (u64, u64) {
        let period = WORKSPACE_COMMIT_PERIODS_SECS[digest_index(
            &workspace_digest(self.seed, "commit-period", &[repo.as_bytes()]),
            WORKSPACE_COMMIT_PERIODS_SECS.len() as u64,
        ) as usize];
        let phase = digest_index(
            &workspace_digest(self.seed, "commit-phase", &[repo.as_bytes()]),
            period,
        );
        (period, phase)
    }

    /// `(epoch, seconds elapsed since that epoch's commit, period)`.
    fn commit_epoch(&self, repo: &str, now_unix_secs: u64) -> (u64, u64, u64) {
        let (period, phase) = self.commit_cadence(repo);
        let shifted = now_unix_secs.saturating_add(phase);
        (shifted / period, shifted % period, period)
    }

    /// Stable inside one commit period, then moves: a developer who never
    /// commits is as unusual as one who commits on every request.
    fn commit_hash(&self, repo: &str, now_unix_secs: u64) -> String {
        let (epoch, _, _) = self.commit_epoch(repo, now_unix_secs);
        let digest = workspace_digest(
            self.seed,
            "commit",
            &[repo.as_bytes(), &epoch.to_be_bytes()],
        );
        hex_lower(&digest[..20])
    }

    /// Clean for a short window after each commit, dirty until the next one.
    fn has_changes(&self, repo: &str, now_unix_secs: u64) -> bool {
        let (epoch, elapsed, period) = self.commit_epoch(repo, now_unix_secs);
        let permille = digest_range(
            &workspace_digest(
                self.seed,
                "clean-window",
                &[repo.as_bytes(), &epoch.to_be_bytes()],
            ),
            WORKSPACE_CLEAN_WINDOW_MIN_PERMILLE,
            WORKSPACE_CLEAN_WINDOW_MAX_PERMILLE,
        );
        elapsed >= period * permille / 1_000
    }
}

/// Replaces every inbound `workspaces` entry with the account's synthetic
/// repository for that root. Field presence mirrors the inbound entry
/// (codex-rs only serializes the fields it collected), so a client version or
/// a checkout without remotes keeps its shape while every value is the
/// account's own: remote, commit and dirty flag all come from the layout's
/// cadence, never from the downstream checkout. Two inbound roots that land on
/// the same repository collapse into one entry. `None` leaves the inbound
/// value untouched: not an object, or empty, in which case nothing
/// identifying is present.
fn synthesize_codex_workspaces(
    inbound: &Value,
    profile: &CodexConcreteAccountProfile,
    now_unix_secs: u64,
) -> Option<Value> {
    let inbound = inbound.as_object()?;
    if inbound.is_empty() {
        return None;
    }
    let layout = CodexWorkspaceLayout::for_profile(profile);
    // codex-rs keys the map with a BTreeMap, so the wire order is sorted.
    let mut synthetic = BTreeMap::new();
    for (inbound_root, inbound_entry) in inbound {
        let repo = layout.repo_for_inbound_root(inbound_root, now_unix_secs);
        let inbound_entry = inbound_entry.as_object();
        let mut entry = Map::new();
        if inbound_entry.is_some_and(|entry| entry.contains_key("associated_remote_urls")) {
            entry.insert(
                "associated_remote_urls".to_string(),
                json!({ "origin": layout.remote_url(repo) }),
            );
        }
        if inbound_entry.is_some_and(|entry| entry.contains_key("latest_git_commit_hash")) {
            entry.insert(
                "latest_git_commit_hash".to_string(),
                Value::String(layout.commit_hash(repo, now_unix_secs)),
            );
        }
        if inbound_entry.is_some_and(|entry| entry.contains_key("has_changes")) {
            entry.insert(
                "has_changes".to_string(),
                Value::Bool(layout.has_changes(repo, now_unix_secs)),
            );
        }
        synthetic.insert(layout.repo_root(repo), Value::Object(entry));
    }
    Some(Value::Object(synthetic.into_iter().collect()))
}

// ---------------------------------------------------------------------------
// Workspace identity (user name / remote owner)
// ---------------------------------------------------------------------------

/// E-mail of the Codex account behind the key, as the OAuth login / import
/// stored it in the auth config (`email`; imports may still carry the
/// `oauth_email` alias).
pub(crate) fn codex_auth_email(auth_config_raw: Option<&str>) -> Option<String> {
    let raw = auth_config_raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<Value>(raw).ok()?;
    ["email", "oauth_email"].iter().find_map(|key| {
        parsed
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| value.contains('@'))
            .map(str::to_ascii_lowercase)
    })
}

/// Persisted identity wins so an account never changes developer; the only
/// move is from a hash-picked fallback to the e-mail-derived name once the
/// auth config gains an e-mail (import without profile enrichment, then a
/// re-login).
fn select_codex_workspace_identity(
    persisted: Option<CodexWorkspaceIdentity>,
    auth_email: Option<&str>,
    selection: &CodexProfileSelectionIdentity,
) -> CodexWorkspaceIdentity {
    let from_email = auth_email.and_then(workspace_identity_from_email);
    match (persisted, from_email) {
        (Some(persisted), Some(from_email))
            if persisted.source == CodexWorkspaceIdentitySource::Fallback =>
        {
            from_email
        }
        (Some(persisted), _) => persisted,
        (None, Some(from_email)) => from_email,
        (None, None) => fallback_workspace_identity(selection),
    }
}

/// `john.smith+codex@example.com` → home `johnsmith`, owner `john-smith`.
/// A local part without a letter (numeric mailboxes) yields `None`: neither
/// a macOS account nor a GitHub handle is usually all digits.
fn workspace_identity_from_email(email: &str) -> Option<CodexWorkspaceIdentity> {
    let local = email.split('@').next()?.split('+').next()?.trim();
    let local = local.to_ascii_lowercase();
    let user_name = local
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(MAX_WORKSPACE_USER_NAME_LEN)
        .collect::<String>();
    if !user_name
        .chars()
        .any(|character| character.is_ascii_alphabetic())
    {
        return None;
    }
    let mut remote_owner = String::new();
    for character in local.chars() {
        if character.is_ascii_alphanumeric() {
            remote_owner.push(character);
        } else if !remote_owner.is_empty() && !remote_owner.ends_with('-') {
            remote_owner.push('-');
        }
    }
    remote_owner.truncate(MAX_WORKSPACE_REMOTE_OWNER_LEN);
    let remote_owner = remote_owner.trim_matches('-');
    let remote_owner = if remote_owner.is_empty() {
        user_name.clone()
    } else {
        remote_owner.to_string()
    };
    Some(CodexWorkspaceIdentity {
        user_name,
        remote_owner,
        source: CodexWorkspaceIdentitySource::AuthEmail,
    })
}

fn fallback_workspace_identity(
    selection: &CodexProfileSelectionIdentity,
) -> CodexWorkspaceIdentity {
    let digest = workspace_digest(
        &selection.selection_key_hash,
        "fallback-user",
        &[selection.selection_key_kind.as_bytes()],
    );
    let mut user_name = pick(WORKSPACE_FALLBACK_USER_NAMES, &digest).to_string();
    if digest[8] & 1 == 1 {
        user_name.push_str(&format!("{:02}", digest[9] % 100));
    }
    CodexWorkspaceIdentity {
        remote_owner: user_name.clone(),
        user_name,
        source: CodexWorkspaceIdentitySource::Fallback,
    }
}

fn codex_profile_workspace_identity_from_object(
    profile: &Map<String, Value>,
) -> Option<CodexWorkspaceIdentity> {
    let identity = profile.get("workspace_identity")?.as_object()?;
    let field = |key: &str, max_len: usize, extra: &[char]| {
        identity
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= max_len
                    && value.chars().all(|character| {
                        character.is_ascii_alphanumeric() || extra.contains(&character)
                    })
            })
            .map(ToOwned::to_owned)
    };
    let user_name = field("user_name", 64, &['.', '_', '-'])?;
    let remote_owner = field("remote_owner", MAX_WORKSPACE_REMOTE_OWNER_LEN, &['-'])?;
    // A hand-written identity without `source` is kept as if pinned.
    let source = identity
        .get("source")
        .and_then(Value::as_str)
        .and_then(CodexWorkspaceIdentitySource::parse)
        .unwrap_or(CodexWorkspaceIdentitySource::AuthEmail);
    Some(CodexWorkspaceIdentity {
        user_name,
        remote_owner,
        source,
    })
}

fn set_header_value_case_insensitive(
    headers: &mut BTreeMap<String, String>,
    target: &str,
    value: &str,
) {
    let header_name = remove_header_case_insensitive(headers, target)
        .map(|(header_name, _)| header_name)
        .unwrap_or_else(|| target.to_string());
    headers.insert(header_name, value.to_string());
}

fn remove_header_case_insensitive(
    headers: &mut BTreeMap<String, String>,
    target: &str,
) -> Option<(String, String)> {
    let header_name = headers
        .keys()
        .find(|candidate| candidate.trim().eq_ignore_ascii_case(target))
        .cloned()?;
    let value = headers.remove(&header_name)?;
    Some((header_name, value))
}

struct CodexProfileSelectionIdentity {
    selection_key_kind: &'static str,
    selection_key_hash: String,
}

fn codex_profile_selection_identity(
    auth_config_raw: Option<&str>,
    key_name: &str,
    key_id: &str,
) -> CodexProfileSelectionIdentity {
    if let Some(account_id) = codex_auth_account_id(auth_config_raw) {
        return CodexProfileSelectionIdentity {
            selection_key_kind: "auth_account_id",
            selection_key_hash: digest_hex(account_id.as_bytes()),
        };
    }
    let key_id = key_id.trim();
    if !key_id.is_empty() {
        return CodexProfileSelectionIdentity {
            selection_key_kind: "key_id",
            selection_key_hash: digest_hex(key_id.as_bytes()),
        };
    }
    let key_name = key_name.trim();
    CodexProfileSelectionIdentity {
        selection_key_kind: "key_name",
        selection_key_hash: digest_hex(key_name.as_bytes()),
    }
}

pub(crate) fn codex_account_selection_key(
    auth_config_raw: Option<&str>,
    key_name: &str,
    key_id: &str,
) -> String {
    if let Some(account_id) = codex_auth_account_id(auth_config_raw) {
        return account_id;
    }
    let key_id = key_id.trim();
    if !key_id.is_empty() {
        return key_id.to_string();
    }
    key_name.trim().to_string()
}

pub(crate) fn codex_auth_account_id(auth_config_raw: Option<&str>) -> Option<String> {
    let raw = auth_config_raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<Value>(raw).ok()?;
    [
        "account_id",
        "accountId",
        "chatgpt_account_id",
        "chatgptAccountId",
    ]
    .iter()
    .find_map(|key| {
        parsed
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn deterministic_installation_id_for_selection(
    selection: &CodexProfileSelectionIdentity,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aether:codex:installation-id:v1");
    hasher.update([0]);
    hasher.update(selection.selection_key_kind.as_bytes());
    hasher.update([0]);
    hasher.update(selection.selection_key_hash.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

fn codex_profile_selection_matches(
    profile: &Map<String, Value>,
    selection: &CodexProfileSelectionIdentity,
) -> bool {
    let kind_matches = profile
        .get("selection_key_kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| value == selection.selection_key_kind);
    let hash_matches = profile
        .get("selection_key_hash")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| value == selection.selection_key_hash);
    kind_matches && hash_matches
}

fn codex_profile_user_agent_from_object(profile: &Map<String, Value>) -> Option<String> {
    profile
        .get("client_headers")
        .and_then(Value::as_object)
        .and_then(|headers| {
            headers
                .get("user_agent")
                .or_else(|| headers.get("user-agent"))
        })
        .or_else(|| {
            profile
                .get("user_agent")
                .or_else(|| profile.get("user-agent"))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn codex_profile_originator_from_object(profile: &Map<String, Value>) -> Option<String> {
    profile
        .get("client_headers")
        .and_then(Value::as_object)
        .and_then(|headers| headers.get("originator"))
        .or_else(|| profile.get("originator"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn codex_profile_installation_id_from_object(profile: &Map<String, Value>) -> Option<String> {
    profile
        .get("install_identity")
        .and_then(Value::as_object)
        .and_then(|identity| identity.get("installation_id"))
        .or_else(|| profile.get("installation_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| Uuid::parse_str(value).is_ok())
        .map(ToOwned::to_owned)
}

fn transport_profile_id_from_value(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            value
                .as_object()
                .and_then(|object| {
                    object
                        .get("profile_id")
                        .or_else(|| object.get("id"))
                        .and_then(Value::as_str)
                })
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
}

fn default_codex_transport_profile() -> Value {
    json!({
        "profile_id": TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO,
        "backend": TRANSPORT_BACKEND_REQWEST_DEFAULT_TLS,
        "http_mode": TRANSPORT_HTTP_MODE_AUTO,
        "pool_scope": TRANSPORT_POOL_SCOPE_KEY,
        "extra": codex_default_transport_profile_extra()
    })
}

fn normalize_codex_default_transport_profile(root: &mut Map<String, Value>) {
    let should_replace = root
        .get(CODEX_TRANSPORT_PROFILE_KEY)
        .and_then(transport_profile_id_from_value)
        .is_some_and(|profile_id| {
            is_legacy_codex_default_transport_profile_id(profile_id)
                || is_codex_default_transport_profile_id(profile_id)
        });
    if should_replace {
        root.insert(
            CODEX_TRANSPORT_PROFILE_KEY.to_string(),
            default_codex_transport_profile(),
        );
    }
}

fn is_codex_default_transport_profile_id(profile_id: &str) -> bool {
    profile_id
        .trim()
        .eq_ignore_ascii_case(TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO)
}

fn is_legacy_codex_default_transport_profile_id(profile_id: &str) -> bool {
    profile_id
        .trim()
        .eq_ignore_ascii_case(TRANSPORT_PROFILE_CODEX_LEGACY_REQWEST_RUSTLS_AUTO)
}

fn transport_tls_fingerprint_hash_from_root(
    root: &Map<String, Value>,
    transport_profile_id: &str,
) -> Option<String> {
    root.get(CODEX_TRANSPORT_PROFILE_KEY)
        .and_then(transport_tls_fingerprint_hash_from_value)
        .or_else(|| default_transport_tls_fingerprint_hash(transport_profile_id))
        .map(ToOwned::to_owned)
}

fn transport_tls_fingerprint_hash_from_value(value: &Value) -> Option<&str> {
    value
        .as_object()
        .and_then(|object| object.get("extra"))
        .and_then(|extra| extra.get("tls_fingerprint"))
        .and_then(|tls| tls.get("ja3_hash"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn default_transport_tls_fingerprint_hash(transport_profile_id: &str) -> Option<&'static str> {
    transport_profile_id
        .trim()
        .eq_ignore_ascii_case(TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO)
        .then_some(CODEX_DEFAULT_TLS_JA3_HASH)
}

fn codex_account_profile_id(selection_key_hash: &str) -> String {
    format!(
        "codex-profile-{}",
        selection_key_hash
            .strip_prefix("sha256:")
            .unwrap_or(selection_key_hash)
            .chars()
            .take(16)
            .collect::<String>()
    )
}

fn codex_concrete_profile_hash(
    user_agent: &str,
    originator: &str,
    installation_id: &str,
    workspace_identity: &CodexWorkspaceIdentity,
    transport_profile_id: &str,
    transport_tls_fingerprint_hash: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aether:codex:concrete-profile:v3");
    hasher.update([0]);
    hasher.update(user_agent.as_bytes());
    hasher.update([0]);
    hasher.update(originator.as_bytes());
    hasher.update([0]);
    hasher.update(installation_id.as_bytes());
    hasher.update([0]);
    hasher.update(workspace_identity.user_name.as_bytes());
    hasher.update([0]);
    hasher.update(workspace_identity.remote_owner.as_bytes());
    hasher.update([0]);
    hasher.update(transport_profile_id.as_bytes());
    hasher.update([0]);
    hasher.update(transport_tls_fingerprint_hash.unwrap_or("").as_bytes());
    format!("sha256:{}", hex_lower(&hasher.finalize()))
}

fn digest_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex_lower(&hasher.finalize()))
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materializes_codex_fingerprint_with_uuidv4_installation_and_transport() {
        let outcome = materialize_codex_key_fingerprint(CodexProfileMaterializeInput {
            provider_type: "codex",
            fingerprint: None,
            auth_config_raw: Some(r#"{"account_id":"acc-1"}"#),
            key_id: "key-1",
            key_name: "name-1",
            user_agent: "codex-tui/0.142.0 test",
            originator: "codex-tui",
            now_unix_secs: 1_760_000_000,
        })
        .expect("codex profile should materialize");

        assert_eq!(
            outcome.materialization,
            CodexProfileMaterialization::Generated
        );
        let profile = &outcome.fingerprint[CODEX_CLIENT_PROFILE_KEY];
        let installation_id = profile["install_identity"]["installation_id"]
            .as_str()
            .expect("installation_id");
        let uuid = Uuid::parse_str(installation_id).expect("uuid installation_id");
        assert_eq!(uuid.get_version_num(), 4);
        assert_eq!(profile["selection_key_kind"], "auth_account_id");
        assert_eq!(
            outcome.fingerprint[CODEX_TRANSPORT_PROFILE_KEY]["backend"],
            TRANSPORT_BACKEND_REQWEST_DEFAULT_TLS
        );
        assert_eq!(
            outcome.fingerprint[CODEX_TRANSPORT_PROFILE_KEY]["extra"]["tls_fingerprint"]
                ["ja3_hash"],
            CODEX_DEFAULT_TLS_JA3_HASH
        );
        assert_eq!(
            outcome.fingerprint[CODEX_TRANSPORT_PROFILE_KEY]["extra"]["tls_fingerprint"]["ja3"],
            CODEX_DEFAULT_TLS_JA3
        );
        assert_eq!(
            profile["transport_tls_fingerprint_hash"],
            CODEX_DEFAULT_TLS_JA3_HASH
        );
        // No e-mail in the auth config: a hash-picked developer, persisted as such.
        assert_eq!(profile["workspace_identity"]["source"], "fallback");
        let user_name = profile["workspace_identity"]["user_name"]
            .as_str()
            .expect("user_name");
        assert!(
            user_name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "{user_name}"
        );
        assert_eq!(profile["workspace_identity"]["remote_owner"], user_name);
        assert_eq!(
            profile["fingerprint_hash"],
            codex_concrete_profile_hash(
                "codex-tui/0.142.0 test",
                "codex-tui",
                installation_id,
                &CodexWorkspaceIdentity {
                    user_name: user_name.to_string(),
                    remote_owner: user_name.to_string(),
                    source: CodexWorkspaceIdentitySource::Fallback,
                },
                TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO,
                Some(CODEX_DEFAULT_TLS_JA3_HASH),
            )
        );
    }

    #[test]
    fn materialization_preserves_existing_profile_when_selection_matches() {
        let selection =
            codex_profile_selection_identity(Some(r#"{"account_id":"acc-1"}"#), "name-1", "key-1");
        let fingerprint = json!({
            "codex_client_profile": {
                "selection_key_kind": selection.selection_key_kind,
                "selection_key_hash": selection.selection_key_hash,
                "client_headers": {
                    "user_agent": "codex-tui/0.141.0 persisted",
                    "originator": "codex-tui"
                },
                "install_identity": {
                    "installation_id": "019f0a27-08f6-47d2-ba0b-1ff45470ee76"
                },
                "created_at_unix_secs": 123,
                "frozen_at_unix_secs": 123
            },
            "transport_profile": "custom-transport"
        });
        let outcome = materialize_codex_key_fingerprint(CodexProfileMaterializeInput {
            provider_type: "codex",
            fingerprint: Some(&fingerprint),
            auth_config_raw: Some(r#"{"account_id":"acc-1"}"#),
            key_id: "key-1",
            key_name: "name-1",
            user_agent: "codex-tui/0.142.0 test",
            originator: "codex-tui",
            now_unix_secs: 999,
        })
        .expect("codex profile should materialize");

        assert_eq!(
            outcome.materialization,
            CodexProfileMaterialization::Existing
        );
        assert_eq!(
            outcome.fingerprint[CODEX_CLIENT_PROFILE_KEY]["install_identity"]["installation_id"],
            "019f0a27-08f6-47d2-ba0b-1ff45470ee76"
        );
        assert_eq!(
            outcome.fingerprint[CODEX_CLIENT_PROFILE_KEY]["client_headers"]["user_agent"],
            "codex-tui/0.141.0 persisted"
        );
        assert_eq!(
            outcome.fingerprint[CODEX_CLIENT_PROFILE_KEY]["transport_profile_id"],
            "custom-transport"
        );
        assert_eq!(
            outcome.fingerprint[CODEX_CLIENT_PROFILE_KEY]["created_at_unix_secs"],
            123
        );
        assert_eq!(
            outcome.fingerprint[CODEX_CLIENT_PROFILE_KEY]["transport_tls_fingerprint_hash"],
            Value::Null
        );
    }

    #[test]
    fn materialization_rebinds_profile_when_selection_changes() {
        let old_selection = codex_profile_selection_identity(
            Some(r#"{"account_id":"acc-old"}"#),
            "name-1",
            "key-1",
        );
        let fingerprint = json!({
            "codex_client_profile": {
                "selection_key_kind": old_selection.selection_key_kind,
                "selection_key_hash": old_selection.selection_key_hash,
                "client_headers": {
                    "user_agent": "codex-tui/0.141.0 old",
                    "originator": "codex-tui"
                },
                "install_identity": {
                    "installation_id": "019f0a27-08f6-47d2-ba0b-1ff45470ee76"
                },
                "created_at_unix_secs": 123,
                "frozen_at_unix_secs": 123
            },
            "transport_profile": "codex-reqwest-default-tls-auto"
        });
        let outcome = materialize_codex_key_fingerprint(CodexProfileMaterializeInput {
            provider_type: "codex",
            fingerprint: Some(&fingerprint),
            auth_config_raw: Some(r#"{"account_id":"acc-new"}"#),
            key_id: "key-1",
            key_name: "name-1",
            user_agent: "codex-tui/0.142.0 new",
            originator: "codex-tui",
            now_unix_secs: 999,
        })
        .expect("codex profile should materialize");

        let profile = &outcome.fingerprint[CODEX_CLIENT_PROFILE_KEY];
        assert_eq!(
            outcome.materialization,
            CodexProfileMaterialization::Generated
        );
        assert_ne!(
            profile["install_identity"]["installation_id"],
            "019f0a27-08f6-47d2-ba0b-1ff45470ee76"
        );
        assert_eq!(
            profile["client_headers"]["user_agent"],
            "codex-tui/0.142.0 new"
        );
        assert_eq!(profile["created_at_unix_secs"], 999);
        assert_eq!(profile["frozen_at_unix_secs"], 999);
    }

    #[test]
    fn codex_profile_selection_accepts_legacy_chatgpt_account_alias() {
        let selection = codex_profile_selection_identity(
            Some(r#"{"chatgptAccountId":"acc-alias"}"#),
            "name-1",
            "key-1",
        );

        assert_eq!(selection.selection_key_kind, "auth_account_id");
        assert_eq!(
            selection.selection_key_hash,
            digest_hex("acc-alias".as_bytes())
        );
        assert_eq!(
            codex_account_selection_key(
                Some(r#"{"chatgpt_account_id":"acc-alias"}"#),
                "name-1",
                "key-1"
            ),
            "acc-alias"
        );
    }

    #[test]
    fn materialization_expands_codex_default_string_transport_to_full_profile() {
        let fingerprint = json!({
            "transport_profile": "codex-reqwest-default-tls-auto"
        });
        let outcome = materialize_codex_key_fingerprint(CodexProfileMaterializeInput {
            provider_type: "codex",
            fingerprint: Some(&fingerprint),
            auth_config_raw: Some(r#"{"account_id":"acc-1"}"#),
            key_id: "key-1",
            key_name: "name-1",
            user_agent: "codex-tui/0.142.0 test",
            originator: "codex-tui",
            now_unix_secs: 999,
        })
        .expect("codex profile should materialize");

        assert_eq!(
            outcome.fingerprint[CODEX_TRANSPORT_PROFILE_KEY]["profile_id"],
            TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO
        );
        assert_eq!(
            outcome.fingerprint[CODEX_TRANSPORT_PROFILE_KEY]["extra"]["tls_fingerprint"]
                ["ja3_hash"],
            CODEX_DEFAULT_TLS_JA3_HASH
        );
        assert_eq!(
            outcome.fingerprint[CODEX_CLIENT_PROFILE_KEY]["transport_tls_fingerprint_hash"],
            CODEX_DEFAULT_TLS_JA3_HASH
        );
    }

    #[test]
    fn materialization_normalizes_legacy_rustls_default_to_codex_default_tls() {
        let fingerprint = json!({
            "transport_profile": {
                "profile_id": "codex-reqwest-rustls-auto",
                "backend": "reqwest_rustls",
                "http_mode": "auto",
                "pool_scope": "key"
            }
        });

        let outcome = materialize_codex_key_fingerprint(CodexProfileMaterializeInput {
            provider_type: "codex",
            fingerprint: Some(&fingerprint),
            auth_config_raw: Some(r#"{"account_id":"acc-1"}"#),
            key_id: "key-1",
            key_name: "name-1",
            user_agent: "codex-tui/0.142.0 test",
            originator: "codex-tui",
            now_unix_secs: 999,
        })
        .expect("codex profile should materialize");

        assert_eq!(
            outcome.fingerprint[CODEX_TRANSPORT_PROFILE_KEY]["profile_id"],
            TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO
        );
        assert_eq!(
            outcome.fingerprint[CODEX_TRANSPORT_PROFILE_KEY]["backend"],
            TRANSPORT_BACKEND_REQWEST_DEFAULT_TLS
        );
        assert_eq!(
            outcome.fingerprint[CODEX_CLIENT_PROFILE_KEY]["transport_profile_id"],
            TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO
        );
    }

    #[test]
    fn read_only_resolution_keeps_legacy_transport_until_materialized() {
        let fingerprint = json!({
            "transport_profile": {
                "profile_id": "codex-reqwest-rustls-auto",
                "backend": "reqwest_rustls",
                "http_mode": "auto",
                "pool_scope": "key"
            }
        });

        let profile = resolve_codex_concrete_account_profile(
            Some(&fingerprint),
            Some(r#"{"account_id":"acc-1"}"#),
            "key-1",
            "name-1",
            "codex-tui/0.142.0 test",
            "codex-tui",
        )
        .expect("profile should resolve");

        assert_eq!(
            profile.fingerprint_hash,
            codex_concrete_profile_hash(
                "codex-tui/0.142.0 test",
                "codex-tui",
                profile.installation_id.as_str(),
                &profile.workspace_identity,
                TRANSPORT_PROFILE_CODEX_LEGACY_REQWEST_RUSTLS_AUTO,
                None,
            )
        );
    }

    fn test_workspace_identity() -> CodexWorkspaceIdentity {
        CodexWorkspaceIdentity {
            user_name: "quinnvale".to_string(),
            remote_owner: "quinnvale".to_string(),
            source: CodexWorkspaceIdentitySource::AuthEmail,
        }
    }

    fn test_profile(user_agent: &str) -> CodexConcreteAccountProfile {
        CodexConcreteAccountProfile {
            user_agent: user_agent.to_string(),
            originator: "codex-tui".to_string(),
            installation_id: "019f0a27-08f6-47d2-ba0b-1ff45470ee76".to_string(),
            workspace_identity: test_workspace_identity(),
            fingerprint_hash: "sha256:hash".to_string(),
        }
    }

    /// Shaped like the blob the operator captured on 2026-09-11; the developer,
    /// paths, remote, commit and ids are synthetic stand-ins.
    const LEAKING_TURN_METADATA: &str = r#"{"installation_id":"7d3f1a2b-9c4e-4f60-8a1b-2c3d4e5f6071","session_id":"01a078d0-a8e5-7c21-9d3e-4f5a6b7c8d9e","thread_id":"01a078d0-a8e5-7c21-9d3e-4f5a6b7c8d9e","agent_name":"/root","turn_id":"01a08e51-5902-7e4f-8a1b-2c3d4e5f6a7b","request_kind":"turn","sandbox":"none","sandbox_mode":"danger-full-access","workspaces":{"/Users/quinn/Projects/ledger":{"associated_remote_urls":{"origin":"git@github.com:QuinnVale/ledger-copilot.git"},"latest_git_commit_hash":"4c1d9e2f7a3b58c6d0e1f2a3b4c5d6e7f8091a2b","has_changes":true}},"turn_started_at_unix_ms":1789094091010}"#;

    fn assert_no_downstream_workspace_leak(serialized: &str) {
        // The home directory is matched with its separator so a synthetic
        // developer whose name merely starts with the real one is not a hit.
        for leaked in [
            "/Users/quinn/",
            "/home/quinn/",
            r"C:\Users\quinn\",
            "Projects/ledger\"",
            "QuinnVale",
            "ledger-copilot",
            "4c1d9e2f7a3b58c6d0e1f2a3b4c5d6e7f8091a2b",
        ] {
            assert!(
                !serialized.contains(leaked),
                "{leaked} leaked in {serialized}"
            );
        }
    }

    #[test]
    fn workspace_identity_derives_home_and_owner_from_auth_email() {
        for (email, user_name, owner) in [
            ("QuinnVale@example.com", "quinnvale", "quinnvale"),
            ("john.smith+codex@company.io", "johnsmith", "john-smith"),
            ("mary_jane.w@example.org", "maryjanew", "mary-jane-w"),
            ("  Dev.Ops--Team@x.dev ", "devopsteam", "dev-ops-team"),
        ] {
            let identity = workspace_identity_from_email(email.trim())
                .unwrap_or_else(|| panic!("{email} should derive"));
            assert_eq!(identity.user_name, user_name, "{email}");
            assert_eq!(identity.remote_owner, owner, "{email}");
            assert_eq!(identity.source, CodexWorkspaceIdentitySource::AuthEmail);
        }
        // Numeric mailboxes and non-ASCII local parts do not make a name.
        assert!(workspace_identity_from_email("1234567890@example.com").is_none());
        assert!(workspace_identity_from_email("用户@example.com").is_none());

        assert_eq!(
            codex_auth_email(Some(r#"{"account_id":"acc","email":" Dev@Example.com "}"#)),
            Some("dev@example.com".to_string())
        );
        assert_eq!(
            codex_auth_email(Some(r#"{"oauth_email":"imported@example.com"}"#)),
            Some("imported@example.com".to_string())
        );
        assert_eq!(codex_auth_email(Some(r#"{"email":"not-an-email"}"#)), None);
        assert_eq!(codex_auth_email(Some(r#"{"account_id":"acc"}"#)), None);
    }

    #[test]
    fn fallback_workspace_identity_is_deterministic_per_selection() {
        let selection =
            codex_profile_selection_identity(Some(r#"{"account_id":"acc-1"}"#), "name", "key");
        let first = fallback_workspace_identity(&selection);
        let second = fallback_workspace_identity(&selection);
        assert_eq!(first, second);
        assert_eq!(first.source, CodexWorkspaceIdentitySource::Fallback);
        assert_eq!(first.remote_owner, first.user_name);
        let letters = first
            .user_name
            .trim_end_matches(|c: char| c.is_ascii_digit());
        assert!(
            WORKSPACE_FALLBACK_USER_NAMES.contains(&letters),
            "{}",
            first.user_name
        );
        assert!(first.user_name.len() - letters.len() <= 2);

        // Other accounts get their own developer (not one name for the pool).
        let distinct = (0..64).any(|index| {
            let selection = codex_profile_selection_identity(
                Some(&format!(r#"{{"account_id":"acc-{index}"}}"#)),
                "name",
                "key",
            );
            fallback_workspace_identity(&selection) != first
        });
        assert!(distinct);
    }

    #[test]
    fn materialization_persists_workspace_identity_and_upgrades_fallback_once() {
        let materialize = |fingerprint: Option<&Value>, auth_config_raw: &str| {
            materialize_codex_key_fingerprint(CodexProfileMaterializeInput {
                provider_type: "codex",
                fingerprint,
                auth_config_raw: Some(auth_config_raw),
                key_id: "key-1",
                key_name: "name-1",
                user_agent: "codex-tui/0.142.0 test",
                originator: "codex-tui",
                now_unix_secs: 1_760_000_000,
            })
            .expect("codex profile should materialize")
            .fingerprint
        };

        // Imported without an e-mail: fallback developer.
        let first = materialize(None, r#"{"account_id":"acc-1"}"#);
        let first_identity = &first[CODEX_CLIENT_PROFILE_KEY]["workspace_identity"];
        assert_eq!(first_identity["source"], "fallback");

        // The auth config gains an e-mail (re-login): upgrade to the e-mail name.
        let second = materialize(
            Some(&first),
            r#"{"account_id":"acc-1","email":"Quinn.Vale@example.com"}"#,
        );
        let second_identity = &second[CODEX_CLIENT_PROFILE_KEY]["workspace_identity"];
        assert_eq!(second_identity["source"], "auth_email");
        assert_eq!(second_identity["user_name"], "quinnvale");
        assert_eq!(second_identity["remote_owner"], "quinn-vale");
        assert_ne!(
            first[CODEX_CLIENT_PROFILE_KEY]["fingerprint_hash"],
            second[CODEX_CLIENT_PROFILE_KEY]["fingerprint_hash"]
        );

        // A later e-mail change does not move the developer again.
        let third = materialize(
            Some(&second),
            r#"{"account_id":"acc-1","email":"other@example.com"}"#,
        );
        assert_eq!(
            third[CODEX_CLIENT_PROFILE_KEY]["workspace_identity"],
            *second_identity
        );

        // Read-only resolution presents the persisted identity.
        let resolved = resolve_codex_concrete_account_profile(
            Some(&third),
            Some(r#"{"account_id":"acc-1","email":"other@example.com"}"#),
            "key-1",
            "name-1",
            "codex-tui/0.142.0 test",
            "codex-tui",
        )
        .expect("profile should resolve");
        assert_eq!(resolved.workspace_identity.user_name, "quinnvale");
        assert_eq!(resolved.workspace_identity.remote_owner, "quinn-vale");
        assert_eq!(
            resolved.fingerprint_hash,
            third[CODEX_CLIENT_PROFILE_KEY]["fingerprint_hash"]
        );

        // A legacy profile without the key derives live from the e-mail, which
        // is exactly what its next refresh persists.
        let mut legacy = second.clone();
        legacy[CODEX_CLIENT_PROFILE_KEY]
            .as_object_mut()
            .expect("profile object")
            .remove("workspace_identity");
        let legacy_resolved = resolve_codex_concrete_account_profile(
            Some(&legacy),
            Some(r#"{"account_id":"acc-1","email":"Quinn.Vale@example.com"}"#),
            "key-1",
            "name-1",
            "codex-tui/0.142.0 test",
            "codex-tui",
        )
        .expect("profile should resolve");
        assert_eq!(
            legacy_resolved.workspace_identity,
            resolved.workspace_identity
        );
    }

    #[test]
    fn rewrites_workspaces_with_the_account_developer_on_every_os_layout() {
        let now = 1_789_094_091;
        for (user_agent, root_prefix, remote_owner_prefixes) in [
            (
                "codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1",
                "/Users/quinnvale/",
                ["git@github.com:quinnvale/", "https://github.com/quinnvale/"],
            ),
            (
                "codex-tui/0.153.4 (Windows 10.0.26200; x86_64) WindowsTerminal (codex-tui; 0.153.4)",
                "C:\\Users\\quinnvale\\",
                ["git@github.com:quinnvale/", "https://github.com/quinnvale/"],
            ),
            (
                "codex-tui/0.153.4 (Linux 6.8.0; x86_64) unknown",
                "/home/quinnvale/",
                ["git@github.com:quinnvale/", "https://github.com/quinnvale/"],
            ),
        ] {
            let profile = test_profile(user_agent);
            let rewritten =
                rewrite_turn_metadata_for_profile_string(LEAKING_TURN_METADATA, &profile, now)
                    .expect("blob should rewrite");
            assert!(rewritten.is_ascii());
            assert_no_downstream_workspace_leak(&rewritten);
            let blob = serde_json::from_str::<Value>(&rewritten).expect("json");
            assert_eq!(blob["installation_id"], profile.installation_id);
            // Everything the profile pass does not own is untouched.
            assert_eq!(blob["session_id"], "01a078d0-a8e5-7c21-9d3e-4f5a6b7c8d9e");
            assert_eq!(blob["agent_name"], "/root");
            assert_eq!(blob["turn_started_at_unix_ms"], 1_789_094_091_010_u64);

            let workspaces = blob["workspaces"].as_object().expect("workspaces object");
            assert_eq!(workspaces.len(), 1, "{user_agent}");
            let (root, entry) = workspaces.iter().next().expect("one workspace");
            assert!(root.starts_with(root_prefix), "{user_agent}: {root}");
            let repo = root.rsplit(['/', '\\']).next().expect("repo name");
            assert!(WORKSPACE_REPO_NAMES.contains(&repo), "{root}");
            // Field order follows codex-rs TurnMetadataWorkspace.
            assert_eq!(
                entry.as_object().expect("entry").keys().collect::<Vec<_>>(),
                ["associated_remote_urls", "latest_git_commit_hash", "has_changes"]
            );
            let origin = entry["associated_remote_urls"]["origin"]
                .as_str()
                .expect("origin");
            assert!(
                remote_owner_prefixes.iter().any(|prefix| origin.starts_with(prefix)),
                "{origin}"
            );
            assert_eq!(entry["associated_remote_urls"].as_object().unwrap().len(), 1);
            assert!(origin.ends_with(&format!("/{repo}.git")), "{origin}");
            let commit = entry["latest_git_commit_hash"].as_str().expect("commit");
            assert_eq!(commit.len(), 40);
            assert!(commit.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            // The dirty flag is the layout's, not the downstream checkout's.
            let layout = CodexWorkspaceLayout::for_profile(&profile);
            assert_eq!(
                entry["has_changes"],
                Value::Bool(layout.has_changes(repo, now)),
                "{user_agent}"
            );

            // Deterministic: the same inbound root presents the same workspace.
            let again =
                rewrite_turn_metadata_for_profile_string(LEAKING_TURN_METADATA, &profile, now)
                    .expect("blob should rewrite");
            assert_eq!(again, rewritten);
        }
    }

    /// A fixed zone keeps the day boundary independent of the host the tests
    /// run on.
    const TEST_TZ: Tz = chrono_tz::America::New_York;

    fn test_layout(profile: &CodexConcreteAccountProfile) -> CodexWorkspaceLayout<'_> {
        CodexWorkspaceLayout::for_profile_in(profile, TEST_TZ)
    }

    #[test]
    fn synthetic_workspace_layout_is_per_account_and_bounded() {
        let mac = "codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1";
        let profile = test_profile(mac);
        let layout = test_layout(&profile);
        let slots = layout.repos.len() as u64;
        assert!((WORKSPACE_MIN_REPOS..=WORKSPACE_MAX_REPOS).contains(&slots));
        let distinct = layout
            .repos
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len() as u64, slots, "{:?}", layout.repos);
        assert!(WORKSPACE_UNIX_PARENT_DIRS.contains(&layout.parent_dir));
        assert!(
            (WORKSPACE_DAY_START_MIN_SECS..=WORKSPACE_DAY_START_MAX_SECS)
                .contains(&layout.day_start_secs)
        );
        assert!((WORKSPACE_PRIMARY_MIN_DAYS..=WORKSPACE_PRIMARY_MAX_DAYS)
            .contains(&layout.primary_period_days));
        assert!(layout.primary_phase_days < layout.primary_period_days);
        assert!(
            (WORKSPACE_SECONDARY_MIN_DAYS..=WORKSPACE_SECONDARY_MAX_DAYS)
                .contains(&layout.secondary_period_days)
        );
        assert!(layout.secondary_phase_days < layout.secondary_period_days);
        for repo in &layout.repos {
            let (period, phase) = layout.commit_cadence(repo);
            assert!(WORKSPACE_COMMIT_PERIODS_SECS.contains(&period), "{repo}");
            assert!(phase < period, "{repo}");
        }

        // Another account (another install) lays its repositories out differently
        // somewhere in these choices, and never shares the seed-derived commit.
        let mut other = test_profile(mac);
        other.installation_id = "6d2f8c1a-2b6e-4d7f-9a1b-3c4d5e6f7a8b".to_string();
        let other_layout = test_layout(&other);
        assert_ne!(
            layout.commit_hash("core", 1_789_094_091),
            other_layout.commit_hash("core", 1_789_094_091)
        );
    }

    #[test]
    fn at_most_two_repositories_are_active_per_developer_day() {
        let profile = test_profile("codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1");
        let layout = test_layout(&profile);
        let roots = (0..200)
            .map(|index| format!("/Users/user{index}/proj{index}"))
            .collect::<Vec<_>>();

        // Walk 60 developer-days in 10-minute steps: every day shows at most
        // two repositories, both from the account's own set, and both lanes
        // are really used over time.
        let start = 1_789_094_091_u64;
        let mut first_day = None;
        let mut days_seen =
            std::collections::BTreeMap::<u64, std::collections::BTreeSet<&str>>::new();
        let mut now = start;
        while now < start + 60 * SECS_PER_DAY {
            let day = layout.local_day_index(now);
            first_day.get_or_insert(day);
            let (primary, secondary) = layout.active_repos(now);
            assert_ne!(primary, secondary);
            let today = days_seen.entry(day).or_default();
            for root in &roots {
                let repo = layout.repo_for_inbound_root(root, now);
                assert!(layout.repos.contains(&repo), "{repo}");
                today.insert(repo);
            }
            assert!(
                today.len() <= WORKSPACE_ACTIVE_REPOS_PER_DAY,
                "day {day}: {today:?}"
            );
            now += 600;
        }
        assert!(days_seen.len() >= 59, "{}", days_seen.len());
        assert!(days_seen
            .values()
            .all(|repos| repos.len() == WORKSPACE_ACTIVE_REPOS_PER_DAY));
        // The whole set is visited over two months, so the account is not
        // stuck on one pair forever.
        let all = days_seen
            .values()
            .flatten()
            .collect::<std::collections::BTreeSet<_>>();
        assert!(all.len() >= 3, "{all:?}");
        // Consecutive days do not always change the main project.
        let primaries = days_seen
            .keys()
            .map(|day| layout.active_repos(day * SECS_PER_DAY + layout.day_start_secs + 12 * 3_600))
            .map(|(primary, _)| primary)
            .collect::<Vec<_>>();
        assert!(
            primaries.windows(2).any(|pair| pair[0] == pair[1]),
            "{primaries:?}"
        );
        assert!(
            primaries.windows(2).any(|pair| pair[0] != pair[1]),
            "{primaries:?}"
        );
    }

    #[test]
    fn developer_day_boundary_follows_the_gateway_zone_and_the_account_phase() {
        let profile = test_profile("codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1");
        let layout = test_layout(&profile);
        // 2026-09-11 00:00:00 America/New_York (EDT, UTC-4) = 04:00:00 UTC.
        let local_midnight = 1_789_099_200_u64;
        let boundary = local_midnight + layout.day_start_secs;
        assert_eq!(
            layout.local_day_index(boundary - 1) + 1,
            layout.local_day_index(boundary)
        );
        // Between two boundaries the day index does not move.
        assert_eq!(
            layout.local_day_index(boundary),
            layout.local_day_index(boundary + SECS_PER_DAY - 1)
        );
        // UTC midnight is not a boundary for this zone.
        let utc_midnight = 1_789_084_800_u64;
        assert_eq!(
            layout.local_day_index(utc_midnight - 1),
            layout.local_day_index(utc_midnight)
        );
        // The same account in UTC counts a different day around the boundary.
        let utc_layout = CodexWorkspaceLayout::for_profile_in(&profile, chrono_tz::UTC);
        assert_eq!(
            utc_layout.local_day_index(utc_midnight + layout.day_start_secs - 1) + 1,
            utc_layout.local_day_index(utc_midnight + layout.day_start_secs)
        );
    }

    #[test]
    fn synthetic_commit_hash_is_stable_within_a_period_and_moves_after_it() {
        let profile = test_profile("codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1");
        let layout = test_layout(&profile);
        let now = 1_789_094_091;
        let (period, phase) = layout.commit_cadence("core");
        // Inside one period: the same commit, aligned to the repository's phase.
        let period_start = (now + phase) / period * period - phase;
        assert_eq!(
            layout.commit_hash("core", period_start),
            layout.commit_hash("core", period_start + period - 1)
        );
        assert_ne!(
            layout.commit_hash("core", period_start),
            layout.commit_hash("core", period_start + period)
        );
        // Repositories move independently: different commits, and over the
        // account's own set not every repository shares one cadence.
        assert_ne!(
            layout.commit_hash("core", now),
            layout.commit_hash("docs", now)
        );
        let cadences = layout
            .repos
            .iter()
            .map(|repo| layout.commit_cadence(repo))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(cadences.len() > 1, "{cadences:?}");
    }

    #[test]
    fn dirty_flag_is_clean_right_after_a_commit_then_dirty_until_the_next() {
        let profile = test_profile("codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1");
        let layout = test_layout(&profile);
        let now = 1_789_094_091;
        let (period, phase) = layout.commit_cadence("core");
        let period_start = (now + phase) / period * period - phase;
        assert!(!layout.has_changes("core", period_start));
        assert!(!layout.has_changes(
            "core",
            period_start + period * WORKSPACE_CLEAN_WINDOW_MIN_PERMILLE / 1_000 - 1
        ));
        assert!(layout.has_changes(
            "core",
            period_start + period * WORKSPACE_CLEAN_WINDOW_MAX_PERMILLE / 1_000
        ));
        assert!(layout.has_changes("core", period_start + period - 1));
        // Monotonic inside the period: once dirty, dirty until the commit.
        let mut seen_dirty = false;
        for step in (0..period).step_by((period / 200).max(1) as usize) {
            let dirty = layout.has_changes("core", period_start + step);
            assert!(!(seen_dirty && !dirty), "step {step}");
            seen_dirty |= dirty;
        }
    }

    #[test]
    fn workspaces_rewrite_mirrors_inbound_field_presence_and_skips_absent_maps() {
        let profile = test_profile("codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1");
        let now = 1_789_094_091;

        // A checkout without remotes keeps its shape: only `has_changes`.
        let rewritten = rewrite_turn_metadata_for_profile_string(
            r#"{"installation_id":"old","workspaces":{"/home/me/private":{"has_changes":false}}}"#,
            &profile,
            now,
        )
        .expect("rewrite");
        let blob = serde_json::from_str::<Value>(&rewritten).expect("json");
        let (root, entry) = blob["workspaces"]
            .as_object()
            .expect("workspaces")
            .iter()
            .next()
            .expect("entry");
        assert!(root.starts_with("/Users/quinnvale/"), "{root}");
        assert_eq!(
            entry.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["has_changes"]
        );
        assert!(entry["has_changes"].is_boolean());
        assert!(!rewritten.contains("/home/me/private"));

        // Two inbound roots: two synthetic entries, sorted like a BTreeMap.
        let rewritten = rewrite_turn_metadata_for_profile_string(
            r#"{"installation_id":"old","workspaces":{"/a":{"latest_git_commit_hash":"1"},"/b":{"latest_git_commit_hash":"2"}}}"#,
            &profile,
            now,
        )
        .expect("rewrite");
        let blob = serde_json::from_str::<Value>(&rewritten).expect("json");
        let roots = blob["workspaces"]
            .as_object()
            .expect("workspaces")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut sorted = roots.clone();
        sorted.sort();
        assert_eq!(roots, sorted);
        for entry in blob["workspaces"].as_object().unwrap().values() {
            assert_eq!(
                entry.as_object().unwrap().keys().collect::<Vec<_>>(),
                ["latest_git_commit_hash"]
            );
        }

        // No `workspaces` (prewarm, compaction, old clients): none is invented.
        let rewritten = rewrite_turn_metadata_for_profile_string(
            r#"{"installation_id":"old","session_id":"sess"}"#,
            &profile,
            now,
        )
        .expect("rewrite");
        let blob = serde_json::from_str::<Value>(&rewritten).expect("json");
        assert!(!blob.as_object().unwrap().contains_key("workspaces"));

        // An empty or non-object map carries nothing and stays as sent.
        for raw in [
            r#"{"installation_id":"old","workspaces":{}}"#,
            r#"{"installation_id":"old","workspaces":null}"#,
        ] {
            let rewritten =
                rewrite_turn_metadata_for_profile_string(raw, &profile, now).expect("rewrite");
            let blob = serde_json::from_str::<Value>(&rewritten).expect("json");
            let expected = serde_json::from_str::<Value>(raw).expect("json");
            assert_eq!(blob["workspaces"], expected["workspaces"], "{raw}");
        }
    }

    #[test]
    fn request_pass_rewrites_workspaces_in_header_and_body_blobs_consistently() {
        let profile = test_profile("codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1");
        let mut headers = BTreeMap::from([(
            "X-Codex-Turn-Metadata".to_string(),
            LEAKING_TURN_METADATA.to_string(),
        )]);
        let inbound_blob = serde_json::from_str::<Value>(LEAKING_TURN_METADATA).expect("json");
        let mut body = json!({
            "input": [{"content": [{"type": "input_text", "text": "<environment_context><cwd>/Users/quinn/Projects/ledger</cwd></environment_context>"}]}],
            "client_metadata": {
                "x-codex-turn-metadata": LEAKING_TURN_METADATA,
                "session_id": "01a078d0-a8e5-7c21-9d3e-4f5a6b7c8d9e"
            }
        });

        apply_codex_concrete_account_profile_to_request_at(
            &mut headers,
            &mut body,
            &profile,
            CodexProfileRequestBodyPolicy::NormalizeClientMetadata,
            1_789_094_091,
        );

        let header_blob = headers.get("X-Codex-Turn-Metadata").expect("header kept");
        let body_blob = body["client_metadata"]["x-codex-turn-metadata"]
            .as_str()
            .expect("body blob");
        assert_eq!(header_blob, body_blob);
        assert_no_downstream_workspace_leak(header_blob);
        let header_blob = serde_json::from_str::<Value>(header_blob).expect("json");
        assert_ne!(header_blob["workspaces"], inbound_blob["workspaces"]);
        assert_eq!(header_blob["session_id"], inbound_blob["session_id"]);
        // The profile pass does not touch the prompt; `<cwd>` is out of scope here.
        assert_eq!(
            body["input"][0]["content"][0]["text"],
            "<environment_context><cwd>/Users/quinn/Projects/ledger</cwd></environment_context>"
        );

        // Object-form blob in the body (a client that does not stringify).
        let mut body = json!({
            "client_metadata": { "x-codex-turn-metadata": inbound_blob.clone() }
        });
        let mut headers = BTreeMap::new();
        apply_codex_concrete_account_profile_to_request_at(
            &mut headers,
            &mut body,
            &profile,
            CodexProfileRequestBodyPolicy::NormalizeClientMetadata,
            1_789_094_091,
        );
        let object_blob = &body["client_metadata"]["x-codex-turn-metadata"];
        assert!(object_blob.is_object());
        assert_no_downstream_workspace_leak(&object_blob.to_string());
        assert_eq!(object_blob["workspaces"], header_blob["workspaces"]);

        // Standalone Search header and the WS handshake normalizer share the rewrite.
        let mut headers = BTreeMap::from([(
            "x-codex-turn-metadata".to_string(),
            LEAKING_TURN_METADATA.to_string(),
        )]);
        apply_codex_concrete_account_profile_to_search_headers_at(
            &mut headers,
            &profile,
            1_789_094_091,
        );
        let search_blob = headers.get("x-codex-turn-metadata").expect("search header");
        assert_no_downstream_workspace_leak(search_blob);
        assert_eq!(
            serde_json::from_str::<Value>(search_blob).expect("json")["workspaces"],
            header_blob["workspaces"]
        );
        let ws_blob = normalize_codex_turn_metadata_for_profile(LEAKING_TURN_METADATA, &profile)
            .expect("ws handshake blob");
        assert_no_downstream_workspace_leak(&ws_blob);
    }

    #[test]
    fn client_version_follows_user_agent_for_every_official_originator_shape() {
        for (user_agent, expected) in [
            (
                "codex-tui/0.153.4 (Windows 10.0.26200; x86_64) WindowsTerminal (codex-tui; 0.153.4)",
                Some("0.153.4"),
            ),
            (
                "Codex Desktop/0.153.1 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.901.31953)",
                Some("0.153.1"),
            ),
            (
                "Codex Desktop/0.153.0-alpha.5 (Windows 10.0.19045; x86_64) unknown (Codex Desktop; 26.901.20858)",
                Some("0.153.0-alpha.5"),
            ),
            (
                "codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1",
                Some("0.153.4"),
            ),
            (
                "codex_vscode/0.153.0 (Windows 10.0.26200; x86_64) unknown (VS Code; 26.901.22334)",
                Some("0.153.0"),
            ),
            ("codex-tui/0.142.0", Some("0.142.0")),
            ("codex-tui/ (Linux; x86_64)", None),
            ("not a codex agent", None),
        ] {
            assert_eq!(
                codex_client_version_from_user_agent(user_agent).as_deref(),
                expected,
                "user-agent {user_agent:?}"
            );
        }
    }

    #[test]
    fn identity_headers_rewrite_version_with_user_agent_and_drop_it_when_unparsable() {
        // A real client sent 0.153.4; the pool profile presents a Desktop build.
        let mut headers = BTreeMap::from([
            (
                "user-agent".to_string(),
                "codex-tui/0.153.4 (Linux; x86_64)".to_string(),
            ),
            ("originator".to_string(), "codex-tui".to_string()),
            ("Version".to_string(), "0.153.4".to_string()),
        ]);
        apply_codex_client_identity_headers(
            &mut headers,
            "Codex Desktop/0.153.1 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.901.31953)",
            "Codex Desktop",
        );
        assert_eq!(
            headers.get("originator").map(String::as_str),
            Some("Codex Desktop")
        );
        assert_eq!(headers.get("version").map(String::as_str), Some("0.153.1"));
        assert!(!headers.contains_key("Version"));

        // Downstream relays send no `version`; a real codex-rs build always does.
        let mut headers = BTreeMap::new();
        apply_codex_client_identity_headers(
            &mut headers,
            "codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1",
            "codex_cli_rs",
        );
        assert_eq!(headers.get("version").map(String::as_str), Some("0.153.4"));

        // Unparsable user-agent: no version rather than a contradictory one.
        let mut headers = BTreeMap::from([("version".to_string(), "0.153.4".to_string())]);
        apply_codex_client_identity_headers(&mut headers, "custom agent", "codex-tui");
        assert!(!headers.contains_key("version"));
    }

    #[test]
    fn normalizes_installation_id_without_touching_runtime_or_prompt_fields() {
        let profile = test_profile("ua");
        let mut headers = BTreeMap::from([
            (
                "x-codex-installation-id".to_string(),
                "old-installation".to_string(),
            ),
            (
                "x-codex-turn-metadata".to_string(),
                r#"{"installation_id":"old","session_id":"sess","thread_id":"thread","turn_id":"turn","window_id":"window"}"#.to_string(),
            ),
        ]);
        let instructions = "do not mutate";
        let input_text = "<environment_context><cwd>/Users/alice/repo</cwd></environment_context>";
        let mut body = json!({
            "instructions": instructions,
            "input": [{"content": [{"type": "input_text", "text": input_text}]}],
            "client_metadata": {
                "x-codex-installation-id": "old-installation",
                "session_id": "sess",
                "thread_id": "thread",
                "x-codex-window-id": "window",
                "x-codex-turn-metadata": "{\"installation_id\":\"old\",\"session_id\":\"sess\",\"thread_id\":\"thread\",\"turn_id\":\"turn\",\"window_id\":\"window\"}"
            }
        });

        apply_codex_concrete_account_profile_to_request(&mut headers, &mut body, &profile);

        assert_eq!(
            headers.get("x-codex-installation-id").map(String::as_str),
            Some("019f0a27-08f6-47d2-ba0b-1ff45470ee76")
        );
        let header_metadata = serde_json::from_str::<Value>(
            headers
                .get("x-codex-turn-metadata")
                .expect("turn metadata header"),
        )
        .expect("header metadata json");
        assert_eq!(
            header_metadata["installation_id"],
            "019f0a27-08f6-47d2-ba0b-1ff45470ee76"
        );
        assert_eq!(header_metadata["session_id"], "sess");
        assert_eq!(
            body["client_metadata"]["x-codex-installation-id"],
            "019f0a27-08f6-47d2-ba0b-1ff45470ee76"
        );
        let body_metadata = serde_json::from_str::<Value>(
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .expect("body turn metadata"),
        )
        .expect("body metadata json");
        assert_eq!(
            body_metadata["installation_id"],
            "019f0a27-08f6-47d2-ba0b-1ff45470ee76"
        );
        assert_eq!(body_metadata["session_id"], "sess");
        assert_eq!(body_metadata["thread_id"], "thread");
        assert_eq!(body_metadata["turn_id"], "turn");
        assert_eq!(body_metadata["window_id"], "window");
        assert_eq!(body["instructions"], instructions);
        assert_eq!(body["input"][0]["content"][0]["text"], input_text);
    }

    #[test]
    fn turn_metadata_normalization_ascii_escapes_unicode_for_http_headers() {
        let profile = test_profile("ua");
        let original = r#"{"installation_id":"old","cwd":"/workspace/\u9879\u76ee\ud83d\ude80","label":"caf\u00e9","delete":"\u007f","\u8def\u5f84":"value"}"#;
        let expected =
            serde_json::from_str::<Value>(original).expect("source metadata should parse");

        let rewritten = normalize_codex_turn_metadata_for_profile(original, &profile)
            .expect("turn metadata should normalize");

        assert!(rewritten.is_ascii());
        assert!(rewritten.contains(r#"\u9879\u76ee\ud83d\ude80"#));
        assert!(rewritten.contains(r#"caf\u00e9"#));
        assert!(rewritten.contains(r#"\u007f"#));
        assert!(http::HeaderValue::from_str(&rewritten)
            .expect("rewritten metadata should be a valid header value")
            .to_str()
            .is_ok());
        let actual =
            serde_json::from_str::<Value>(&rewritten).expect("rewritten metadata should parse");
        assert_eq!(actual["installation_id"], profile.installation_id);
        assert_eq!(actual["cwd"], expected["cwd"]);
        assert_eq!(actual["label"], expected["label"]);
        assert_eq!(actual["delete"], expected["delete"]);
        assert_eq!(actual["\u{8def}\u{5f84}"], expected["\u{8def}\u{5f84}"]);
    }

    #[test]
    fn injects_profile_installation_id_when_request_omits_codex_metadata() {
        let profile = test_profile("ua");
        let mut headers = BTreeMap::new();
        let instructions = "do not mutate";
        let input_text = "<environment_context><cwd>/Users/alice/repo</cwd></environment_context>";
        let mut body = json!({
            "instructions": instructions,
            "input": [{"content": [{"type": "input_text", "text": input_text}]}]
        });

        apply_codex_concrete_account_profile_to_request(&mut headers, &mut body, &profile);

        assert_eq!(
            headers.get("x-codex-installation-id").map(String::as_str),
            Some("019f0a27-08f6-47d2-ba0b-1ff45470ee76")
        );
        assert_eq!(
            body["client_metadata"]["x-codex-installation-id"],
            "019f0a27-08f6-47d2-ba0b-1ff45470ee76"
        );
        assert_eq!(body["instructions"], instructions);
        assert_eq!(body["input"][0]["content"][0]["text"], input_text);
    }
}
