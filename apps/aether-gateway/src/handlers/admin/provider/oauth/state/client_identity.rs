use aether_data::repository::provider_oauth::StoredAdminProviderOAuthState;
use serde_json::Value;

/// Client identity (`User-Agent` + `originator`) a codex OAuth login advertises.
///
/// codex-rs sends the same `originator`/`User-Agent` pair on the authorize
/// URL, the code exchange, the api-key exchange and every later refresh. The
/// pair is therefore chosen once when the authorize URL is built, stored with
/// the OAuth state, replayed on the token exchange and frozen into the key
/// fingerprint so pool traffic keeps the identity the account logged in with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdminProviderOAuthClientIdentity {
    pub(crate) user_agent: String,
    pub(crate) originator: String,
}

impl AdminProviderOAuthClientIdentity {
    /// New login/import: the account is unknown until the token arrives, so the
    /// profile is picked from the provider's pool profiles with a one-off seed
    /// (the OAuth state nonce). Returns `None` for non-codex providers or when
    /// codex client header profiles are disabled.
    pub(crate) fn for_new_codex_login(
        provider_type: &str,
        provider_config: Option<&Value>,
        seed: &str,
    ) -> Option<Self> {
        let (user_agent, originator) = crate::ai_serving::select_codex_pool_client_header_profile(
            provider_type,
            provider_config,
            seed,
        )?;
        Some(Self {
            user_agent,
            originator,
        })
    }

    /// Re-login of an existing key: keep the identity the key already uses so
    /// the login and the pool traffic before/after it look like one client.
    /// Falls back to a profile seeded by the key id when nothing is persisted.
    pub(crate) fn for_existing_codex_key(
        provider_type: &str,
        provider_config: Option<&Value>,
        key_fingerprint: Option<&Value>,
        key_id: &str,
    ) -> Option<Self> {
        // Resolving the fallback first also applies the "profiles disabled"
        // gate, so a disabled provider never advertises a persisted identity.
        let fallback = Self::for_new_codex_login(provider_type, provider_config, key_id)?;
        match crate::codex_profile::codex_profile_persisted_client_headers(key_fingerprint) {
            Some((user_agent, originator)) => Some(Self {
                user_agent,
                originator,
            }),
            None => Some(fallback),
        }
    }

    /// Identity recorded with the OAuth state when the authorize URL was built.
    pub(crate) fn from_stored_state(state: &StoredAdminProviderOAuthState) -> Option<Self> {
        let user_agent = non_empty(state.client_user_agent.as_deref())?;
        let originator = non_empty(state.client_originator.as_deref())?;
        Some(Self {
            user_agent,
            originator,
        })
    }
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::AdminProviderOAuthClientIdentity;
    use aether_data::repository::provider_oauth::StoredAdminProviderOAuthState;
    use serde_json::json;

    #[test]
    fn new_codex_login_picks_a_profile_from_defaults() {
        let identity = AdminProviderOAuthClientIdentity::for_new_codex_login("codex", None, "seed")
            .expect("codex should resolve a default profile");
        assert!(!identity.user_agent.is_empty());
        assert!(!identity.originator.is_empty());
        // Stable for the same seed.
        assert_eq!(
            AdminProviderOAuthClientIdentity::for_new_codex_login("codex", None, "seed"),
            Some(identity)
        );
    }

    #[test]
    fn new_login_is_codex_only_and_honors_disable() {
        assert!(
            AdminProviderOAuthClientIdentity::for_new_codex_login("openai", None, "seed").is_none()
        );
        let disabled = json!({
            "pool_advanced": { "codex_client_headers": { "enabled": false } }
        });
        assert!(AdminProviderOAuthClientIdentity::for_new_codex_login(
            "codex",
            Some(&disabled),
            "seed"
        )
        .is_none());
    }

    #[test]
    fn existing_key_prefers_persisted_client_headers() {
        let fingerprint = json!({
            "codex_client_profile": {
                "client_headers": {
                    "user_agent": "Codex Desktop/0.153.1 (Windows 10.0.26100; x86_64)",
                    "originator": "Codex Desktop",
                }
            }
        });
        let identity = AdminProviderOAuthClientIdentity::for_existing_codex_key(
            "codex",
            None,
            Some(&fingerprint),
            "key-1",
        )
        .expect("identity should resolve");
        assert_eq!(
            identity,
            AdminProviderOAuthClientIdentity {
                user_agent: "Codex Desktop/0.153.1 (Windows 10.0.26100; x86_64)".to_string(),
                originator: "Codex Desktop".to_string(),
            }
        );
    }

    #[test]
    fn existing_key_without_persisted_headers_uses_key_id_seed() {
        let identity =
            AdminProviderOAuthClientIdentity::for_existing_codex_key("codex", None, None, "key-1")
                .expect("identity should resolve");
        assert_eq!(
            Some(identity),
            AdminProviderOAuthClientIdentity::for_new_codex_login("codex", None, "key-1")
        );
    }

    #[test]
    fn stored_state_round_trips_identity() {
        let state = StoredAdminProviderOAuthState {
            key_id: String::new(),
            provider_id: "p".to_string(),
            provider_type: "codex".to_string(),
            pkce_verifier: None,
            client_user_agent: Some(" codex-tui/0.153.3 (Debian 13.0.0; x86_64) ".to_string()),
            client_originator: Some("codex-tui".to_string()),
        };
        assert_eq!(
            AdminProviderOAuthClientIdentity::from_stored_state(&state),
            Some(AdminProviderOAuthClientIdentity {
                user_agent: "codex-tui/0.153.3 (Debian 13.0.0; x86_64)".to_string(),
                originator: "codex-tui".to_string(),
            })
        );
        let legacy = StoredAdminProviderOAuthState {
            client_user_agent: None,
            client_originator: None,
            ..state
        };
        assert!(AdminProviderOAuthClientIdentity::from_stored_state(&legacy).is_none());
    }
}
