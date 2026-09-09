use super::generic::{
    provider_account_state_from_metadata, template_for_provider_type, GenericProviderOAuthAdapter,
};
use crate::provider::ProviderOAuthAdapter;

#[derive(Debug, Clone)]
pub struct CodexProviderOAuthAdapter {
    inner: GenericProviderOAuthAdapter,
}

impl Default for CodexProviderOAuthAdapter {
    fn default() -> Self {
        Self {
            inner: GenericProviderOAuthAdapter::new(
                template_for_provider_type("codex").expect("codex template should exist"),
            ),
        }
    }
}

#[async_trait::async_trait]
impl ProviderOAuthAdapter for CodexProviderOAuthAdapter {
    fn provider_type(&self) -> &'static str {
        self.inner.provider_type()
    }

    fn capabilities(&self) -> crate::provider::ProviderOAuthCapabilities {
        crate::provider::ProviderOAuthCapabilities {
            supports_account_probe: true,
            ..self.inner.capabilities()
        }
    }

    fn build_authorize_url(
        &self,
        ctx: &crate::provider::ProviderOAuthTransportContext,
        state: &str,
        code_challenge: Option<&str>,
    ) -> Result<crate::core::OAuthAuthorizeResponse, crate::core::OAuthError> {
        // The codex template already renders the codex-rs authorize URL shape
        // (parameter order, `%20` scopes, `originator`, no `prompt=login`).
        self.inner.build_authorize_url(ctx, state, code_challenge)
    }

    async fn exchange_code(
        &self,
        executor: &dyn crate::network::OAuthHttpExecutor,
        ctx: &crate::provider::ProviderOAuthTransportContext,
        code: &str,
        state: &str,
        pkce_verifier: Option<&str>,
    ) -> Result<crate::provider::ProviderOAuthTokenSet, crate::core::OAuthError> {
        self.inner
            .exchange_code(executor, ctx, code, state, pkce_verifier)
            .await
    }

    async fn import_credentials(
        &self,
        executor: &dyn crate::network::OAuthHttpExecutor,
        ctx: &crate::provider::ProviderOAuthTransportContext,
        input: crate::provider::ProviderOAuthImportInput,
    ) -> Result<crate::provider::ProviderOAuthTokenSet, crate::core::OAuthError> {
        self.inner.import_credentials(executor, ctx, input).await
    }

    async fn refresh(
        &self,
        executor: &dyn crate::network::OAuthHttpExecutor,
        ctx: &crate::provider::ProviderOAuthTransportContext,
        account: &crate::provider::ProviderOAuthAccount,
    ) -> Result<crate::provider::ProviderOAuthTokenSet, crate::core::OAuthError> {
        self.inner.refresh(executor, ctx, account).await
    }

    fn resolve_request_auth(
        &self,
        account: &crate::provider::ProviderOAuthAccount,
    ) -> Result<crate::provider::ProviderOAuthRequestAuth, crate::core::OAuthError> {
        self.inner.resolve_request_auth(account)
    }

    fn account_fingerprint(
        &self,
        account: &crate::provider::ProviderOAuthAccount,
    ) -> Option<String> {
        self.inner.account_fingerprint(account)
    }

    async fn probe_account_state(
        &self,
        _executor: &dyn crate::network::OAuthHttpExecutor,
        _ctx: &crate::provider::ProviderOAuthTransportContext,
        account: &crate::provider::ProviderOAuthAccount,
    ) -> Result<Option<crate::provider::ProviderOAuthProbeResult>, crate::core::OAuthError> {
        Ok(Some(provider_account_state_from_metadata("codex", account)))
    }
}

#[cfg(test)]
mod tests {
    use super::CodexProviderOAuthAdapter;
    use crate::network::{OAuthHttpExecutor, OAuthHttpRequest, OAuthHttpResponse};
    use crate::provider::{
        ProviderOAuthAccount, ProviderOAuthAdapter, ProviderOAuthTransportContext,
    };
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::BTreeMap;

    struct UnusedExecutor;

    #[async_trait]
    impl OAuthHttpExecutor for UnusedExecutor {
        async fn execute(
            &self,
            _request: OAuthHttpRequest,
        ) -> Result<OAuthHttpResponse, crate::core::OAuthError> {
            unreachable!("metadata probe should not execute network requests")
        }
    }

    fn codex_ctx(originator: Option<&str>) -> ProviderOAuthTransportContext {
        ProviderOAuthTransportContext {
            provider_id: String::new(),
            provider_type: "codex".to_string(),
            endpoint_id: None,
            key_id: None,
            auth_type: Some("oauth".to_string()),
            decrypted_api_key: None,
            decrypted_auth_config: None,
            provider_config: None,
            endpoint_config: None,
            key_config: None,
            network: crate::network::OAuthNetworkContext::provider_operation(None),
            user_agent: Some(
                "codex_cli_rs/0.153.4 (Mac OS 26.5.1; arm64) ghostty/1.3.1".to_string(),
            ),
            originator: originator.map(ToOwned::to_owned),
        }
    }

