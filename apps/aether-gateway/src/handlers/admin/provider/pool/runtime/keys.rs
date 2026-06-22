pub(super) fn pool_sticky_pattern(provider_id: &str) -> String {
    format!("ap:{provider_id}:sticky:*")
}

pub(super) fn pool_sticky_key(provider_id: &str, session_token: &str) -> String {
    format!("ap:{provider_id}:sticky:{session_token}")
}

pub(super) fn pool_sticky_init_key(provider_id: &str, session_token: &str) -> String {
    format!("ap:{provider_id}:sticky_init:{session_token}")
}

pub(super) fn pool_sticky_prebind_key(
    provider_id: &str,
    session_token: &str,
    owner: &str,
) -> String {
    format!("ap:{provider_id}:sticky_prebind:{session_token}:{owner}")
}

pub(super) fn pool_lru_key(provider_id: &str) -> String {
    format!("ap:{provider_id}:lru")
}

pub(super) fn pool_cooldown_key(provider_id: &str, key_id: &str) -> String {
    format!("ap:{provider_id}:cooldown:{key_id}")
}

pub(super) fn pool_cooldown_index_key(provider_id: &str) -> String {
    format!("ap:{provider_id}:cooldown_idx")
}

pub(super) fn pool_cost_key(provider_id: &str, key_id: &str) -> String {
    format!("ap:{provider_id}:cost:{key_id}")
}

pub(super) fn pool_latency_key(provider_id: &str, key_id: &str) -> String {
    format!("ap:{provider_id}:latency:{key_id}")
}

pub(super) fn pool_stream_timeout_key(provider_id: &str, key_id: &str) -> String {
    format!("ap:{provider_id}:stream_timeout:{key_id}")
}

pub(super) fn parse_pool_cost_member(member: &str) -> u64 {
    member
        .rsplit_once(':')
        .and_then(|(_, suffix)| suffix.parse::<u64>().ok())
        .unwrap_or(0)
}

pub(super) fn parse_pool_latency_member(member: &str) -> u64 {
    member
        .rsplit_once(':')
        .and_then(|(_, suffix)| suffix.parse::<u64>().ok())
        .unwrap_or(0)
}

pub(super) fn pool_cooldown_keys(provider_id: &str, key_ids: &[String]) -> Vec<String> {
    key_ids
        .iter()
        .map(|key_id| pool_cooldown_key(provider_id, key_id))
        .collect()
}

pub(super) fn pool_cost_keys(provider_id: &str, key_ids: &[String]) -> Vec<String> {
    key_ids
        .iter()
        .map(|key_id| pool_cost_key(provider_id, key_id))
        .collect()
}

pub(super) fn pool_latency_keys(provider_id: &str, key_ids: &[String]) -> Vec<String> {
    key_ids
        .iter()
        .map(|key_id| pool_latency_key(provider_id, key_id))
        .collect()
}
