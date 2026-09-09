use super::AdminProviderOAuthClientIdentity;
use crate::handlers::admin::request::AdminProviderOAuthTemplate;
use aether_oauth::provider::{ProviderOAuthService, ProviderOAuthTransportContext};
use serde_json::json;
use url::form_urlencoded;

pub(crate) fn build_provider_oauth_start_response(
    template: AdminProviderOAuthTemplate,
    nonce: &str,
    code_challenge: Option<&str>,
    client_identity: Option<&AdminProviderOAuthClientIdentity>,
) -> serde_json::Value {
    let authorization_url =
        build_provider_oauth_authorization_url(template, nonce, code_challenge, client_identity)
            .unwrap_or_else(|| {
                build_provider_oauth_authorization_url_legacy(template, nonce, code_challenge)
            });
    let redirect_uri = url::Url::parse(&authorization_url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "redirect_uri")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or_else(|| template.redirect_uri.to_string());

    json!({
        "authorization_url": authorization_url,
        "redirect_uri": redirect_uri,
        "provider_type": template.provider_type,
        "instructions": "1) 打开 authorization_url 完成授权\n2) 授权后会跳转到 redirect_uri（localhost）\n3) 复制浏览器地址栏完整 URL，调用 complete 接口粘贴 callback_url",
    })
}

fn build_provider_oauth_authorization_url(
    template: AdminProviderOAuthTemplate,
    nonce: &str,
    code_challenge: Option<&str>,
    client_identity: Option<&AdminProviderOAuthClientIdentity>,
) -> Option<String> {
    let ctx = ProviderOAuthTransportContext {
        provider_id: String::new(),
        provider_type: template.provider_type.to_string(),
        endpoint_id: None,
        key_id: None,
        auth_type: Some("oauth".to_string()),
        decrypted_api_key: None,
        decrypted_auth_config: None,
        provider_config: None,
        endpoint_config: None,
        key_config: None,
        network: aether_oauth::network::OAuthNetworkContext::provider_operation(None),
        user_agent: client_identity.map(|identity| identity.user_agent.clone()),
        originator: client_identity.map(|identity| identity.originator.clone()),
    };
    ProviderOAuthService::with_builtin_adapters()
        .build_authorize_url(&ctx, nonce, code_challenge)
        .ok()
        .map(|response| response.authorize_url)
}

fn build_provider_oauth_authorization_url_legacy(
    template: AdminProviderOAuthTemplate,
    nonce: &str,
    code_challenge: Option<&str>,
) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("client_id", template.client_id);
    serializer.append_pair("response_type", "code");
    serializer.append_pair("redirect_uri", template.redirect_uri);
    serializer.append_pair("scope", &template.scopes.join(" "));
    serializer.append_pair("state", nonce);
    if template.provider_type == "codex" {
        serializer.append_pair("id_token_add_organizations", "true");
        serializer.append_pair("codex_cli_simplified_flow", "true");
    }
    if template.use_pkce {
        if let Some(code_challenge) = code_challenge {
            serializer.append_pair("code_challenge", code_challenge);
            serializer.append_pair("code_challenge_method", "S256");
        }
    }

    format!("{}?{}", template.authorize_url, serializer.finish())
}

#[cfg(test)]
mod tests {
    use super::build_provider_oauth_start_response;
    use super::AdminProviderOAuthClientIdentity;
    use crate::handlers::admin::provider::oauth::state::admin_provider_oauth_template;

    #[test]
    fn codex_start_response_advertises_selected_originator() {
        let template = admin_provider_oauth_template("codex").expect("codex template");
        let identity = AdminProviderOAuthClientIdentity {
            user_agent: "codex-tui/0.153.3 (Debian 13.0.0; x86_64)".to_string(),
            originator: "codex-tui".to_string(),
        };
        let response =
            build_provider_oauth_start_response(template, "state-1", Some("chal"), Some(&identity));
        let url = response["authorization_url"].as_str().expect("url");
        assert!(
            url.ends_with("&state=state-1&originator=codex-tui"),
            "{url}"
        );
        assert!(!url.contains("prompt="), "{url}");
        assert!(url.contains("scope=openid%20profile%20email%20offline_access%20api.connectors.read%20api.connectors.invoke"), "{url}");
        assert_eq!(
            response["redirect_uri"].as_str(),
            Some("http://localhost:1455/auth/callback")
        );
    }
}
