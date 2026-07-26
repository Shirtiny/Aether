use std::collections::BTreeMap;

use aether_contracts::{
    codex_default_transport_profile_extra, CODEX_DEFAULT_TLS_JA3, CODEX_DEFAULT_TLS_JA3_HASH,
    TRANSPORT_BACKEND_REQWEST_DEFAULT_TLS, TRANSPORT_HTTP_MODE_AUTO, TRANSPORT_POOL_SCOPE_KEY,
    TRANSPORT_PROFILE_CODEX_LEGACY_REQWEST_RUSTLS_AUTO,
    TRANSPORT_PROFILE_CODEX_REQWEST_DEFAULT_TLS_AUTO,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(crate) const CODEX_CLIENT_PROFILE_KEY: &str = "codex_client_profile";
pub(crate) const CODEX_TRANSPORT_PROFILE_KEY: &str = "transport_profile";
const CODEX_PROFILE_SCHEMA_VERSION: u64 = 1;
const X_CODEX_INSTALLATION_ID: &str = "x-codex-installation-id";
const X_CODEX_TURN_METADATA: &str = "x-codex-turn-metadata";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexConcreteAccountProfile {
    pub(crate) user_agent: String,
    pub(crate) originator: String,
    pub(crate) installation_id: String,
    pub(crate) fingerprint_hash: String,
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
        transport_profile_id,
        transport_tls_fingerprint_hash.as_deref(),
    );

    Some(CodexConcreteAccountProfile {
        user_agent: materialized_user_agent,
        originator: materialized_originator,
        installation_id,
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
    provider_request_headers.insert("user-agent".to_string(), profile.user_agent.clone());
    provider_request_headers.insert("originator".to_string(), profile.originator.clone());

    normalize_installation_id_in_headers(provider_request_headers, &profile.installation_id);
    match body_policy {
        CodexProfileRequestBodyPolicy::NormalizeClientMetadata => {
            normalize_installation_id_in_body(provider_request_body, &profile.installation_id);
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
    provider_request_headers.insert("user-agent".to_string(), profile.user_agent.clone());
    provider_request_headers.insert("originator".to_string(), profile.originator.clone());
    remove_header_case_insensitive(provider_request_headers, X_CODEX_INSTALLATION_ID);

    // Standalone Search carries turn metadata as a header and does not use the
    // Responses client_metadata body contract. Preserve the Search payload while
    // keeping its installation identity aligned with the selected pool account.
    if let Some((header_name, metadata)) =
        remove_header_case_insensitive(provider_request_headers, X_CODEX_TURN_METADATA)
    {
        if let Some(rewritten) =
            rewrite_turn_metadata_installation_id_string(&metadata, &profile.installation_id)
        {
            provider_request_headers.insert(header_name, rewritten);
        }
    }
}

pub(crate) fn apply_codex_concrete_account_profile_to_body_with_policy(
    provider_request_body: &mut Value,
    profile: &CodexConcreteAccountProfile,
    body_policy: CodexProfileRequestBodyPolicy,
) {
    match body_policy {
        CodexProfileRequestBodyPolicy::NormalizeClientMetadata => {
            normalize_installation_id_in_body(provider_request_body, &profile.installation_id);
        }
        CodexProfileRequestBodyPolicy::StripClientMetadata => {
            strip_codex_client_metadata_from_body(provider_request_body);
        }
    }
}

fn normalize_installation_id_in_headers(
    provider_request_headers: &mut BTreeMap<String, String>,
    installation_id: &str,
) {
    set_header_value_case_insensitive(
        provider_request_headers,
        X_CODEX_INSTALLATION_ID,
        installation_id,
    );
    if let Some((header_name, metadata)) =
        remove_header_case_insensitive(provider_request_headers, X_CODEX_TURN_METADATA)
    {
        let rewritten = rewrite_turn_metadata_installation_id_string(&metadata, installation_id)
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

fn normalize_installation_id_in_body(provider_request_body: &mut Value, installation_id: &str) {
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
        Value::String(installation_id.to_string()),
    );
    let Some(turn_metadata) = metadata.get_mut(X_CODEX_TURN_METADATA) else {
        return;
    };
    rewrite_turn_metadata_installation_id_value(turn_metadata, installation_id);
}

fn rewrite_turn_metadata_installation_id_value(value: &mut Value, installation_id: &str) -> bool {
    match value {
        Value::String(raw) => {
            let Some(rewritten) =
                rewrite_turn_metadata_installation_id_string(raw, installation_id)
            else {
                return false;
            };
            *raw = rewritten;
            true
        }
        Value::Object(object) => {
            object.insert(
                "installation_id".to_string(),
                Value::String(installation_id.to_string()),
            );
            true
        }
        _ => false,
    }
}

fn rewrite_turn_metadata_installation_id_string(
    raw: &str,
    installation_id: &str,
) -> Option<String> {
    let mut parsed = serde_json::from_str::<Value>(raw).ok()?;
    match parsed.as_object_mut() {
        Some(object) => {
            object.insert(
                "installation_id".to_string(),
                Value::String(installation_id.to_string()),
            );
            serde_json::to_string(&parsed).ok()
        }
        None => None,
    }
}

pub(crate) fn normalize_codex_turn_metadata_for_profile(
    raw: &str,
    profile: &CodexConcreteAccountProfile,
) -> Option<String> {
    rewrite_turn_metadata_installation_id_string(raw, &profile.installation_id)
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

fn codex_auth_account_id(auth_config_raw: Option<&str>) -> Option<String> {
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
    transport_profile_id: &str,
    transport_tls_fingerprint_hash: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aether:codex:concrete-profile:v2");
    hasher.update([0]);
    hasher.update(user_agent.as_bytes());
    hasher.update([0]);
    hasher.update(originator.as_bytes());
    hasher.update([0]);
    hasher.update(installation_id.as_bytes());
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

fn hex_lower(bytes: &[u8]) -> String {
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
        assert_eq!(
            profile["fingerprint_hash"],
            codex_concrete_profile_hash(
                "codex-tui/0.142.0 test",
                "codex-tui",
                installation_id,
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
                TRANSPORT_PROFILE_CODEX_LEGACY_REQWEST_RUSTLS_AUTO,
                None,
            )
        );
    }

    #[test]
    fn normalizes_installation_id_without_touching_runtime_or_prompt_fields() {
        let profile = CodexConcreteAccountProfile {
            user_agent: "ua".to_string(),
            originator: "codex-tui".to_string(),
            installation_id: "019f0a27-08f6-47d2-ba0b-1ff45470ee76".to_string(),
            fingerprint_hash: "sha256:hash".to_string(),
        };
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
    fn injects_profile_installation_id_when_request_omits_codex_metadata() {
        let profile = CodexConcreteAccountProfile {
            user_agent: "ua".to_string(),
            originator: "codex-tui".to_string(),
            installation_id: "019f0a27-08f6-47d2-ba0b-1ff45470ee76".to_string(),
            fingerprint_hash: "sha256:hash".to_string(),
        };
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
