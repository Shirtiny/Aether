use crate::core::{
    current_unix_secs, OAuthAuthorizeResponse, OAuthDeviceAuthorization, OAuthError, OAuthTokenSet,
};
use crate::network::{OAuthHttpExecutor, OAuthHttpRequest};
use crate::provider::ProviderOAuthAdapter;
use crate::provider::{
    ProviderOAuthAccount, ProviderOAuthAccountState, ProviderOAuthCapabilities,
    ProviderOAuthImportInput, ProviderOAuthProbeResult, ProviderOAuthRequestAuth,
    ProviderOAuthTokenSet, ProviderOAuthTransportContext,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use url::form_urlencoded;

pub const XAI_DEFAULT_AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";
pub const XAI_DEFAULT_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
/// OIDC discovery document. xAI publishes no stable device authorization URL,
/// so discovery is the only supported way to locate that endpoint.
pub const XAI_DEFAULT_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
/// RFC 8628 device authorization grant.
pub const XAI_DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Poll interval floor applied when the device endpoint omits `interval`.
pub const XAI_DEVICE_DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
/// Upper bound on how long a device authorization may stay pending.
pub const XAI_DEVICE_MAX_POLL_DURATION_SECS: u64 = 30 * 60;
/// Official xAI API root. Grok CLI OAuth grants reach it only when the account
/// opts into API mode; it is also the endpoint for media and websocket traffic
/// once those return.
pub const XAI_API_BASE_URL: &str = "https://api.x.ai/v1";
/// Grok CLI chat-proxy root. A subscription OAuth grant serves non-media chat
/// from here, so it is the default base URL for pooled Grok accounts.
pub const XAI_CLI_CHAT_PROXY_BASE_URL: &str = "https://cli-chat-proxy.grok.com/v1";
pub const XAI_DEFAULT_BASE_URL: &str = XAI_CLI_CHAT_PROXY_BASE_URL;
pub const XAI_DEFAULT_REDIRECT_URI: &str = "http://127.0.0.1:56121/callback";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenericProviderOAuthTemplate {
    pub provider_type: &'static str,
    pub display_name: &'static str,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub client_id: &'static str,
    pub client_secret: &'static str,
    pub scopes: &'static [&'static str],
    pub redirect_uri: &'static str,
    pub use_pkce: bool,
    pub uses_json_payload: bool,
}

pub const GENERIC_PROVIDER_OAUTH_TEMPLATES: &[GenericProviderOAuthTemplate] = &[
    GenericProviderOAuthTemplate {
        provider_type: "claude_code",
        display_name: "ClaudeCode",
        authorize_url: "https://claude.ai/oauth/authorize",
        token_url: "https://console.anthropic.com/v1/oauth/token",
        client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
        client_secret: "",
        scopes: &["org:create_api_key", "user:profile", "user:inference"],
        redirect_uri: "http://localhost:54545/callback",
        use_pkce: true,
        uses_json_payload: true,
    },
    GenericProviderOAuthTemplate {
        provider_type: "codex",
        display_name: "Codex",
        authorize_url: "https://auth.openai.com/oauth/authorize",
        token_url: "https://auth.openai.com/oauth/token",
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
        client_secret: "",
        scopes: &["openid", "email", "profile", "offline_access"],
        redirect_uri: "http://localhost:1455/auth/callback",
        use_pkce: true,
        uses_json_payload: false,
    },
    GenericProviderOAuthTemplate {
        provider_type: "chatgpt_web",
        display_name: "ChatGPT Web",
        authorize_url: "https://auth.openai.com/oauth/authorize",
        token_url: "https://auth.openai.com/oauth/token",
        client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
        client_secret: "",
        scopes: &["openid", "email", "profile", "offline_access"],
        redirect_uri: "http://localhost:1455/auth/callback",
        use_pkce: true,
        uses_json_payload: false,
    },
    GenericProviderOAuthTemplate {
        provider_type: "gemini_cli",
        display_name: "GeminiCli",
        authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
        token_url: "https://oauth2.googleapis.com/token",
        client_id: "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com",
        client_secret: "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",
        scopes: &[
            "https://www.googleapis.com/auth/cloud-platform",
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
        ],
        redirect_uri: "http://localhost:8085/oauth2callback",
        use_pkce: false,
        uses_json_payload: false,
    },
    GenericProviderOAuthTemplate {
        provider_type: "antigravity",
        display_name: "Antigravity",
        authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
        token_url: "https://oauth2.googleapis.com/token",
        client_id: "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com",
        client_secret: "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",
        scopes: &[
            "https://www.googleapis.com/auth/cloud-platform",
            "https://www.googleapis.com/auth/userinfo.email",
            "https://www.googleapis.com/auth/userinfo.profile",
            "https://www.googleapis.com/auth/cclog",
            "https://www.googleapis.com/auth/experimentsandconfigs",
        ],
        redirect_uri: "http://localhost:51121/oauth2callback",
        use_pkce: true,
        uses_json_payload: false,
    },
    GenericProviderOAuthTemplate {
        provider_type: "grok",
        display_name: "Grok (xAI)",
        authorize_url: XAI_DEFAULT_AUTHORIZE_URL,
        token_url: XAI_DEFAULT_TOKEN_URL,
        client_id: "b1a00492-073a-47ea-816f-4c329264a828",
        client_secret: "",
        scopes: &[
            "openid",
            "profile",
            "email",
            "offline_access",
            "grok-cli:access",
            "api:access",
        ],
        redirect_uri: XAI_DEFAULT_REDIRECT_URI,
        use_pkce: true,
        uses_json_payload: false,
    },
];

#[derive(Debug, Clone)]
pub struct GenericProviderOAuthAdapter {
    template: GenericProviderOAuthTemplate,
    authorize_url_override: Option<String>,
    token_url_override: Option<String>,
    discovery_url_override: Option<String>,
    client_id_override: Option<String>,
    scopes_override: Option<Vec<String>>,
    redirect_uri_override: Option<String>,
    base_url_override: Option<String>,
}

impl GenericProviderOAuthAdapter {
    pub fn new(template: GenericProviderOAuthTemplate) -> Self {
        Self {
            template,
            authorize_url_override: None,
            token_url_override: None,
            discovery_url_override: None,
            client_id_override: None,
            scopes_override: None,
            redirect_uri_override: None,
            base_url_override: None,
        }
    }

    pub fn for_provider_type(provider_type: &str) -> Option<Self> {
        template_for_provider_type(provider_type).map(|template| {
            let mut adapter = Self::new(template);
            if template.provider_type == "grok" {
                adapter.authorize_url_override = Some(effective_xai_oauth_authorize_url());
                adapter.token_url_override = Some(effective_xai_oauth_token_url());
                adapter.client_id_override = env_non_empty("XAI_OAUTH_CLIENT_ID");
                adapter.scopes_override = env_non_empty("XAI_OAUTH_SCOPE").map(|scope| {
                    scope
                        .split_whitespace()
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                });
                adapter.redirect_uri_override = Some(effective_xai_oauth_redirect_uri());
                adapter.base_url_override = Some(effective_xai_base_url());
            }
            adapter
        })
    }

    pub fn with_token_url_override(mut self, token_url: impl Into<String>) -> Self {
        self.token_url_override = Some(token_url.into());
        self
    }

    pub fn with_discovery_url_override(mut self, discovery_url: impl Into<String>) -> Self {
        self.discovery_url_override = Some(discovery_url.into());
        self
    }

