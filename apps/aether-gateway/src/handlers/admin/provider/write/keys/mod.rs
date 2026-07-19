pub(crate) use self::create::build_admin_create_provider_key_record;
pub(crate) use self::payload::build_admin_provider_keys_page_payload;
pub(crate) use self::payload::build_admin_provider_keys_payload;
pub(crate) use self::update::build_admin_update_provider_key_record;

fn normalize_provider_key_concurrent_limit(
    provider_type: &str,
    auth_type: &str,
    requested: Option<i32>,
    default_grok_oauth_limit: bool,
) -> Result<Option<i32>, String> {
    let is_grok_oauth = provider_type.trim().eq_ignore_ascii_case("grok")
        && auth_type.trim().eq_ignore_ascii_case("oauth");
    let normalized = match requested {
        Some(value) if value >= 0 => Some(value),
        Some(_) => return Err("concurrent_limit 必须是非负整数".to_string()),
        None if default_grok_oauth_limit && is_grok_oauth => Some(1),
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
}