    /// Records every request and replays canned responses in order.
    struct RecordingExecutor {
        requests: std::sync::Mutex<Vec<OAuthHttpRequest>>,
        responses: std::sync::Mutex<std::collections::VecDeque<OAuthHttpResponse>>,
    }

    impl RecordingExecutor {
        fn new(responses: Vec<OAuthHttpResponse>) -> Self {
            Self {
                requests: std::sync::Mutex::new(Vec::new()),
                responses: std::sync::Mutex::new(responses.into_iter().collect()),
            }
        }

        fn requests(&self) -> Vec<OAuthHttpRequest> {
            self.requests.lock().expect("requests lock").clone()
        }
    }

    #[async_trait]
    impl OAuthHttpExecutor for RecordingExecutor {
        async fn execute(
            &self,
            request: OAuthHttpRequest,
        ) -> Result<OAuthHttpResponse, crate::core::OAuthError> {
            self.requests.lock().expect("requests lock").push(request);
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .ok_or_else(|| crate::core::OAuthError::invalid_response("no canned response"))
        }
    }

    fn json_response(status_code: u16, body: serde_json::Value) -> OAuthHttpResponse {
        OAuthHttpResponse {
            status_code,
            body_text: body.to_string(),
            json_body: Some(body),
        }
    }

    #[test]
    fn codex_authorize_url_matches_codex_rs_shape() {
        let adapter = CodexProviderOAuthAdapter::default();
        let response = adapter
            .build_authorize_url(
                &codex_ctx(Some("codex_cli_rs")),
                "state-1",
                Some("challenge-1"),
            )
            .expect("authorize url should build");

        assert_eq!(
            response.authorize_url,
            "https://auth.openai.com/oauth/authorize?response_type=code\
             &client_id=app_EMoamEEZ73f0CkXaXp7hrann\
             &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback\
             &scope=openid%20profile%20email%20offline_access%20api.connectors.read%20api.connectors.invoke\
             &code_challenge=challenge-1&code_challenge_method=S256\
             &id_token_add_organizations=true&codex_cli_simplified_flow=true\
             &state=state-1&originator=codex_cli_rs"
        );
        assert!(!response.authorize_url.contains("prompt="));
        assert!(!response.authorize_url.contains('+'));
        assert_eq!(response.state, "state-1");
        assert_eq!(response.code_challenge.as_deref(), Some("challenge-1"));
    }

    #[test]
    fn codex_authorize_url_omits_originator_when_unknown() {
        let adapter = CodexProviderOAuthAdapter::default();
        let response = adapter
            .build_authorize_url(&codex_ctx(None), "state-1", Some("challenge-1"))
            .expect("authorize url should build");

        assert!(response.authorize_url.ends_with("&state=state-1"));
        assert!(!response.authorize_url.contains("originator="));
    }

    #[tokio::test]
    async fn codex_exchange_code_sends_codex_rs_form_then_api_key_exchange() {
        let adapter = CodexProviderOAuthAdapter::default();
        let executor = RecordingExecutor::new(vec![
            json_response(
                200,
                json!({
                    "access_token": "access-1",
                    "refresh_token": "refresh-1",
                    "id_token": "id.token.1",
                    "token_type": "Bearer",
                    "expires_in": 3600
                }),
            ),
            json_response(200, json!({ "access_token": "sk-api-1" })),
        ]);

        let result = adapter
            .exchange_code(
                &executor,
                &codex_ctx(Some("codex_cli_rs")),
                "code 1",
                "state-1",
                Some("verifier~1"),
            )
            .await
            .expect("exchange should succeed");
        assert_eq!(result.token_set.access_token, "access-1");
        assert_eq!(result.token_set.refresh_token.as_deref(), Some("refresh-1"));

        let requests = executor.requests();
        assert_eq!(requests.len(), 2, "code exchange then api-key exchange");

        let exchange = &requests[0];
        assert_eq!(exchange.url, "https://auth.openai.com/oauth/token");
        assert_eq!(
            String::from_utf8(exchange.body_bytes.clone().expect("form body")).unwrap(),
            "grant_type=authorization_code&code=code%201\
             &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback\
             &client_id=app_EMoamEEZ73f0CkXaXp7hrann&code_verifier=verifier~1"
        );
        assert!(exchange.json_body.is_none());
        assert_eq!(
            exchange.headers,
            BTreeMap::from([
                (
                    "content-type".to_string(),
                    "application/x-www-form-urlencoded".to_string()
                ),
                ("originator".to_string(), "codex_cli_rs".to_string()),
            ]),
            "no accept header, originator carried"
        );
        assert!(exchange
            .user_agent
            .as_deref()
            .unwrap()
            .starts_with("codex_cli_rs/"));

        let api_key = &requests[1];
        assert_eq!(api_key.url, "https://auth.openai.com/oauth/token");
        assert_eq!(
            String::from_utf8(api_key.body_bytes.clone().expect("form body")).unwrap(),
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange\
             &client_id=app_EMoamEEZ73f0CkXaXp7hrann&requested_token=openai-api-key\
             &subject_token=id.token.1\
             &subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aid_token"
        );
        assert_eq!(api_key.headers, exchange.headers);
    }