    /// Reuse an xAI token endpoint previously admitted from discovery. This is
    /// intentionally stricter than `with_token_url_override`: refresh tokens
    /// must not be sent to an arbitrary URL read from stored account metadata.
    pub fn with_discovered_xai_token_url_override(
        mut self,
        token_url: impl Into<String>,
    ) -> Result<Self, OAuthError> {
        if self.template.provider_type != "grok" {
            return Err(OAuthError::UnsupportedProvider(
                self.template.provider_type.to_string(),
            ));
        }
        let discovery_url = self
            .discovery_url_override
            .clone()
            .unwrap_or_else(effective_xai_oauth_discovery_url);
        self.token_url_override = Some(validate_xai_discovery_endpoint(
            &token_url.into(),
            "token_endpoint",
            &discovery_url,
        )?);
        Ok(self)
    }

    pub fn with_token_url_for_tests(self, token_url: impl Into<String>) -> Self {
        self.with_token_url_override(token_url)
    }

    fn token_url(&self) -> String {
        self.token_url_override
            .clone()
            .unwrap_or_else(|| self.template.token_url.to_string())
    }

    fn authorize_url(&self) -> &str {
        self.authorize_url_override
            .as_deref()
            .unwrap_or(self.template.authorize_url)
    }

    fn client_id(&self) -> &str {
        self.client_id_override
            .as_deref()
            .unwrap_or(self.template.client_id)
    }

    fn scopes(&self) -> Vec<String> {
        self.scopes_override.clone().unwrap_or_else(|| {
            self.template
                .scopes
                .iter()
                .map(|scope| (*scope).to_string())
                .collect()
        })
    }

    fn redirect_uri(&self) -> &str {
        self.redirect_uri_override
            .as_deref()
            .unwrap_or(self.template.redirect_uri)
    }

    fn base_url(&self) -> &str {
        self.base_url_override
            .as_deref()
            .unwrap_or(XAI_DEFAULT_BASE_URL)
    }

    /// Resolve xAI's OAuth endpoints from its OIDC discovery document. There is
    /// no fallback: the device authorization endpoint is published nowhere else,
    /// and guessing it would send device codes to an unverified URL.
    pub async fn discover_xai_oauth_endpoints(
        &self,
        executor: &dyn OAuthHttpExecutor,
        ctx: &ProviderOAuthTransportContext,
    ) -> Result<XaiOAuthDiscovery, OAuthError> {
        if self.template.provider_type != "grok" {
            return Err(OAuthError::UnsupportedProvider(
                self.template.provider_type.to_string(),
            ));
        }
        let discovery_url = self
            .discovery_url_override
            .clone()
            .unwrap_or_else(effective_xai_oauth_discovery_url);
        let response = executor
            .execute(OAuthHttpRequest {
                request_id: "provider-oauth:grok:discovery".to_string(),
                method: reqwest::Method::GET,
                url: discovery_url.clone(),
                headers: json_headers(),
                content_type: None,
                json_body: None,
                body_bytes: None,
                network: ctx.network.clone(),
            })
            .await?;
        if !(200..300).contains(&response.status_code) {
            return Err(OAuthError::HttpStatus {
                status_code: response.status_code,
                body_excerpt: truncate_body(&response.body_text),
            });
        }
        let payload = response
            .json_body
            .or_else(|| serde_json::from_str::<Value>(&response.body_text).ok())
            .ok_or_else(|| OAuthError::invalid_response("xai discovery response is not json"))?;
        let device_authorization_endpoint = validate_xai_discovery_endpoint(
            payload
                .get("device_authorization_endpoint")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "device_authorization_endpoint",
            &discovery_url,
        )?;
        let token_endpoint = validate_xai_discovery_endpoint(
            payload
                .get("token_endpoint")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "token_endpoint",
            &discovery_url,
        )?;
        Ok(XaiOAuthDiscovery {
            device_authorization_endpoint,
            token_endpoint,
        })
    }

    /// Ask xAI for a device code the operator can approve from any browser.
    pub async fn start_xai_device_authorization(
        &self,
        executor: &dyn OAuthHttpExecutor,
        ctx: &ProviderOAuthTransportContext,
    ) -> Result<XaiDeviceAuthorization, OAuthError> {
        let discovery = self.discover_xai_oauth_endpoints(executor, ctx).await?;
        let form_body = {
            let mut form = form_urlencoded::Serializer::new(String::new());
            form.append_pair("client_id", self.client_id());
            let scopes = self.scopes();
            if !scopes.is_empty() {
                form.append_pair("scope", &scopes.join(" "));
            }
            form.finish().into_bytes()
        };
        let response = executor
            .execute(OAuthHttpRequest {
                request_id: "provider-oauth:grok:device-authorize".to_string(),
                method: reqwest::Method::POST,
                url: discovery.device_authorization_endpoint.clone(),
                headers: form_headers(),
                content_type: Some("application/x-www-form-urlencoded".to_string()),
                json_body: None,
                body_bytes: Some(form_body),
                network: ctx.network.clone(),
            })
            .await?;
        if !(200..300).contains(&response.status_code) {
            return Err(OAuthError::HttpStatus {
                status_code: response.status_code,
                body_excerpt: truncate_body(&response.body_text),
            });
        }
        let payload = response
            .json_body
            .or_else(|| serde_json::from_str::<Value>(&response.body_text).ok())
            .ok_or_else(|| {
                OAuthError::invalid_response("xai device authorization response is not json")
            })?;
        let string_field = |key: &str| {
            payload
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        };
        let device_code = string_field("device_code").ok_or_else(|| {
            OAuthError::invalid_response("xai device authorization missing device_code")
        })?;
        let user_code = string_field("user_code").ok_or_else(|| {
            OAuthError::invalid_response("xai device authorization missing user_code")
        })?;
        let verification_uri = string_field("verification_uri")
            .or_else(|| string_field("verification_url"))
            .ok_or_else(|| {
                OAuthError::invalid_response("xai device authorization missing verification_uri")
            })?;
        let verification_uri_complete = string_field("verification_uri_complete")
            .or_else(|| string_field("verification_url_complete"))
            .unwrap_or_else(|| verification_uri.clone());
        let expires_in = payload
            .get("expires_in")
            .and_then(Value::as_u64)
            .unwrap_or(XAI_DEVICE_MAX_POLL_DURATION_SECS)
            .min(XAI_DEVICE_MAX_POLL_DURATION_SECS);
        let interval = payload
            .get("interval")
            .and_then(Value::as_u64)
            .filter(|interval| *interval > 0)
            .unwrap_or(XAI_DEVICE_DEFAULT_POLL_INTERVAL_SECS)
            .max(XAI_DEVICE_DEFAULT_POLL_INTERVAL_SECS);
        Ok(XaiDeviceAuthorization {
            authorization: OAuthDeviceAuthorization {
                device_code,
                user_code,
                verification_uri,
                verification_uri_complete,
                expires_in,
                interval,
            },
            token_endpoint: discovery.token_endpoint,
        })
    }

