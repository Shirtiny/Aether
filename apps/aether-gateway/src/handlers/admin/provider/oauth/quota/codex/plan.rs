use super::super::shared::{
    build_provider_quota_execution_plan, execute_provider_quota_plan,
    resolve_provider_quota_execution_timeouts, ProviderQuotaExecutionOutcome,
};
use crate::handlers::admin::request::{AdminAppState, AdminGatewayProviderTransportSnapshot};
use crate::GatewayError;
use aether_contracts::{
    ProxySnapshot, EXECUTION_COOKIE_JAR_CHATGPT_CLOUDFLARE,
    EXECUTION_HEADER_ORDER_CODEX_BACKEND_CLIENT, EXECUTION_REQUEST_COOKIE_JAR_HEADER,
    EXECUTION_REQUEST_HEADER_ORDER_HEADER,
};
use aether_provider_pool::build_codex_pool_reset_credits_request;
use aether_provider_pool::{build_codex_pool_quota_request, ProviderPoolQuotaRequestSpec};
use std::collections::BTreeMap;

pub(super) fn build_codex_quota_request_spec(
    transport: &AdminGatewayProviderTransportSnapshot,
    resolved_oauth_auth: Option<(String, String)>,
) -> Result<ProviderPoolQuotaRequestSpec, String> {
    let auth_config = transport
        .key
        .decrypted_auth_config
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
    build_codex_pool_quota_request(
        &transport.key.id,
        resolved_oauth_auth,
        Some(transport.key.decrypted_api_key.as_str()),
        auth_config.as_ref(),
    )
}

pub(super) fn build_codex_reset_credits_request_spec(
    transport: &AdminGatewayProviderTransportSnapshot,
    resolved_oauth_auth: (String, String),
) -> ProviderPoolQuotaRequestSpec {
    let auth_config = transport
        .key
        .decrypted_auth_config
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
    build_codex_pool_reset_credits_request(
        &transport.key.id,
        resolved_oauth_auth,
        auth_config.as_ref(),
    )
}

/// Aligns a `/backend-api/wham/*` request with the codex-rs `BackendClient`
/// wire shape.
///
/// `apply_codex_pool_stable_client_headers` writes the identity trio
/// (`user-agent`, `originator`, `version`) that `core/src/client.rs` puts on
/// `/backend-api/codex/*`. The wham routes go through `BackendClient` instead,
/// whose `headers()` (`backend-client/src/client.rs:245-265`) sends only the
/// user-agent alongside auth — capture `http-0009` confirms no `originator`
/// and no `version` on the wire. Leaving them on would be a shape no codex
/// client produces, so they come off here rather than being suppressed
/// upstream, where `/responses` still needs them.
fn apply_codex_backend_client_wire_shape(headers: &mut BTreeMap<String, String>) {
    headers.retain(|name, _| {
        !name.eq_ignore_ascii_case("originator") && !name.eq_ignore_ascii_case("version")
    });
    headers.insert(
        EXECUTION_REQUEST_HEADER_ORDER_HEADER.to_string(),
        EXECUTION_HEADER_ORDER_CODEX_BACKEND_CLIENT.to_string(),
    );
    // The real client shares one cookie jar across `/responses` and the wham
    // routes, so the account's Cloudflare cookies must ride along here too.
    headers.insert(
        EXECUTION_REQUEST_COOKIE_JAR_HEADER.to_string(),
        EXECUTION_COOKIE_JAR_CHATGPT_CLOUDFLARE.to_string(),
    );
}

pub(super) async fn execute_codex_quota_plan(
    state: &AdminAppState<'_>,
    transport: &AdminGatewayProviderTransportSnapshot,
    mut spec: ProviderPoolQuotaRequestSpec,
    proxy_override: Option<&ProxySnapshot>,
) -> Result<ProviderQuotaExecutionOutcome, GatewayError> {
    crate::ai_serving::apply_codex_pool_stable_client_headers(&mut spec.headers, transport);
    apply_codex_backend_client_wire_shape(&mut spec.headers);
    let proxy = match proxy_override {
        Some(proxy) => Some(proxy.clone()),
        None => {
            state
                .resolve_transport_proxy_snapshot_with_tunnel_affinity(transport)
                .await
        }
    };
    let timeouts = Some(resolve_provider_quota_execution_timeouts(
        state.resolve_transport_execution_timeouts(transport),
        proxy.as_ref(),
    ));
    let plan = build_provider_quota_execution_plan(
        transport,
        spec,
        proxy,
        state.resolve_transport_profile(transport),
        timeouts,
    );
    execute_provider_quota_plan(state, transport, plan, "codex").await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity trio that `/responses` needs but `BackendClient` never sends.
    #[test]
    fn wham_wire_shape_drops_originator_and_version_and_arms_the_backend_client_order() {
        let mut headers = BTreeMap::from([
            ("accept".to_string(), "*/*".to_string()),
            ("authorization".to_string(), "Bearer t".to_string()),
            ("chatgpt-account-id".to_string(), "acct".to_string()),
            ("originator".to_string(), "codex-tui".to_string()),
            ("user-agent".to_string(), "codex-tui/0.154.0".to_string()),
            ("version".to_string(), "0.154.0".to_string()),
            ("x-openai-codex-luna-reserve".to_string(), "1".to_string()),
        ]);

        apply_codex_backend_client_wire_shape(&mut headers);

        assert!(!headers.contains_key("originator"));
        assert!(!headers.contains_key("version"));
        // The account identity itself must survive: a wham request with no
        // user-agent looks like no codex client at all.
        assert_eq!(
            headers.get("user-agent").map(String::as_str),
            Some("codex-tui/0.154.0")
        );
        assert_eq!(
            headers
                .get(EXECUTION_REQUEST_HEADER_ORDER_HEADER)
                .map(String::as_str),
            Some(EXECUTION_HEADER_ORDER_CODEX_BACKEND_CLIENT)
        );
        assert_eq!(
            headers
                .get(EXECUTION_REQUEST_COOKIE_JAR_HEADER)
                .map(String::as_str),
            Some(EXECUTION_COOKIE_JAR_CHATGPT_CLOUDFLARE)
        );
    }

    #[test]
    fn wham_wire_shape_strips_identity_headers_case_insensitively() {
        let mut headers = BTreeMap::from([
            ("Originator".to_string(), "codex-tui".to_string()),
            ("Version".to_string(), "0.154.0".to_string()),
        ]);

        apply_codex_backend_client_wire_shape(&mut headers);

        assert!(headers
            .keys()
            .all(|name| !name.eq_ignore_ascii_case("originator")
                && !name.eq_ignore_ascii_case("version")));
    }
}
