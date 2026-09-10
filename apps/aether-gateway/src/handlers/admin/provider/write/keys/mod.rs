pub(crate) use self::create::build_admin_create_provider_key_record;
pub(crate) use self::payload::build_admin_provider_keys_page_payload;
pub(crate) use self::payload::build_admin_provider_keys_payload;
pub(crate) use self::update::build_admin_update_provider_key_record;

/// Whether a key is a personal-quota OAuth login (Grok / Codex) whose
/// concurrency starts at one.
pub(crate) fn provider_key_is_oauth_quota_account(provider_type: &str, auth_type: &str) -> bool {
    auth_type.trim().eq_ignore_ascii_case("oauth")
        && matches!(
            provider_type.trim().to_ascii_lowercase().as_str(),
            "grok" | "codex"
        )
}

fn normalize_provider_key_concurrent_limit(
    provider_type: &str,
    auth_type: &str,
    requested: Option<i32>,
    default_oauth_limit: bool,
) -> Result<Option<i32>, String> {
    // A newly added Codex/Grok OAuth account is a personal-quota login: one
    // in-flight request until an operator raises it. An explicit value,
    // including 0 for unlimited, always wins.
    let is_oauth_quota_account = provider_key_is_oauth_quota_account(provider_type, auth_type);
    let normalized = match requested {
        Some(value) if value >= 0 => Some(value),
        Some(_) => return Err("concurrent_limit 必须是非负整数".to_string()),
        None if default_oauth_limit && is_oauth_quota_account => Some(1),
        None => None,
    };
    Ok(normalized)
}

mod create;
mod payload;
mod update;

#[cfg(test)]
mod tests {
    use super::normalize_provider_key_concurrent_limit;

    #[test]
    fn grok_oauth_key_defaults_to_single_concurrency() {
        assert_eq!(
            normalize_provider_key_concurrent_limit("grok", "oauth", None, true)
                .expect("grok key limit"),
            Some(1)
        );
    }

    #[test]
    fn grok_oauth_key_allows_explicit_concurrency() {
        assert_eq!(
            normalize_provider_key_concurrent_limit("grok", "oauth", Some(8), true)
                .expect("explicit grok key limit"),
            Some(8)
        );
        assert_eq!(
            normalize_provider_key_concurrent_limit("grok", "oauth", Some(0), false)
                .expect("unlimited grok key concurrency"),
            Some(0)
        );
        assert_eq!(
            normalize_provider_key_concurrent_limit("grok", "oauth", None, false)
                .expect("cleared grok key concurrency"),
            None
        );
    }

    #[test]
    fn non_grok_key_keeps_unspecified_concurrency() {
        assert_eq!(
            normalize_provider_key_concurrent_limit("openai", "api_key", None, true)
                .expect("generic key limit"),
            None
        );
    }

    #[test]
    fn codex_oauth_key_defaults_to_single_concurrency_only_when_unspecified() {
        assert_eq!(
            normalize_provider_key_concurrent_limit("codex", "oauth", None, true)
                .expect("codex key limit"),
            Some(1)
        );
        // An operator's explicit value is never overridden, including 0.
        for value in [0, 4] {
            assert_eq!(
                normalize_provider_key_concurrent_limit("codex", "oauth", Some(value), true)
                    .expect("explicit codex key limit"),
                Some(value)
            );
        }
        // Clearing it stays cleared: the update path must not re-add the default.
        assert_eq!(
            normalize_provider_key_concurrent_limit("codex", "oauth", None, false)
                .expect("cleared codex key limit"),
            None
        );
        // API-key accounts are not quota logins.
        assert_eq!(
            normalize_provider_key_concurrent_limit("codex", "api_key", None, true)
                .expect("codex api key limit"),
            None
        );
    }
}