    /// Poll once for the outcome of a device authorization. A pending or
    /// slow-down answer is a normal state, not a failure.
    pub async fn poll_xai_device_token(
        &self,
        executor: &dyn OAuthHttpExecutor,
        ctx: &ProviderOAuthTransportContext,
        device_code: &str,
        token_endpoint: &str,
    ) -> Result<XaiDevicePollOutcome, OAuthError> {
        // Re-check the pinned endpoint against the same rule that admitted it,
        // so a tampered session cannot move the exchange.
        let discovery_url = self
            .discovery_url_override
            .clone()
            .unwrap_or_else(effective_xai_oauth_discovery_url);
        let token_endpoint =
            validate_xai_discovery_endpoint(token_endpoint, "token_endpoint", &discovery_url)?;
        let form_body = {
            let mut form = form_urlencoded::Serializer::new(String::new());
            form.append_pair("grant_type", XAI_DEVICE_CODE_GRANT_TYPE);
            form.append_pair("client_id", self.client_id());
            form.append_pair("device_code", device_code);
            form.finish().into_bytes()
        };
        let response = executor
            .execute(OAuthHttpRequest {
                request_id: "provider-oauth:grok:device-poll".to_string(),
                method: reqwest::Method::POST,
                url: token_endpoint.clone(),
                headers: form_headers(),
                content_type: Some("application/x-www-form-urlencoded".to_string()),
                json_body: None,
                body_bytes: Some(form_body),
                network: ctx.network.clone(),
            })
            .await?;
        let payload = response
            .json_body
            .clone()
            .or_else(|| serde_json::from_str::<Value>(&response.body_text).ok());
        if (200..300).contains(&response.status_code) {
            let payload = payload.ok_or_else(|| {
                OAuthError::invalid_response("xai device token response is not json")
            })?;
            let mut result = self.token_set_from_payload(payload)?;
            if let Some(auth_config) = result.auth_config.as_object_mut() {
                // The endpoint used for the device grant, rather than the
                // authorization-code fallback endpoint, owns this refresh
                // token and must be retained for subsequent refreshes.
                auth_config.insert("token_endpoint".to_string(), json!(token_endpoint));
            }
            return Ok(XaiDevicePollOutcome::Authorized(Box::new(result)));
        }
        // RFC 8628 reports the in-progress states as OAuth errors, so the error
        // code decides whether to keep waiting rather than to give up.
        let error_code = payload
            .as_ref()
            .and_then(|payload| payload.get("error"))
            .and_then(Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase())
            .unwrap_or_default();
        match error_code.as_str() {
            "authorization_pending" => Ok(XaiDevicePollOutcome::Pending { slow_down: false }),
            "slow_down" => Ok(XaiDevicePollOutcome::Pending { slow_down: true }),
            "expired_token" => Ok(XaiDevicePollOutcome::Expired),
            "access_denied" => Ok(XaiDevicePollOutcome::Denied(
                payload
                    .as_ref()
                    .and_then(|payload| payload.get("error_description"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("授权被拒绝")
                    .to_string(),
            )),
            _ => Err(OAuthError::HttpStatus {
                status_code: response.status_code,
                body_excerpt: truncate_body(&response.body_text),
            }),
        }
    }

    async fn exchange_grant(
        &self,
        executor: &dyn OAuthHttpExecutor,
        ctx: &ProviderOAuthTransportContext,
        grant_type: &str,
        code_or_refresh_token: &str,
        state: Option<&str>,
        pkce_verifier: Option<&str>,
    ) -> Result<ProviderOAuthTokenSet, OAuthError> {
        let scopes = self.scopes();
        let scope = (!scopes.is_empty()).then(|| scopes.join(" "));
        let request_id = match grant_type {
            "authorization_code" => "provider-oauth:exchange-code".to_string(),
            "refresh_token" => "provider-oauth:refresh-token".to_string(),
            _ => format!(
                "provider-oauth:{}:{grant_type}",
                self.template.provider_type
            ),
        };
        let response = if self.template.uses_json_payload {
            let mut body = serde_json::Map::from_iter([
                (
                    "grant_type".to_string(),
                    Value::String(grant_type.to_string()),
                ),
                (
                    "client_id".to_string(),
                    Value::String(self.client_id().to_string()),
                ),
            ]);
            if grant_type == "authorization_code" {
                body.insert(
                    "code".to_string(),
                    Value::String(code_or_refresh_token.to_string()),
                );
                body.insert(
                    "redirect_uri".to_string(),
                    Value::String(self.redirect_uri().to_string()),
                );
                if let Some(state) = state {
                    body.insert("state".to_string(), Value::String(state.to_string()));
                }
                if let Some(verifier) = pkce_verifier {
                    body.insert(
                        "code_verifier".to_string(),
                        Value::String(verifier.to_string()),
                    );
                }
            } else {
                body.insert(
                    "refresh_token".to_string(),
                    Value::String(code_or_refresh_token.to_string()),
                );
            }
            if let Some(scope) = scope.as_ref() {
                body.insert("scope".to_string(), Value::String(scope.clone()));
            }
            executor
                .execute(OAuthHttpRequest {
                    request_id: request_id.clone(),
                    method: reqwest::Method::POST,
                    url: self.token_url(),
                    headers: json_headers(),
                    content_type: Some("application/json".to_string()),
                    json_body: Some(Value::Object(body)),
                    body_bytes: None,
                    network: ctx.network.clone(),
                })
                .await?
        } else {
            let form_body = {
                let mut form = form_urlencoded::Serializer::new(String::new());
                form.append_pair("grant_type", grant_type);
                form.append_pair("client_id", self.client_id());
                if grant_type == "authorization_code" {
                    form.append_pair("redirect_uri", self.redirect_uri());
                    form.append_pair("code", code_or_refresh_token);
                    if let Some(verifier) = pkce_verifier {
                        form.append_pair("code_verifier", verifier);
                    }
                } else {
                    form.append_pair("refresh_token", code_or_refresh_token);
                }
                if let Some(scope) = scope.as_ref() {
                    form.append_pair("scope", scope);
                }
                if !self.template.client_secret.trim().is_empty() {
                    form.append_pair("client_secret", self.template.client_secret);
                }
                form.finish().into_bytes()
            };
            executor
                .execute(OAuthHttpRequest {
                    request_id,
                    method: reqwest::Method::POST,
                    url: self.token_url(),
                    headers: form_headers(),
                    content_type: Some("application/x-www-form-urlencoded".to_string()),
                    json_body: None,
                    body_bytes: Some(form_body),
                    network: ctx.network.clone(),
                })
                .await?
        };
        if !(200..300).contains(&response.status_code) {
            return Err(OAuthError::HttpStatus {
                status_code: response.status_code,
                body_excerpt: truncate_body(&response.body_text),
            });
        }
        let payload = response
            .json_body
            .or_else(|| serde_json::from_str::<Value>(&response.body_text).ok())
            .ok_or_else(|| OAuthError::invalid_response("token response is not json"))?;
        if self.template.provider_type == "grok" && grant_type == "authorization_code" {
            validate_xai_id_token_nonce(&payload, state)?;
        }
        self.token_set_from_payload(payload)
    }

    fn token_set_from_payload(&self, payload: Value) -> Result<ProviderOAuthTokenSet, OAuthError> {
        let token_set = OAuthTokenSet::from_token_payload(payload.clone())
            .ok_or_else(|| OAuthError::invalid_response("token response missing access_token"))?;
        let mut auth_config = serde_json::Map::new();
        auth_config.insert(
            "provider_type".to_string(),
            json!(self.template.provider_type),
        );
        auth_config.insert("updated_at".to_string(), json!(current_unix_secs()));
        if let Some(token_type) = token_set.token_type.as_ref() {
            auth_config.insert("token_type".to_string(), json!(token_type));
        }
        if let Some(refresh_token) = token_set.refresh_token.as_ref() {
            auth_config.insert("refresh_token".to_string(), json!(refresh_token));
        }
        if let Some(expires_at) = token_set.expires_at_unix_secs {
            auth_config.insert("expires_at".to_string(), json!(expires_at));
        }
        if let Some(scope) = token_set.scope.as_ref() {
            auth_config.insert("scope".to_string(), json!(scope));
        }
        if self.template.provider_type == "grok" {
            auth_config.insert("access_token".to_string(), json!(token_set.access_token));
            if let Some(id_token) = payload
                .get("id_token")
                .or_else(|| payload.get("idToken"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                auth_config.insert("id_token".to_string(), json!(id_token));
            }
            auth_config.insert("token_endpoint".to_string(), json!(self.token_url()));
            auth_config.insert("client_id".to_string(), json!(self.client_id()));
            auth_config.insert("base_url".to_string(), json!(self.base_url()));
            auth_config
                .entry("scope".to_string())
                .or_insert_with(|| json!(self.scopes().join(" ")));
        }
        enrich_generic_identity(self.template.provider_type, &mut auth_config, &payload);
        Ok(ProviderOAuthTokenSet {
            token_set,
            auth_config: Value::Object(auth_config),
        })
    }
}

#[async_trait]
impl ProviderOAuthAdapter for GenericProviderOAuthAdapter {
    fn provider_type(&self) -> &'static str {
        self.template.provider_type
    }

    fn capabilities(&self) -> ProviderOAuthCapabilities {
        if self.template.provider_type == "grok" {
            // The device grant is how Grok is meant to be enrolled: it needs no
            // redirect target, which a server-side deployment cannot receive.
            // The authorization-code path stays reachable as a fallback, so it
            // is still advertised.
            return ProviderOAuthCapabilities {
                supports_device_flow: true,
                ..ProviderOAuthCapabilities::GENERIC_AUTH_CODE
            };
        }
        ProviderOAuthCapabilities::GENERIC_AUTH_CODE
    }

    fn build_authorize_url(
        &self,
        _ctx: &ProviderOAuthTransportContext,
        state: &str,
        code_challenge: Option<&str>,
    ) -> Result<OAuthAuthorizeResponse, OAuthError> {
        let mut url = url::Url::parse(self.authorize_url())
            .map_err(|_| OAuthError::invalid_request("authorize_url must be absolute"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", self.client_id());
            query.append_pair("redirect_uri", self.redirect_uri());
            query.append_pair("state", state);
            let scopes = self.scopes();
            if !scopes.is_empty() {
                query.append_pair("scope", &scopes.join(" "));
            }
            if let Some(challenge) = code_challenge {
                query.append_pair("code_challenge", challenge);
                query.append_pair("code_challenge_method", "S256");
            }
            if self.template.provider_type == "grok" {
                query.append_pair("nonce", state);
                query.append_pair("plan", "generic");
                query.append_pair("referrer", "aether");
            }
        }
        Ok(OAuthAuthorizeResponse {
            authorize_url: url.to_string(),
            state: state.to_string(),
            code_challenge: code_challenge.map(ToOwned::to_owned),
        })
    }

    async fn exchange_code(
        &self,
        executor: &dyn OAuthHttpExecutor,
        ctx: &ProviderOAuthTransportContext,
        code: &str,
        state: &str,
        pkce_verifier: Option<&str>,
    ) -> Result<ProviderOAuthTokenSet, OAuthError> {
        self.exchange_grant(
            executor,
            ctx,
            "authorization_code",
            code,
            Some(state),
            pkce_verifier,
        )
        .await
    }

    async fn import_credentials(
        &self,
        executor: &dyn OAuthHttpExecutor,
        ctx: &ProviderOAuthTransportContext,
        input: ProviderOAuthImportInput,
    ) -> Result<ProviderOAuthTokenSet, OAuthError> {
        let refresh_token = input
            .refresh_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| OAuthError::invalid_request("refresh_token is required"))?;
        self.exchange_grant(executor, ctx, "refresh_token", refresh_token, None, None)
            .await
    }

    async fn refresh(
        &self,
        executor: &dyn OAuthHttpExecutor,
        ctx: &ProviderOAuthTransportContext,
        account: &ProviderOAuthAccount,
    ) -> Result<ProviderOAuthTokenSet, OAuthError> {
        let refresh_token = account
            .auth_config
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| OAuthError::invalid_request("auth_config missing refresh_token"))?;
        let mut refreshed = self
            .exchange_grant(executor, ctx, "refresh_token", refresh_token, None, None)
            .await?;

        // Refresh responses often omit stable account metadata, and some providers
        // do not rotate refresh_token on every refresh. Preserve the stored config
        // as the base while letting the fresh token payload win.
        if let Some(existing) = account.auth_config.as_object() {
            let mut merged = existing.clone();
            if let Some(updated) = refreshed.auth_config.as_object() {
                for (key, value) in updated {
                    merged.insert(key.clone(), value.clone());
                }
            }
            if refreshed.token_set.refresh_token.is_none() {
                refreshed.token_set.refresh_token = Some(refresh_token.to_string());
                merged.insert("refresh_token".to_string(), json!(refresh_token));
            }
            refreshed.auth_config = Value::Object(merged);
        }
        Ok(refreshed)
    }

    fn resolve_request_auth(
        &self,
        account: &ProviderOAuthAccount,
    ) -> Result<ProviderOAuthRequestAuth, OAuthError> {
        Ok(account.request_bearer_auth())
    }

    fn account_fingerprint(&self, account: &ProviderOAuthAccount) -> Option<String> {
        let refresh_token = account
            .auth_config
            .get("refresh_token")
            .and_then(Value::as_str)
            .or(Some(account.access_token.as_str()))?;
        Some(secret_fingerprint(refresh_token))
    }
}

pub fn template_for_provider_type(provider_type: &str) -> Option<GenericProviderOAuthTemplate> {
    let normalized = provider_type.trim();
    GENERIC_PROVIDER_OAUTH_TEMPLATES
        .iter()
        .find(|template| normalized.eq_ignore_ascii_case(template.provider_type))
        .copied()
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn xai_allow_unsafe_url_overrides() -> bool {
    env_non_empty("XAI_ALLOW_UNSAFE_URL_OVERRIDES").is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

pub fn effective_xai_oauth_authorize_url() -> String {
    effective_xai_url_override(
        "XAI_OAUTH_AUTHORIZE_URL",
        XAI_DEFAULT_AUTHORIZE_URL,
        XaiUrlKind::OAuth,
    )
}

pub fn effective_xai_oauth_token_url() -> String {
    effective_xai_url_override(
        "XAI_OAUTH_TOKEN_URL",
        XAI_DEFAULT_TOKEN_URL,
        XaiUrlKind::OAuth,
    )
}

pub fn effective_xai_oauth_redirect_uri() -> String {
    effective_xai_url_override(
        "XAI_OAUTH_REDIRECT_URI",
        XAI_DEFAULT_REDIRECT_URI,
        XaiUrlKind::Redirect,
    )
}

pub fn effective_xai_base_url() -> String {
    effective_xai_url_override("XAI_BASE_URL", XAI_DEFAULT_BASE_URL, XaiUrlKind::ApiBase)
}

pub fn effective_xai_oauth_discovery_url() -> String {
    effective_xai_url_override(
        "XAI_OAUTH_DISCOVERY_URL",
        XAI_DEFAULT_DISCOVERY_URL,
        XaiUrlKind::OAuth,
    )
}

/// Endpoints resolved from xAI's OIDC discovery document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XaiOAuthDiscovery {
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
}

/// A device authorization pending user approval, paired with the token endpoint
/// that resolved alongside it so polling cannot drift to a different host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XaiDeviceAuthorization {
    pub authorization: OAuthDeviceAuthorization,
    pub token_endpoint: String,
}

/// The state of a device authorization as of one poll.
#[derive(Debug, Clone, PartialEq)]
pub enum XaiDevicePollOutcome {
    /// The user has not finished approving yet. `slow_down` asks the caller to
    /// widen its poll interval.
    Pending {
        slow_down: bool,
    },
    /// The user refused, or the authorization server rejected the client.
    Denied(String),
    /// The device code aged out before approval.
    Expired,
    Authorized(Box<ProviderOAuthTokenSet>),
}

/// Validate an endpoint handed to us by the discovery document. The document is
/// fetched over the network, so a compromised or spoofed response must not be
/// able to redirect a device code or a bearer token off x.ai.
///
/// A discovery document served from x.ai may name any x.ai host, matching how
/// xAI publishes its endpoints. A deliberately overridden discovery URL (self
/// hosting, tests) is instead trusted only to describe its own origin, so an
/// override can never widen where credentials may be sent.
fn validate_xai_discovery_endpoint(
    raw: &str,
    field: &str,
    discovery_url: &str,
) -> Result<String, OAuthError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(OAuthError::invalid_response(format!(
            "xai discovery {field} is empty"
        )));
    }
    let url = url::Url::parse(raw)
        .map_err(|_| OAuthError::invalid_response(format!("xai discovery {field} is invalid")))?;
    let host_of = |url: &url::Url| {
        url.host_str()
            .map(|host| host.trim().to_ascii_lowercase())
            .unwrap_or_default()
    };
    let is_on_xai = |host: &str| host == "x.ai" || host.ends_with(".x.ai");

    let discovery = url::Url::parse(discovery_url.trim()).ok();
    let discovery_is_official = discovery
        .as_ref()
        .is_some_and(|discovery| discovery.scheme() == "https" && is_on_xai(&host_of(discovery)));

    if discovery_is_official {
        if url.scheme() != "https" {
            return Err(OAuthError::invalid_response(format!(
                "xai discovery {field} must use https"
            )));
        }
        let host = host_of(&url);
        if !is_on_xai(&host) {
            return Err(OAuthError::invalid_response(format!(
                "xai discovery {field} host {host:?} is not on x.ai"
            )));
        }
        return Ok(url.to_string());
    }

    let Some(discovery) = discovery else {
        return Err(OAuthError::invalid_response(format!(
            "xai discovery {field} cannot be checked against an invalid discovery url"
        )));
    };
    if url.scheme() != discovery.scheme()
        || host_of(&url) != host_of(&discovery)
        || url.port_or_known_default() != discovery.port_or_known_default()
    {
        return Err(OAuthError::invalid_response(format!(
            "xai discovery {field} must stay on the discovery document's own origin"
        )));
    }
    Ok(url.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XaiUrlKind {
    OAuth,
    ApiBase,
    Redirect,
}

fn effective_xai_url_override(name: &str, default: &str, kind: XaiUrlKind) -> String {
    env_non_empty(name)
        .and_then(|value| validate_xai_url_override(&value, kind, xai_allow_unsafe_url_overrides()))
        .unwrap_or_else(|| default.to_string())
}

fn validate_xai_url_override(raw: &str, kind: XaiUrlKind, allow_unsafe: bool) -> Option<String> {
    let mut url = url::Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    if !allow_unsafe {
        let host = url.host_str()?.trim().to_ascii_lowercase();
        let allowed = match kind {
            XaiUrlKind::OAuth => {
                url.scheme() == "https" && (host == "x.ai" || host.ends_with(".x.ai"))
            }
            XaiUrlKind::ApiBase => {
                url.scheme() == "https"
                    && matches!(host.as_str(), "api.x.ai" | "cli-chat-proxy.grok.com")
            }
            XaiUrlKind::Redirect => {
                url.scheme() == "http"
                    && matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1")
                    && url.port() == Some(56_121)
                    && url.path() == "/callback"
                    && url.query().is_none()
            }
        };
        if !allowed {
            return None;
        }
    }
    if kind == XaiUrlKind::ApiBase && !allow_unsafe {
        let path = url.path().trim_end_matches('/');
        if path.is_empty() {
            url.set_path("/v1");
        } else if path != "/v1" {
            return None;
        }
    }
    url.set_fragment(None);
    Some(url.to_string().trim_end_matches('/').to_string())
}

fn validate_xai_id_token_nonce(
    payload: &Value,
    expected_nonce: Option<&str>,
) -> Result<(), OAuthError> {
    let Some(expected_nonce) = expected_nonce
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(OAuthError::invalid_request("xAI OAuth nonce is required"));
    };
    let Some(id_token) = payload
        .get("id_token")
        .or_else(|| payload.get("idToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let claims = decode_jwt_claims(id_token)
        .ok_or_else(|| OAuthError::invalid_response("xAI id_token is not a valid JWT"))?;
    let actual_nonce = claims
        .get("nonce")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OAuthError::invalid_response("xAI id_token missing nonce"))?;
    if actual_nonce != expected_nonce {
        return Err(OAuthError::invalid_response("xAI id_token nonce mismatch"));
    }
    Ok(())
}

fn form_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "content-type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ),
        ("accept".to_string(), "application/json".to_string()),
    ])
}

