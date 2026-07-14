use crate::core::{current_unix_secs, OAuthAuthorizeResponse, OAuthError, OAuthTokenSet};
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
pub const XAI_DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
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
        GenericProviderOAuthAdapter, XaiUrlKind,
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
        assert_eq!(exchanged.auth_config["base_url"], "https://api.x.ai/v1");
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
}
