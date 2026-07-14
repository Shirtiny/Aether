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
    if is_grok_oauth && normalized != Some(1) && !grok_unsafe_concurrency_override_enabled() {
        return Err(
            "Grok OAuth Key 默认只允许并发 1；如确认风险，请设置 XAI_GROK_UNSAFE_ALLOW_CONCURRENCY_GT_ONE=true"
                .to_string(),
        );
    }
    Ok(normalized)
}

pub(crate) fn grok_unsafe_concurrency_override_enabled() -> bool {
    std::env::var("XAI_GROK_UNSAFE_ALLOW_CONCURRENCY_GT_ONE")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
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
    fn non_grok_key_keeps_unspecified_concurrency() {
        assert_eq!(
            normalize_provider_key_concurrent_limit("openai", "api_key", None, true)
                .expect("generic key limit"),
            None
        );
    }
}