fn json_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("content-type".to_string(), "application/json".to_string()),
        ("accept".to_string(), "application/json".to_string()),
    ])
}

fn truncate_body(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        "-".to_string()
    } else {
        body.chars().take(500).collect()
    }
}

fn secret_fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut fingerprint = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        use std::fmt::Write as _;
        let _ = write!(&mut fingerprint, "{byte:02x}");
    }
    fingerprint
}

fn enrich_generic_identity(
    provider_type: &str,
    auth_config: &mut serde_json::Map<String, Value>,
    token_payload: &Value,
) {
    if let Some(object) = token_payload.as_object() {
        for field in [
            "email",
            "account_id",
            "account_user_id",
            "plan_type",
            "user_id",
            "account_name",
        ] {
            if !auth_config.contains_key(field) {
                if let Some(value) = object.get(field).cloned() {
                    auth_config.insert(field.to_string(), value);
                }
            }
        }
    }
    let provider_type = provider_type.trim().to_ascii_lowercase();
    if !matches!(provider_type.as_str(), "codex" | "chatgpt_web" | "grok") {
        return;
    }
    if let Some(access_token) = token_payload
        .get("access_token")
        .and_then(Value::as_str)
        .or_else(|| token_payload.get("id_token").and_then(Value::as_str))
    {
        if let Some(claims) = decode_jwt_claims(access_token) {
            for field in ["email", "sub"] {
                if let Some(value) = claims.get(field).cloned() {
                    let target = if field == "sub" { "user_id" } else { field };
                    auth_config.entry(target.to_string()).or_insert(value);
                }
            }
            if provider_type == "grok" {
                if let Some(value) = claims.get("sub").cloned() {
                    auth_config.entry("user_id".to_string()).or_insert(value);
                }
                return;
            }
            if let Some(auth) = claims
                .get("https://api.openai.com/auth")
                .and_then(Value::as_object)
            {
                for (source, target) in [
                    ("chatgpt_account_id", "account_id"),
                    ("chatgpt_account_user_id", "account_user_id"),
                    ("chatgpt_plan_type", "plan_type"),
                    ("chatgpt_user_id", "user_id"),
                ] {
                    if let Some(value) = auth.get(source).cloned() {
                        auth_config.entry(target.to_string()).or_insert(value);
                    }
                }
                if let Some(value) = auth.get("organizations").cloned() {
                    auth_config
                        .entry("organizations".to_string())
                        .or_insert(value);
                }
            }
            if let Some(profile) = claims
                .get("https://api.openai.com/profile")
                .and_then(Value::as_object)
            {
                if let Some(value) = profile.get("email").cloned() {
                    auth_config.entry("email".to_string()).or_insert(value);
                }
            }
        }
    }
}