    #[tokio::test]
    async fn codex_exchange_code_tolerates_api_key_exchange_failure() {
        let adapter = CodexProviderOAuthAdapter::default();
        let executor = RecordingExecutor::new(vec![
            json_response(
                200,
                json!({
                    "access_token": "access-1",
                    "refresh_token": "refresh-1",
                    "id_token": "id.token.1",
                    "expires_in": 3600
                }),
            ),
            OAuthHttpResponse {
                status_code: 500,
                body_text: "boom".to_string(),
                json_body: None,
            },
        ]);

        let result = adapter
            .exchange_code(
                &executor,
                &codex_ctx(Some("codex_cli_rs")),
                "code-1",
                "state-1",
                Some("verifier-1"),
            )
            .await
            .expect("api-key exchange failure must not fail the login");
        assert_eq!(result.token_set.access_token, "access-1");
        assert_eq!(executor.requests().len(), 2);
    }

    #[tokio::test]
    async fn codex_exchange_code_skips_api_key_exchange_without_id_token() {
        let adapter = CodexProviderOAuthAdapter::default();
        let executor = RecordingExecutor::new(vec![json_response(
            200,
            json!({ "access_token": "access-1", "refresh_token": "refresh-1" }),
        )]);

        adapter
            .exchange_code(
                &executor,
                &codex_ctx(None),
                "code-1",
                "state-1",
                Some("verifier-1"),
            )
            .await
            .expect("exchange should succeed");
        let requests = executor.requests();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].headers.contains_key("originator"));
    }

    #[tokio::test]
    async fn codex_refresh_sends_json_in_codex_rs_field_order() {
        let adapter = CodexProviderOAuthAdapter::default();
        let executor = RecordingExecutor::new(vec![json_response(
            200,
            json!({
                "access_token": "access-2",
                "refresh_token": "refresh-2",
                "expires_in": 3600
            }),
        )]);

        let result = adapter
            .import_credentials(
                &executor,
                &codex_ctx(Some("codex-tui")),
                crate::provider::ProviderOAuthImportInput {
                    provider_type: "codex".to_string(),
                    name: None,
                    refresh_token: Some("refresh-1".to_string()),
                    raw_credentials: None,
                    network: crate::network::OAuthNetworkContext::provider_operation(None),
                },
            )
            .await
            .expect("refresh should succeed");
        assert_eq!(result.token_set.access_token, "access-2");

        let requests = executor.requests();
        assert_eq!(
            requests.len(),
            1,
            "refresh never triggers the api-key exchange"
        );
        let refresh = &requests[0];
        assert!(refresh.body_bytes.is_none());
        assert_eq!(
            serde_json::to_string(refresh.json_body.as_ref().expect("json body")).unwrap(),
            r#"{"client_id":"app_EMoamEEZ73f0CkXaXp7hrann","grant_type":"refresh_token","refresh_token":"refresh-1"}"#
        );
        assert_eq!(
            refresh.headers,
            BTreeMap::from([
                ("content-type".to_string(), "application/json".to_string()),
                ("originator".to_string(), "codex-tui".to_string()),
            ])
        );
    }

    #[tokio::test]
    async fn codex_probe_reports_metadata_quota_and_email() {
        let adapter = CodexProviderOAuthAdapter::default();
        let ctx = ProviderOAuthTransportContext {
            provider_id: String::new(),
            provider_type: "codex".to_string(),
            endpoint_id: None,
            key_id: None,
            auth_type: Some("oauth".to_string()),
            decrypted_api_key: None,
            decrypted_auth_config: None,
            provider_config: None,
            endpoint_config: None,
            key_config: None,
            network: crate::network::OAuthNetworkContext::provider_operation(None),
            user_agent: None,
            originator: None,
        };
        let account = ProviderOAuthAccount {
            provider_type: "codex".to_string(),
            access_token: "access-token".to_string(),
            auth_config: json!({
                "email": "alice@example.com",
                "codex": {
                    "remaining_percent": 42,
                    "updated_at": 1000
                }
            }),
            expires_at_unix_secs: Some(2000),
            identity: BTreeMap::new(),
        };

        let probe = adapter
            .probe_account_state(&UnusedExecutor, &ctx, &account)
            .await
            .expect("probe should succeed")
            .expect("probe should return state");

        assert!(probe.state.is_valid);
        assert_eq!(probe.state.email.as_deref(), Some("alice@example.com"));
        assert_eq!(probe.state.quota.as_ref().unwrap()["remaining_percent"], 42);
    }
}
