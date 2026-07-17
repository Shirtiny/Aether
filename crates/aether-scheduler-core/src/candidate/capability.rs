use super::types::SchedulerMinimalCandidateSelectionCandidate;

pub const CODEX_OFFICIAL_WS_REQUIRED_CAPABILITY: &str = "codex_official_ws";

#[derive(Debug, Clone, Copy)]
pub(crate) struct RequiredCapabilityDescriptor<'a> {
    pub(crate) name: &'a str,
    pub(crate) compatible: bool,
}

pub fn candidate_supports_required_capability(
    candidate: &SchedulerMinimalCandidateSelectionCandidate,
    required_capability: &str,
) -> bool {
    let required_capability = required_capability.trim();
    if required_capability.is_empty() {
        return true;
    }
    let Some(capabilities) = candidate.key_capabilities.as_ref() else {
        return false;
    };

    if let Some(object) = capabilities.as_object() {
        if object
            .get(required_capability)
            .is_some_and(capability_value_is_enabled)
        {
            return true;
        }
        return object.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case(required_capability) && capability_value_is_enabled(value)
        });
    }

    if let Some(items) = capabilities.as_array() {
        return items.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(required_capability))
        });
    }

    false
}

pub fn candidate_supports_flat_required_capabilities(
    candidate: &SchedulerMinimalCandidateSelectionCandidate,
    required_capabilities: Option<&serde_json::Value>,
) -> bool {
    let Some(required_capabilities) = required_capabilities else {
        return true;
    };
    let Some(required_capabilities) = required_capabilities.as_object() else {
        return false;
    };

    required_capabilities
        .iter()
        .filter(|(_, value)| requested_capability_is_enabled(value))
        .all(|(capability, _)| candidate_supports_required_capability(candidate, capability))
}

pub fn hard_filter_candidates_by_flat_required_capabilities(
    candidates: &mut Vec<SchedulerMinimalCandidateSelectionCandidate>,
    required_capabilities: Option<&serde_json::Value>,
) {
    candidates.retain(|candidate| {
        candidate_supports_flat_required_capabilities(candidate, required_capabilities)
    });
}

/// Applies the account-capability hard gate required by a native Codex WebSocket selection.
/// Call this immediately after enumeration and before runtime-state reads or ranking. Endpoint and
/// immutable transport-profile eligibility must still be checked from the selected provider
/// transport snapshot before dialing upstream.
pub fn hard_filter_candidates_for_codex_official_ws(
    candidates: &mut Vec<SchedulerMinimalCandidateSelectionCandidate>,
) {
    candidates.retain(candidate_supports_codex_official_ws_capability);
}

pub fn candidate_supports_codex_official_ws_capability(
    candidate: &SchedulerMinimalCandidateSelectionCandidate,
) -> bool {
    candidate.provider_type.trim().eq_ignore_ascii_case("codex")
        && candidate.key_auth_type.trim().eq_ignore_ascii_case("oauth")
        && candidate
            .key_capabilities
            .as_ref()
            .and_then(serde_json::Value::as_object)
            .and_then(|capabilities| capabilities.get(CODEX_OFFICIAL_WS_REQUIRED_CAPABILITY))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

pub fn requested_capability_priority_for_candidate(
    required_capabilities: Option<&serde_json::Value>,
    candidate: &SchedulerMinimalCandidateSelectionCandidate,
) -> (u32, u32) {
    let Some(required_capabilities) = required_capabilities.and_then(serde_json::Value::as_object)
    else {
        return (0, 0);
    };

    requested_capability_priority_for_candidate_descriptors(
        required_capabilities
            .iter()
            .filter_map(|(capability, value)| {
                requested_capability_is_enabled(value).then_some(RequiredCapabilityDescriptor {
                    name: capability.as_str(),
                    compatible: requested_capability_is_compatible(capability),
                })
            }),
        candidate,
    )
}

fn requested_capability_priority_for_candidate_descriptors<'a, I>(
    required_capabilities: I,
    candidate: &SchedulerMinimalCandidateSelectionCandidate,
) -> (u32, u32)
where
    I: IntoIterator<Item = RequiredCapabilityDescriptor<'a>>,
{
    let mut exclusive_misses = 0u32;
    let mut compatible_misses = 0u32;
    for capability in required_capabilities {
        if candidate_supports_required_capability(candidate, capability.name) {
            continue;
        }
        if capability.compatible {
            compatible_misses += 1;
        } else {
            exclusive_misses += 1;
        }
    }

    (exclusive_misses, compatible_misses)
}

fn requested_capability_is_enabled(value: &serde_json::Value) -> bool {
    capability_value_is_enabled(value)
}

fn capability_value_is_enabled(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::String(value) => value.eq_ignore_ascii_case("true"),
        serde_json::Value::Number(value) => value.as_i64().is_some_and(|value| value > 0),
        _ => false,
    }
}

fn requested_capability_is_compatible(capability: &str) -> bool {
    matches!(
        capability.trim().to_ascii_lowercase().as_str(),
        "cache_1h" | "context_1m"
    )
}