pub(super) fn provider_account_state_from_metadata(
    metadata_key: &str,
    account: &ProviderOAuthAccount,
) -> ProviderOAuthProbeResult {
    let metadata = account
        .identity
        .get(metadata_key)
        .cloned()
        .or_else(|| account.auth_config.get(metadata_key).cloned());
    let email = string_field(&account.auth_config, "email")
        .or_else(|| account.identity.get("email").and_then(value_to_string))
        .or_else(|| {
            metadata
                .as_ref()
                .and_then(|value| string_field(value, "email"))
        });
    let invalid_reason = string_field(&account.auth_config, "oauth_invalid_reason")
        .or_else(|| string_field(&account.auth_config, "invalid_reason"))
        .or_else(|| metadata.as_ref().and_then(metadata_invalid_reason));
    let raw = json!({
        "auth_config": account.auth_config,
        "identity": account.identity,
    });
    ProviderOAuthProbeResult {
        state: ProviderOAuthAccountState {
            is_valid: !account.access_token.trim().is_empty() && invalid_reason.is_none(),
            email,
            quota: metadata,
            invalid_reason,
            raw: Some(raw),
        },
    }
}

fn metadata_invalid_reason(value: &Value) -> Option<String> {
    if value
        .get("is_forbidden")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return string_field(value, "forbidden_reason")
            .or_else(|| string_field(value, "message"))
            .or_else(|| Some("account_forbidden".to_string()));
    }
    if value
        .get("account_disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return string_field(value, "message")
            .or_else(|| string_field(value, "reason"))
            .or_else(|| Some("account_disabled".to_string()));
    }
    string_field(value, "invalid_reason").or_else(|| string_field(value, "reason"))
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(value_to_string)
}

