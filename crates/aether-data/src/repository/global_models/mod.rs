mod memory;
mod mysql;
mod postgres;
mod sqlite;

use serde_json::Value;

#[allow(unused_imports)]
pub(crate) use aether_data_contracts::repository::global_models::{
    AdminGlobalModelListQuery, AdminProviderModelListQuery, CreateAdminGlobalModelRecord,
    GlobalModelReadRepository, GlobalModelWriteRepository, PublicCatalogModelListQuery,
    PublicCatalogModelSearchQuery, PublicGlobalModelQuery, StoredAdminGlobalModel,
    StoredAdminGlobalModelPage, StoredAdminProviderModel, StoredProviderActiveGlobalModel,
    StoredProviderModelStats, StoredPublicCatalogModel, StoredPublicGlobalModel,
    StoredPublicGlobalModelPage, UpdateAdminGlobalModelRecord, UpsertAdminProviderModelRecord,
};
pub use memory::InMemoryGlobalModelReadRepository;
pub use mysql::MysqlGlobalModelReadRepository;
pub use postgres::SqlxGlobalModelReadRepository;
pub use sqlite::SqliteGlobalModelReadRepository;

const EMBEDDING_CAPABILITY: &str = "embedding";
const EMBEDDING_API_FORMATS: &[&str] = &[
    "openai:embedding",
    "gemini:embedding",
    "jina:embedding",
    "doubao:embedding",
    "aliyun:multimodal_embedding",
    "/v1/embeddings",
    "/jina/v1/embeddings",
];

pub(super) fn metadata_supports_embedding(
    supported_capabilities: Option<&Value>,
    global_config: Option<&Value>,
    model_config: Option<&Value>,
) -> Option<bool> {
    Some(
        supported_capabilities.is_some_and(value_contains_embedding_capability)
            || global_config.is_some_and(value_contains_embedding_metadata)
            || model_config.is_some_and(value_contains_embedding_metadata),
    )
}

fn value_contains_embedding_capability(value: &Value) -> bool {
    match value {
        Value::String(value) => value.trim().eq_ignore_ascii_case(EMBEDDING_CAPABILITY),
        Value::Array(values) => values.iter().any(value_contains_embedding_capability),
        Value::Object(object) => {
            object
                .get(EMBEDDING_CAPABILITY)
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || [
                    "capability",
                    "model_type",
                    "type",
                    "task_type",
                    "request_type",
                ]
                .iter()
                .any(|key| {
                    object
                        .get(*key)
                        .is_some_and(value_contains_embedding_capability)
                })
                || ["capabilities", "supported_capabilities"]
                    .iter()
                    .any(|key| {
                        object
                            .get(*key)
                            .is_some_and(value_contains_embedding_capability)
                    })
        }
        _ => false,
    }
}

fn value_contains_embedding_metadata(value: &Value) -> bool {
    match value {
        Value::String(value) => {
            value.trim().eq_ignore_ascii_case(EMBEDDING_CAPABILITY)
                || is_known_embedding_api_format(value)
        }
        Value::Array(values) => values.iter().any(value_contains_embedding_metadata),
        Value::Object(object) => {
            value_contains_embedding_capability(value)
                || ["api_format", "client_api_format", "provider_api_format"]
                    .iter()
                    .any(|key| {
                        object
                            .get(*key)
                            .and_then(Value::as_str)
                            .is_some_and(is_known_embedding_api_format)
                    })
                || ["api_formats", "client_api_formats", "provider_api_formats"]
                    .iter()
                    .any(|key| {
                        object
                            .get(*key)
                            .is_some_and(value_contains_embedding_metadata)
                    })
        }
        _ => false,
    }
}

fn is_known_embedding_api_format(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    EMBEDDING_API_FORMATS
        .iter()
        .any(|format| normalized == *format || normalized.ends_with(*format))
}
