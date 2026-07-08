use serde_json::Value;

pub(crate) const PROVIDER_ANONYMOUS_AVOIDANCE_SKIP_REASON: &str = "provider_anonymous_avoidance";

const PROVIDER_ANONYMOUS_AVOIDANCE_KEYS: &[&str] = &[
    "avoid_anonymous",
    "avoid_anonymous_requests",
    "anonymous_avoidance_enabled",
    "avoid_anonymous_requests_enabled",
];

pub(crate) fn provider_anonymous_avoidance_enabled(config: Option<&Value>) -> bool {
    let Some(config) = config.and_then(Value::as_object) else {
        return false;
    };

    bool_from_any_key(config, PROVIDER_ANONYMOUS_AVOIDANCE_KEYS).unwrap_or_else(|| {
        config
            .get("pool_advanced")
            .and_then(Value::as_object)
            .and_then(|pool_advanced| {
                bool_from_any_key(pool_advanced, PROVIDER_ANONYMOUS_AVOIDANCE_KEYS)
            })
            .unwrap_or(false)
    })
}

fn bool_from_any_key(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_bool))
}

#[cfg(test)]
mod tests {
    use super::provider_anonymous_avoidance_enabled;
    use serde_json::json;

    #[test]
    fn anonymous_avoidance_defaults_to_disabled() {
        assert!(!provider_anonymous_avoidance_enabled(None));
        assert!(!provider_anonymous_avoidance_enabled(Some(&json!({}))));
        assert!(!provider_anonymous_avoidance_enabled(Some(&json!({
            "pool_advanced": {}
        }))));
    }

    #[test]
    fn anonymous_avoidance_reads_pool_advanced_primary_key() {
        assert!(provider_anonymous_avoidance_enabled(Some(&json!({
            "pool_advanced": {
                "avoid_anonymous": true
            }
        }))));
        assert!(!provider_anonymous_avoidance_enabled(Some(&json!({
            "pool_advanced": {
                "avoid_anonymous": false,
                "anonymous_avoidance_enabled": true
            }
        }))));
    }

    #[test]
    fn anonymous_avoidance_supports_top_level_compatibility_key() {
        assert!(provider_anonymous_avoidance_enabled(Some(&json!({
            "anonymous_avoidance_enabled": true
        }))));
    }
}
