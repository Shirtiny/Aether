mod memory;
mod mysql;
mod postgres;
mod sqlite;

use serde_json::{Map, Value};

fn config_with_codex_client_headers(
    config: Option<Value>,
    client_headers: &Value,
) -> Result<Value, crate::DataLayerError> {
    let mut config = match config {
        Some(Value::Object(config)) => config,
        Some(_) => {
            return Err(crate::DataLayerError::UnexpectedValue(
                "provider config must be an object".to_string(),
            ));
        }
        None => Map::new(),
    };
    let pool_advanced = config
        .entry("pool_advanced".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(pool_advanced) = pool_advanced.as_object_mut() else {
        return Err(crate::DataLayerError::UnexpectedValue(
            "provider pool_advanced must be an object".to_string(),
        ));
    };
    pool_advanced.insert("codex_client_headers".to_string(), client_headers.clone());
    Ok(Value::Object(config))
}

#[allow(unused_imports)]
pub(crate) use aether_data_contracts::repository::provider_catalog::{
    ProviderCatalogKeyListOrder, ProviderCatalogKeyListQuery, ProviderCatalogReadRepository,
    ProviderCatalogWriteRepository, StoredProviderCatalogEndpoint, StoredProviderCatalogKey,
    StoredProviderCatalogKeyMaintenanceSummary, StoredProviderCatalogKeyPage,
    StoredProviderCatalogKeyStats, StoredProviderCatalogProvider,
};
pub use memory::InMemoryProviderCatalogReadRepository;
pub use mysql::MysqlProviderCatalogReadRepository;
pub use postgres::SqlxProviderCatalogReadRepository;
pub use sqlite::SqliteProviderCatalogReadRepository;
