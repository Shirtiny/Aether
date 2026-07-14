mod antigravity;
mod codex;
mod generic;
mod kiro;
mod windsurf;

pub use antigravity::AntigravityProviderOAuthAdapter;
pub use codex::CodexProviderOAuthAdapter;
pub use generic::{
    effective_xai_base_url, effective_xai_oauth_authorize_url, effective_xai_oauth_redirect_uri,
    effective_xai_oauth_token_url, xai_allow_unsafe_url_overrides, GenericProviderOAuthAdapter,
    GenericProviderOAuthTemplate, GENERIC_PROVIDER_OAUTH_TEMPLATES, XAI_DEFAULT_AUTHORIZE_URL,
    XAI_DEFAULT_BASE_URL, XAI_DEFAULT_REDIRECT_URI, XAI_DEFAULT_TOKEN_URL,
};
pub use kiro::{
    generate_kiro_machine_id, normalize_kiro_machine_id, KiroAuthConfig, KiroProviderOAuthAdapter,
    DEFAULT_KIRO_VERSION, DEFAULT_NODE_VERSION, DEFAULT_REGION, DEFAULT_SYSTEM_VERSION,
    KIRO_PROVIDER_TYPE,
};
pub use windsurf::{
    WindsurfProviderOAuthAdapter, WINDSURF_CLIENT_ID, WINDSURF_PROVIDER_TYPE,
    WINDSURF_SHOW_AUTH_TOKEN_REDIRECT, WINDSURF_SIGNIN_URL,
};
