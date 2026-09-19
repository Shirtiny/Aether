use super::*;
use aether_data_contracts::repository::provider_catalog::StoredProviderCatalogKey;

pub(super) const KEY_OVERRIDE_CONFIG: &str = "turn_state_source_key_id";

/// Provider cache IDs remain unchanged for backwards compatibility. Account IDs
/// include both bindings, so moving/deleting a key can never borrow another cache.
pub(super) fn account_source_id(provider_id: &str, key_id: &str) -> String {
    format!("account:{provider_id}:{key_id}")
}

pub(super) fn parse_source_id(id: &str) -> Option<(&str, Option<&str>)> {
    if let Some(rest) = id.strip_prefix("account:") {
        let (provider, key) = rest.split_once(':')?;
        (valid_source_id(provider) && valid_source_id(key)).then_some((provider, Some(key)))
    } else {
        valid_source_id(id).then_some((id, None))
    }
}

pub(crate) fn key_collection_config(key: &StoredProviderCatalogKey) -> Option<&Value> {
    key.fingerprint
        .as_ref()?
        .get(SOURCE_CONFIG)
        .filter(|v| !v.is_null())
}

/// Use the existing account-scoped metadata document, but expose a dedicated
/// admin field so editing collection never replaces a TLS/client/WS profile.
pub(crate) fn set_key_collection_config(
    fingerprint: &mut Option<Value>,
    value: Option<Value>,
) -> Result<(), String> {
    let value = value.filter(|v| !v.is_null());
    if let Some(value) = &value {
        parse_source_config(value)?;
    }
    let mut object = fingerprint
        .take()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    if let Some(value) = value {
        object.insert(SOURCE_CONFIG.into(), value);
    } else {
        object.remove(SOURCE_CONFIG);
    }
    *fingerprint = (!object.is_empty()).then_some(Value::Object(object));
    Ok(())
}

pub(crate) fn validate_key_collection_config(key: &StoredProviderCatalogKey) -> Result<(), String> {
    if let Some(value) = key_collection_config(key).filter(|v| !v.is_null()) {
        parse_source_config(value)?;
    }
    Ok(())
}

fn account_config(
    provider: &StoredProviderCatalogProvider,
    key: &StoredProviderCatalogKey,
) -> Option<SourceConfig> {
    let config = parse_source_config(key_collection_config(key)?).ok()?;
    // An explicit collection opt-in is independent of normal pool scheduling.
    (provider.is_active && key.provider_id == provider.id && config.enabled).then_some(config)
}

pub(super) async fn current_source_config(
    state: &AppState,
    source_id: &str,
) -> Result<Option<SourceConfig>, String> {
    let (provider_id, key_id) = parse_source_id(source_id).ok_or("票据来源标识无效")?;
    let provider = state
        .read_provider_catalog_providers_by_ids(&[provider_id.to_owned()])
        .await
        .map_err(|_| "读取票据采集渠道失败")?
        .into_iter()
        .next();
    let Some(provider) = provider else {
        return Ok(None);
    };
    if let Some(key_id) = key_id {
        let key = state
            .read_provider_catalog_keys_by_ids(&[key_id.to_owned()])
            .await
            .map_err(|_| "读取票据采集账号失败")?
            .into_iter()
            .next();
        Ok(key.as_ref().and_then(|key| account_config(&provider, key)))
    } else {
        Ok(source_config(&provider))
    }
}

/// Safe, lightweight summary used by the source dropdown; never credentials.
pub(crate) fn account_sources(
    provider: &StoredProviderCatalogProvider,
    keys: &[StoredProviderCatalogKey],
) -> Value {
    Value::Array(
        keys.iter()
            .filter(|key| account_config(provider, key).is_some())
            .map(|key| json!({"key_id": key.id, "name": key.name}))
            .collect(),
    )
}