fn value_to_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn decode_jwt_claims(token: &str) -> Option<serde_json::Map<String, Value>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.as_bytes()).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .as_object()
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::{
        template_for_provider_type, validate_xai_id_token_nonce, validate_xai_url_override,
        GenericProviderOAuthAdapter, XaiDevicePollOutcome, XaiUrlKind,
    };
    use crate::network::{OAuthHttpExecutor, OAuthHttpRequest, OAuthHttpResponse};
    use crate::provider::ProviderOAuthAdapter;
    use crate::provider::{ProviderOAuthAccount, ProviderOAuthTransportContext};
    use async_trait::async_trait;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[test]
    fn resolves_generic_provider_templates() {
        assert!(template_for_provider_type("codex").is_some());
        assert!(template_for_provider_type("claude_code").is_some());
        assert!(template_for_provider_type("grok").is_some());
        assert!(template_for_provider_type("kiro").is_none());
    }

    #[test]
    fn grok_authorize_url_uses_xai_pkce_cli_contract() {
        let adapter = GenericProviderOAuthAdapter::new(
            template_for_provider_type("grok").expect("grok template should exist"),
        );
        let ctx = ProviderOAuthTransportContext {
            provider_id: "provider-1".to_string(),
            provider_type: "grok".to_string(),
            endpoint_id: None,
            key_id: None,
            auth_type: Some("oauth".to_string()),
            decrypted_api_key: None,
            decrypted_auth_config: None,
            provider_config: None,
            endpoint_config: None,
            key_config: None,
            network: crate::network::OAuthNetworkContext::provider_operation(None),
        };
        let response = adapter
            .build_authorize_url(&ctx, "state-1", Some("challenge-1"))
            .expect("authorize url should build");
        let url = url::Url::parse(&response.authorize_url).expect("authorize url should parse");
        let query = url.query_pairs().collect::<BTreeMap<_, _>>();

        assert_eq!(url.host_str(), Some("auth.x.ai"));
        assert_eq!(url.path(), "/oauth2/authorize");
        assert_eq!(
            query.get("nonce").map(|value| value.as_ref()),
            Some("state-1")
        );
        assert_eq!(
            query.get("plan").map(|value| value.as_ref()),
            Some("generic")
        );
        assert_eq!(
            query.get("referrer").map(|value| value.as_ref()),
            Some("aether")
        );
        assert_eq!(
            query
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
        assert!(
            query.get("scope").is_some_and(
                |scope| scope.contains("grok-cli:access") && scope.contains("api:access")
            )
        );
    }

    #[test]
    fn xai_url_overrides_reject_credential_exfiltration_hosts_by_default() {
        assert!(validate_xai_url_override(
            "https://auth.x.ai/oauth2/token",
            XaiUrlKind::OAuth,
            false,
        )
        .is_some());
        assert!(validate_xai_url_override(
            "https://cli-chat-proxy.grok.com/v1/",
            XaiUrlKind::ApiBase,
            false,
        )
        .is_some());
        assert!(validate_xai_url_override(
            "https://attacker.example/token",
            XaiUrlKind::OAuth,
            false,
        )
        .is_none());
        assert!(
            validate_xai_url_override("http://api.x.ai/v1", XaiUrlKind::ApiBase, false,).is_none()
        );
        assert!(
            validate_xai_url_override("https://api.x.ai/not-v1", XaiUrlKind::ApiBase, false,)
                .is_none()
        );
        assert!(
            validate_xai_url_override("http://127.0.0.1:18080/token", XaiUrlKind::OAuth, true,)
                .is_some()
        );
        assert!(validate_xai_url_override(
            "http://127.0.0.1:56121/callback",
            XaiUrlKind::Redirect,
            false,
        )
        .is_some());
        assert!(validate_xai_url_override(
            "https://attacker.example/callback",
            XaiUrlKind::Redirect,
            false,
        )
        .is_none());
    }

    #[test]
    fn xai_id_token_nonce_must_match_the_authorization_request() {
        let claims = URL_SAFE_NO_PAD.encode(json!({ "nonce": "state-1" }).to_string());
        let payload = json!({
            "id_token": format!("e30.{claims}.")
        });

        assert!(validate_xai_id_token_nonce(&payload, Some("state-1")).is_ok());
        assert!(validate_xai_id_token_nonce(&payload, Some("different-state")).is_err());
        assert!(validate_xai_id_token_nonce(&payload, None).is_err());
    }

    #[test]
    fn generic_adapter_exposes_provider_type() {
        let adapter = GenericProviderOAuthAdapter::for_provider_type("codex")
            .expect("codex template should exist");
        assert_eq!(adapter.provider_type(), "codex");
        assert!(adapter.capabilities().supports_refresh_token_import);
    }

    #[derive(Debug, Clone)]
    struct StaticExecutor {
        seen_request: Arc<Mutex<Option<OAuthHttpRequest>>>,
    }

    fn grok_context() -> ProviderOAuthTransportContext {
        ProviderOAuthTransportContext {
            provider_id: "provider-1".to_string(),
            provider_type: "grok".to_string(),
            endpoint_id: None,
            key_id: Some("key-1".to_string()),
            auth_type: Some("oauth".to_string()),
            decrypted_api_key: None,
            decrypted_auth_config: None,
            provider_config: None,
            endpoint_config: None,
            key_config: None,
            network: crate::network::OAuthNetworkContext::provider_operation(None),
        }
    }

    #[tokio::test]
    async fn grok_exchange_uses_xai_form_and_builds_refreshable_auth_config() {
        let seen_request = Arc::new(Mutex::new(None));
        let executor = StaticExecutor {
            seen_request: Arc::clone(&seen_request),
        };
        let adapter = GenericProviderOAuthAdapter::new(
            template_for_provider_type("grok").expect("grok template should exist"),
        );

        let exchanged = adapter
            .exchange_code(
                &executor,
                &grok_context(),
                "xai-code",
                "xai-state",
                Some("xai-verifier"),
            )
            .await
            .expect("xAI code exchange should succeed");

        let seen = seen_request
            .lock()
            .expect("mutex should lock")
            .clone()
            .expect("request should be captured");
        assert_eq!(seen.url, "https://auth.x.ai/oauth2/token");
        let form = String::from_utf8(seen.body_bytes.expect("form body should exist"))
            .expect("form body should be utf8");
        assert!(form.contains("grant_type=authorization_code"));
        assert!(form.contains("code=xai-code"));
        assert!(form.contains("code_verifier=xai-verifier"));
        assert!(form.contains("grok-cli%3Aaccess"));
        assert_eq!(exchanged.auth_config["access_token"], "new-access-token");
        assert_eq!(
            exchanged.auth_config["client_id"],
            "b1a00492-073a-47ea-816f-4c329264a828"
        );
        assert_eq!(
            exchanged.auth_config["base_url"],
            "https://cli-chat-proxy.grok.com/v1"
        );
        assert_eq!(
            exchanged.auth_config["token_endpoint"],
            "https://auth.x.ai/oauth2/token"
        );
    }

    #[async_trait]
    impl OAuthHttpExecutor for StaticExecutor {
        async fn execute(
            &self,
            request: OAuthHttpRequest,
        ) -> Result<OAuthHttpResponse, crate::core::OAuthError> {
            *self.seen_request.lock().expect("mutex should lock") = Some(request);
            Ok(OAuthHttpResponse {
                status_code: 200,
                body_text: json!({
                    "access_token": "new-access-token",
                    "expires_in": 3600
                })
                .to_string(),
                json_body: None,
            })
        }
    }

    #[tokio::test]
    async fn refresh_preserves_existing_metadata_when_refresh_token_is_not_rotated() {
        let seen_request = Arc::new(Mutex::new(None));
        let executor = StaticExecutor {
            seen_request: Arc::clone(&seen_request),
        };
        let adapter = GenericProviderOAuthAdapter::for_provider_type("codex")
            .expect("codex adapter should exist")
            .with_token_url_override("https://auth.example.test/token");
        let ctx = ProviderOAuthTransportContext {
            provider_id: "provider-1".to_string(),
            provider_type: "codex".to_string(),
            endpoint_id: None,
            key_id: Some("key-1".to_string()),
            auth_type: Some("oauth".to_string()),
            decrypted_api_key: None,
            decrypted_auth_config: None,
            provider_config: None,
            endpoint_config: None,
            key_config: None,
            network: crate::network::OAuthNetworkContext::provider_operation(None),
        };
        let account = ProviderOAuthAccount {
            provider_type: "codex".to_string(),
            access_token: "old-access-token".to_string(),
            auth_config: json!({
                "provider_type": "codex",
                "refresh_token": "old-refresh-token",
                "email": "alice@example.com",
                "account_id": "acct-123",
                "updated_at": 1
            }),
            expires_at_unix_secs: Some(1),
            identity: BTreeMap::new(),
        };

        let refreshed = adapter
            .refresh(&executor, &ctx, &account)
            .await
            .expect("refresh should succeed");

        assert_eq!(refreshed.token_set.access_token, "new-access-token");
        assert_eq!(
            refreshed.token_set.refresh_token.as_deref(),
            Some("old-refresh-token")
        );
        assert_eq!(refreshed.auth_config["email"], "alice@example.com");
        assert_eq!(refreshed.auth_config["account_id"], "acct-123");
        assert_eq!(refreshed.auth_config["refresh_token"], "old-refresh-token");

        let seen = seen_request
            .lock()
            .expect("mutex should lock")
            .clone()
            .expect("request should be captured");
        let form = String::from_utf8(seen.body_bytes.expect("form body should exist"))
            .expect("form body should be utf8");
        assert!(form.contains("grant_type=refresh_token"));
        assert!(form.contains("refresh_token=old-refresh-token"));
    }

    /// Replays a queued response per call so a multi-leg flow can be driven end
    /// to end, and keeps every request for inspection.
    struct ScriptedExecutor {
        responses: Mutex<std::collections::VecDeque<OAuthHttpResponse>>,
        seen_requests: Arc<Mutex<Vec<OAuthHttpRequest>>>,
    }

    impl ScriptedExecutor {
        fn new(
            responses: Vec<(u16, serde_json::Value)>,
            seen_requests: Arc<Mutex<Vec<OAuthHttpRequest>>>,
        ) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(|(status_code, body)| OAuthHttpResponse {
                            status_code,
                            body_text: body.to_string(),
                            json_body: None,
                        })
                        .collect(),
                ),
                seen_requests,
            }
        }
    }

    #[async_trait]
    impl OAuthHttpExecutor for ScriptedExecutor {
        async fn execute(
            &self,
            request: OAuthHttpRequest,
        ) -> Result<OAuthHttpResponse, crate::core::OAuthError> {
            self.seen_requests
                .lock()
                .expect("mutex should lock")
                .push(request);
            self.responses
                .lock()
                .expect("mutex should lock")
                .pop_front()
                .ok_or_else(|| crate::core::OAuthError::Transport("no scripted response".into()))
        }
    }

    fn grok_adapter() -> GenericProviderOAuthAdapter {
        GenericProviderOAuthAdapter::new(
            template_for_provider_type("grok").expect("grok template should exist"),
        )
    }

    fn discovery_body() -> serde_json::Value {
        json!({
            "device_authorization_endpoint": "https://auth.x.ai/oauth2/device/code",
            "token_endpoint": "https://auth.x.ai/oauth2/token",
        })
    }

    #[tokio::test]
    async fn grok_device_authorization_resolves_endpoints_through_discovery() {
        let seen_requests = Arc::new(Mutex::new(Vec::new()));
        let executor = ScriptedExecutor::new(
            vec![
                (200, discovery_body()),
                (
                    200,
                    json!({
                        "device_code": "device-code-1",
                        "user_code": "ABCD-1234",
                        "verification_uri": "https://x.ai/device",
                        "verification_uri_complete": "https://x.ai/device?code=ABCD-1234",
                        "expires_in": 600,
                        "interval": 7,
                    }),
                ),
            ],
            Arc::clone(&seen_requests),
        );

        let started = grok_adapter()
            .start_xai_device_authorization(&executor, &grok_context())
            .await
            .expect("device authorization should start");

        assert_eq!(started.authorization.user_code, "ABCD-1234");
        assert_eq!(started.authorization.device_code, "device-code-1");
        assert_eq!(started.authorization.interval, 7);
        assert_eq!(started.token_endpoint, "https://auth.x.ai/oauth2/token");

        let requests = seen_requests.lock().expect("mutex should lock").clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].url,
            "https://auth.x.ai/.well-known/openid-configuration"
        );
        assert_eq!(requests[1].url, "https://auth.x.ai/oauth2/device/code");
        let form = String::from_utf8(
            requests[1]
                .body_bytes
                .clone()
                .expect("form body should exist"),
        )
        .expect("form body should be utf8");
        assert!(form.contains("client_id=b1a00492-073a-47ea-816f-4c329264a828"));
        assert!(form.contains("grok-cli%3Aaccess"));
    }

    #[tokio::test]
    async fn grok_device_authorization_floors_a_missing_or_tiny_poll_interval() {
        let seen_requests = Arc::new(Mutex::new(Vec::new()));
        let executor = ScriptedExecutor::new(
            vec![
                (200, discovery_body()),
                (
                    200,
                    json!({
                        "device_code": "device-code-1",
                        "user_code": "ABCD-1234",
                        "verification_uri": "https://x.ai/device",
                        "interval": 1,
                    }),
                ),
            ],
            Arc::clone(&seen_requests),
        );

        let started = grok_adapter()
            .start_xai_device_authorization(&executor, &grok_context())
            .await
            .expect("device authorization should start");

        assert_eq!(started.authorization.interval, 5);
        // A device endpoint that omits verification_uri_complete still gives the
        // operator something to open.
        assert_eq!(
            started.authorization.verification_uri_complete,
            "https://x.ai/device"
        );
    }

    #[tokio::test]
    async fn grok_device_discovery_refuses_endpoints_that_leave_x_ai() {
        for evil in [
            json!({
                "device_authorization_endpoint": "https://evil.example.com/device",
                "token_endpoint": "https://auth.x.ai/oauth2/token",
            }),
            json!({
                "device_authorization_endpoint": "https://auth.x.ai/oauth2/device/code",
                "token_endpoint": "https://evil.example.com/token",
            }),
            json!({
                "device_authorization_endpoint": "http://auth.x.ai/oauth2/device/code",
                "token_endpoint": "https://auth.x.ai/oauth2/token",
            }),
        ] {
            let executor =
                ScriptedExecutor::new(vec![(200, evil)], Arc::new(Mutex::new(Vec::new())));
            let error = grok_adapter()
                .discover_xai_oauth_endpoints(&executor, &grok_context())
                .await
                .expect_err("discovery must reject an endpoint that is not https on x.ai");
            assert!(matches!(error, crate::core::OAuthError::InvalidResponse(_)));
        }
    }

    #[tokio::test]
    async fn an_overridden_discovery_may_only_describe_its_own_origin() {
        // Self-hosting and tests point discovery elsewhere; that must not become
        // a way to send device codes to a third host.
        let executor = ScriptedExecutor::new(
            vec![(
                200,
                json!({
                    "device_authorization_endpoint": "http://127.0.0.1:9999/device",
                    "token_endpoint": "https://evil.example.com/token",
                }),
            )],
            Arc::new(Mutex::new(Vec::new())),
        );
        let adapter = grok_adapter()
            .with_discovery_url_override("http://127.0.0.1:9999/.well-known/openid-configuration");
        let error = adapter
            .discover_xai_oauth_endpoints(&executor, &grok_context())
            .await
            .expect_err("an overridden discovery must not name a foreign origin");
        assert!(matches!(error, crate::core::OAuthError::InvalidResponse(_)));

        let executor = ScriptedExecutor::new(
            vec![(
                200,
                json!({
                    "device_authorization_endpoint": "http://127.0.0.1:9999/device",
                    "token_endpoint": "http://127.0.0.1:9999/token",
                }),
            )],
            Arc::new(Mutex::new(Vec::new())),
        );
        let resolved = grok_adapter()
            .with_discovery_url_override("http://127.0.0.1:9999/.well-known/openid-configuration")
            .discover_xai_oauth_endpoints(&executor, &grok_context())
            .await
            .expect("same-origin endpoints should be accepted");
        assert_eq!(resolved.token_endpoint, "http://127.0.0.1:9999/token");
    }

    #[tokio::test]
    async fn grok_device_poll_treats_pending_states_as_normal() {
        for (error_code, expected_slow_down) in
            [("authorization_pending", false), ("slow_down", true)]
        {
            let executor = ScriptedExecutor::new(
                vec![(400, json!({ "error": error_code }))],
                Arc::new(Mutex::new(Vec::new())),
            );
            let outcome = grok_adapter()
                .poll_xai_device_token(
                    &executor,
                    &grok_context(),
                    "device-code-1",
                    "https://auth.x.ai/oauth2/token",
                )
                .await
                .expect("a pending device authorization is not a failure");
            assert_eq!(
                outcome,
                XaiDevicePollOutcome::Pending {
                    slow_down: expected_slow_down
                }
            );
        }
    }

    #[tokio::test]
    async fn grok_device_poll_reports_terminal_states() {
        let executor = ScriptedExecutor::new(
            vec![(400, json!({ "error": "expired_token" }))],
            Arc::new(Mutex::new(Vec::new())),
        );
        assert_eq!(
            grok_adapter()
                .poll_xai_device_token(
                    &executor,
                    &grok_context(),
                    "device-code-1",
                    "https://auth.x.ai/oauth2/token",
                )
                .await
                .expect("expired is a poll outcome"),
            XaiDevicePollOutcome::Expired
        );

        let executor = ScriptedExecutor::new(
            vec![(
                400,
                json!({ "error": "access_denied", "error_description": "user refused" }),
            )],
            Arc::new(Mutex::new(Vec::new())),
        );
        assert_eq!(
            grok_adapter()
                .poll_xai_device_token(
                    &executor,
                    &grok_context(),
                    "device-code-1",
                    "https://auth.x.ai/oauth2/token",
                )
                .await
                .expect("denial is a poll outcome"),
            XaiDevicePollOutcome::Denied("user refused".to_string())
        );

        // An unrecognised failure must surface rather than look like pending.
        let executor = ScriptedExecutor::new(
            vec![(500, json!({ "error": "server_error" }))],
            Arc::new(Mutex::new(Vec::new())),
        );
        assert!(grok_adapter()
            .poll_xai_device_token(
                &executor,
                &grok_context(),
                "device-code-1",
                "https://auth.x.ai/oauth2/token",
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn grok_device_poll_builds_a_refreshable_auth_config() {
        let seen_requests = Arc::new(Mutex::new(Vec::new()));
        let executor = ScriptedExecutor::new(
            vec![(
                200,
                json!({
                    "access_token": "device-access-token",
                    "refresh_token": "device-refresh-token",
                    "expires_in": 3600,
                }),
            )],
            Arc::clone(&seen_requests),
        );

        let outcome = grok_adapter()
            .poll_xai_device_token(
                &executor,
                &grok_context(),
                "device-code-1",
                "https://device.auth.x.ai/oauth2/token",
            )
            .await
            .expect("poll should succeed");

        let XaiDevicePollOutcome::Authorized(token_set) = outcome else {
            panic!("expected an authorized outcome");
        };
        assert_eq!(token_set.token_set.access_token, "device-access-token");
        assert_eq!(
            token_set.auth_config.get("refresh_token"),
            Some(&json!("device-refresh-token"))
        );
        assert_eq!(
            token_set.auth_config.get("base_url"),
            Some(&json!("https://cli-chat-proxy.grok.com/v1"))
        );
        assert_eq!(
            token_set.auth_config.get("token_endpoint"),
            Some(&json!("https://device.auth.x.ai/oauth2/token"))
        );

        let requests = seen_requests.lock().expect("mutex should lock").clone();
        assert_eq!(requests[0].url, "https://device.auth.x.ai/oauth2/token");
        let form = String::from_utf8(
            requests[0]
                .body_bytes
                .clone()
                .expect("form body should exist"),
        )
        .expect("form body should be utf8");
        assert!(form.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
        assert!(form.contains("device_code=device-code-1"));
    }

    #[tokio::test]
    async fn grok_device_poll_refuses_a_token_endpoint_off_x_ai() {
        let executor = ScriptedExecutor::new(vec![], Arc::new(Mutex::new(Vec::new())));
        assert!(grok_adapter()
            .poll_xai_device_token(
                &executor,
                &grok_context(),
                "device-code-1",
                "https://evil.example.com/token",
            )
            .await
            .is_err());
    }

    #[test]
    fn grok_advertises_device_flow() {
        let capabilities = grok_adapter().capabilities();
        assert!(capabilities.supports_device_flow);
        // The authorization-code path still works and stays available as a
        // fallback until the device grant is proven against a live account.
        assert!(capabilities.supports_authorization_code);

        let codex = GenericProviderOAuthAdapter::new(
            template_for_provider_type("codex").expect("codex template should exist"),
        )
        .capabilities();
        assert!(!codex.supports_device_flow);
        assert!(codex.supports_authorization_code);
    }
}
