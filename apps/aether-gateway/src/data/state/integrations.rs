use aether_billing::enrich_usage_event_with_billing;
use aether_billing::BillingModelContextLookup;
use aether_data::repository::audit::RequestAuditReader;
use aether_data::repository::auth::{
    AuthApiKeyLookupKey, ResolvedAuthApiKeySnapshotReader, StoredAuthApiKeySnapshot,
};
use aether_data::DataLayerError;
use aether_data_contracts::repository::billing::StoredBillingModelContext;
use aether_data_contracts::repository::candidate_selection::StoredMinimalCandidateSelectionRow;
use aether_data_contracts::repository::candidates::DecisionTrace;
use aether_data_contracts::repository::provider_catalog::{
    StoredProviderCatalogEndpoint, StoredProviderCatalogKey, StoredProviderCatalogProvider,
};
use aether_data_contracts::repository::settlement::{StoredUsageSettlement, UsageSettlementInput};
use aether_data_contracts::repository::usage::{
    ProxyNodeCounterDelta, StoredRequestUsageAudit, UpsertUsageRecord,
};
use aether_data_contracts::repository::video_tasks::{StoredVideoTask, VideoTaskLookupKey};
use aether_runtime_state::RuntimeQueueStore;
use aether_usage_runtime::{
    UsageBillingEventEnricher, UsageBodyCapturePolicy, UsageEvent, UsagePromptCapturePolicy,
    UsageRecordWriter, UsageRequestRecordLevel, UsageRuntimeAccess, UsageSettlementWriter,
    DEFAULT_USAGE_REQUEST_BODY_CAPTURE_LIMIT_BYTES,
    DEFAULT_USAGE_RESPONSE_BODY_CAPTURE_LIMIT_BYTES,
};
use aether_video_tasks_core::StoredVideoTaskReadSide;
use async_trait::async_trait;
use serde_json::Value;

use super::GatewayDataState;
use crate::data::candidate_selection::MinimalCandidateSelectionRowSource;
use crate::provider_transport::ProviderTransportSnapshotSource;

const REQUEST_RECORD_LEVEL_KEY: &str = "request_record_level";
const LEGACY_REQUEST_LOG_LEVEL_KEY: &str = "request_log_level";
const MAX_REQUEST_BODY_SIZE_KEY: &str = "max_request_body_size";
const MAX_RESPONSE_BODY_SIZE_KEY: &str = "max_response_body_size";
const REQUEST_CAPTURE_POLICY_KEY: &str = "request_capture_policy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestCaptureScopeMode {
    All,
    IncludeGroups,
    ExcludeGroups,
}

#[derive(Debug, Clone)]
struct RequestCaptureScope {
    mode: RequestCaptureScopeMode,
    group_ids: Vec<String>,
}

impl Default for RequestCaptureScope {
    fn default() -> Self {
        Self {
            mode: RequestCaptureScopeMode::All,
            group_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct RequestCapturePolicyConfig {
    scope: RequestCaptureScope,
    body: UsageBodyCapturePolicy,
}

fn usage_request_record_level_from_value(value: Option<&Value>) -> UsageRequestRecordLevel {
    let Some(value) = value.and_then(Value::as_str).map(str::trim) else {
        return UsageRequestRecordLevel::Full;
    };

    if value.eq_ignore_ascii_case("basic")
        || value.eq_ignore_ascii_case("base")
        || value.eq_ignore_ascii_case("headers")
        || value.eq_ignore_ascii_case("minimal")
        || value.eq_ignore_ascii_case("none")
    {
        UsageRequestRecordLevel::Basic
    } else {
        UsageRequestRecordLevel::Full
    }
}

fn usage_body_capture_limit_from_value(value: Option<&Value>, default: usize) -> Option<usize> {
    match value.and_then(Value::as_u64) {
        Some(0) => None,
        Some(limit) => usize::try_from(limit).ok().filter(|limit| *limit > 0),
        None => Some(default),
    }
}

fn request_capture_policy_from_values(
    policy_value: Option<&Value>,
    record_level_value: Option<&Value>,
    max_request_body_size_value: Option<&Value>,
    max_response_body_size_value: Option<&Value>,
) -> RequestCapturePolicyConfig {
    let mut body = UsageBodyCapturePolicy {
        record_level: usage_request_record_level_from_value(record_level_value),
        max_request_body_bytes: usage_body_capture_limit_from_value(
            max_request_body_size_value,
            DEFAULT_USAGE_REQUEST_BODY_CAPTURE_LIMIT_BYTES,
        ),
        max_response_body_bytes: usage_body_capture_limit_from_value(
            max_response_body_size_value,
            DEFAULT_USAGE_RESPONSE_BODY_CAPTURE_LIMIT_BYTES,
        ),
        prompt_capture: UsagePromptCapturePolicy::default(),
    };
    let mut scope = RequestCaptureScope::default();

    let Some(policy_object) = policy_value.and_then(Value::as_object) else {
        return RequestCapturePolicyConfig { scope, body };
    };

    if let Some(level) = policy_object
        .get(REQUEST_RECORD_LEVEL_KEY)
        .or_else(|| policy_object.get(LEGACY_REQUEST_LOG_LEVEL_KEY))
        .or_else(|| policy_object.get("body_record_level"))
    {
        body.record_level = usage_request_record_level_from_value(Some(level));
    }
    body.max_request_body_bytes = usage_body_capture_limit_from_value(
        policy_object
            .get(MAX_REQUEST_BODY_SIZE_KEY)
            .or_else(|| policy_object.get("max_request_body_bytes")),
        body.max_request_body_bytes
            .unwrap_or(DEFAULT_USAGE_REQUEST_BODY_CAPTURE_LIMIT_BYTES),
    );
    body.max_response_body_bytes = usage_body_capture_limit_from_value(
        policy_object
            .get(MAX_RESPONSE_BODY_SIZE_KEY)
            .or_else(|| policy_object.get("max_response_body_bytes")),
        body.max_response_body_bytes
            .unwrap_or(DEFAULT_USAGE_RESPONSE_BODY_CAPTURE_LIMIT_BYTES),
    );
    body.prompt_capture =
        usage_prompt_capture_policy_from_value(policy_object.get("prompt_capture"));
    scope = request_capture_scope_from_value(policy_value);

    RequestCapturePolicyConfig { scope, body }
}

fn request_capture_scope_from_value(value: Option<&Value>) -> RequestCaptureScope {
    let Some(object) = value.and_then(Value::as_object) else {
        return RequestCaptureScope::default();
    };
    let scope_object = object.get("scope").and_then(Value::as_object);
    let mode_value = scope_object
        .and_then(|scope| scope.get("mode"))
        .or_else(|| object.get("scope_mode"))
        .and_then(Value::as_str)
        .unwrap_or("all");
    let mode = if mode_value.eq_ignore_ascii_case("include_groups")
        || mode_value.eq_ignore_ascii_case("include")
        || mode_value.eq_ignore_ascii_case("only_groups")
    {
        RequestCaptureScopeMode::IncludeGroups
    } else if mode_value.eq_ignore_ascii_case("exclude_groups")
        || mode_value.eq_ignore_ascii_case("exclude")
        || mode_value.eq_ignore_ascii_case("except_groups")
    {
        RequestCaptureScopeMode::ExcludeGroups
    } else {
        RequestCaptureScopeMode::All
    };
    let group_ids = string_array_from_value(
        scope_object
            .and_then(|scope| scope.get("group_ids"))
            .or_else(|| object.get("group_ids"))
            .or_else(|| object.get("user_group_ids")),
    );

    RequestCaptureScope { mode, group_ids }
}

fn usage_prompt_capture_policy_from_value(value: Option<&Value>) -> UsagePromptCapturePolicy {
    let mut policy = UsagePromptCapturePolicy::default();
    let Some(object) = value.and_then(Value::as_object) else {
        policy.enabled = value.and_then(Value::as_bool).unwrap_or(false);
        return policy;
    };
    policy.enabled = object
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(policy.enabled);
    policy.include_system = object
        .get("include_system")
        .or_else(|| object.get("system"))
        .and_then(Value::as_bool)
        .unwrap_or(policy.include_system);
    policy.include_developer = object
        .get("include_developer")
        .or_else(|| object.get("developer"))
        .and_then(Value::as_bool)
        .unwrap_or(policy.include_developer);
    policy.include_user = object
        .get("include_user")
        .or_else(|| object.get("user"))
        .and_then(Value::as_bool)
        .unwrap_or(policy.include_user);
    policy.include_tools = object
        .get("include_tools")
        .or_else(|| object.get("tools"))
        .and_then(Value::as_bool)
        .unwrap_or(policy.include_tools);
    policy.preview_chars = usize_from_value(object.get("preview_chars"))
        .or_else(|| usize_from_value(object.get("max_preview_chars")))
        .unwrap_or(policy.preview_chars)
        .min(8_192);
    policy.max_items = usize_from_value(object.get("max_items"))
        .unwrap_or(policy.max_items)
        .min(256);
    policy
}

fn usize_from_value(value: Option<&Value>) -> Option<usize> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn string_array_from_value(value: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

#[async_trait]
impl RequestAuditReader for GatewayDataState {
    async fn find_request_usage_audit_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<StoredRequestUsageAudit>, DataLayerError> {
        GatewayDataState::find_request_usage_by_request_id(self, request_id).await
    }

    async fn read_request_decision_trace(
        &self,
        request_id: &str,
        attempted_only: bool,
    ) -> Result<Option<DecisionTrace>, DataLayerError> {
        GatewayDataState::read_decision_trace(self, request_id, attempted_only).await
    }

    async fn read_resolved_auth_api_key_snapshot(
        &self,
        user_id: &str,
        api_key_id: &str,
        now_unix_secs: u64,
    ) -> Result<Option<aether_data::repository::auth::ResolvedAuthApiKeySnapshot>, DataLayerError>
    {
        GatewayDataState::read_auth_api_key_snapshot(self, user_id, api_key_id, now_unix_secs).await
    }
}

#[async_trait]
impl ResolvedAuthApiKeySnapshotReader for GatewayDataState {
    async fn find_stored_auth_api_key_snapshot(
        &self,
        key: AuthApiKeyLookupKey<'_>,
    ) -> Result<Option<StoredAuthApiKeySnapshot>, DataLayerError> {
        GatewayDataState::find_auth_api_key_snapshot(self, key).await
    }
}

#[async_trait]
impl StoredVideoTaskReadSide for GatewayDataState {
    async fn find_stored_video_task(
        &self,
        key: VideoTaskLookupKey<'_>,
    ) -> Result<Option<StoredVideoTask>, DataLayerError> {
        GatewayDataState::find_video_task(self, key).await
    }
}

#[async_trait]
impl ProviderTransportSnapshotSource for GatewayDataState {
    fn encryption_key(&self) -> Option<&str> {
        GatewayDataState::encryption_key(self)
    }

    async fn list_provider_catalog_providers_by_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<StoredProviderCatalogProvider>, DataLayerError> {
        GatewayDataState::list_provider_catalog_providers_by_ids(self, ids).await
    }

    async fn list_provider_catalog_endpoints_by_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<StoredProviderCatalogEndpoint>, DataLayerError> {
        GatewayDataState::list_provider_catalog_endpoints_by_ids(self, ids).await
    }

    async fn list_provider_catalog_keys_by_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<StoredProviderCatalogKey>, DataLayerError> {
        GatewayDataState::list_provider_catalog_keys_by_ids(self, ids).await
    }
}

#[async_trait]
impl MinimalCandidateSelectionRowSource for GatewayDataState {
    async fn read_minimal_candidate_selection_rows_for_api_format_and_global_model(
        &self,
        api_format: &str,
        global_model_name: &str,
    ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
        self.list_minimal_candidate_selection_rows(api_format, global_model_name)
            .await
    }

    async fn read_minimal_candidate_selection_rows_for_api_format_and_requested_model(
        &self,
        api_format: &str,
        requested_model_name: &str,
    ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
        self.list_minimal_candidate_selection_rows_for_requested_model(
            api_format,
            requested_model_name,
        )
        .await
    }

    async fn read_minimal_candidate_selection_rows_for_api_format_and_requested_model_page(
        &self,
        query: &aether_data_contracts::repository::candidate_selection::StoredRequestedModelCandidateRowsQuery,
    ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
        self.list_minimal_candidate_selection_rows_for_requested_model_page(query)
            .await
    }

    async fn read_minimal_candidate_selection_rows_for_api_format(
        &self,
        api_format: &str,
    ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
        self.list_minimal_candidate_selection_rows_for_api_format(api_format)
            .await
    }

    async fn read_pool_key_candidate_rows_for_group(
        &self,
        query: &aether_data_contracts::repository::candidate_selection::StoredPoolKeyCandidateRowsQuery,
    ) -> Result<Vec<StoredMinimalCandidateSelectionRow>, DataLayerError> {
        self.list_pool_key_candidate_rows_for_group(query).await
    }
}

#[async_trait]
impl BillingModelContextLookup for GatewayDataState {
    async fn find_billing_model_context_by_model_id(
        &self,
        provider_id: &str,
        provider_api_key_id: Option<&str>,
        model_id: &str,
    ) -> Result<Option<StoredBillingModelContext>, DataLayerError> {
        GatewayDataState::find_billing_model_context_by_model_id(
            self,
            provider_id,
            provider_api_key_id,
            model_id,
        )
        .await
    }

    async fn find_billing_model_context(
        &self,
        provider_id: &str,
        provider_api_key_id: Option<&str>,
        global_model_name: &str,
    ) -> Result<Option<StoredBillingModelContext>, DataLayerError> {
        GatewayDataState::find_billing_model_context(
            self,
            provider_id,
            provider_api_key_id,
            global_model_name,
        )
        .await
    }
}

#[async_trait]
impl UsageSettlementWriter for GatewayDataState {
    fn has_usage_settlement_writer(&self) -> bool {
        GatewayDataState::has_settlement_writer(self)
    }

    async fn settle_usage(
        &self,
        input: UsageSettlementInput,
    ) -> Result<Option<StoredUsageSettlement>, DataLayerError> {
        GatewayDataState::settle_usage(self, input).await
    }
}

#[async_trait]
impl UsageBillingEventEnricher for GatewayDataState {
    async fn enrich_usage_event(&self, event: &mut UsageEvent) -> Result<(), DataLayerError> {
        enrich_usage_event_with_billing(self, event).await
    }
}

#[async_trait]
impl UsageRuntimeAccess for GatewayDataState {
    fn has_usage_writer(&self) -> bool {
        GatewayDataState::has_usage_writer(self)
    }

    fn has_usage_worker_queue(&self) -> bool {
        GatewayDataState::has_usage_worker_queue(self)
    }

    fn usage_worker_queue(&self) -> Option<std::sync::Arc<dyn RuntimeQueueStore>> {
        GatewayDataState::usage_worker_queue(self)
    }

    async fn body_capture_policy(&self) -> Result<UsageBodyCapturePolicy, DataLayerError> {
        self.body_capture_policy_for_user(None).await
    }

    async fn body_capture_policy_for_user(
        &self,
        user_id: Option<&str>,
    ) -> Result<UsageBodyCapturePolicy, DataLayerError> {
        let value = GatewayDataState::find_system_config_value(self, REQUEST_RECORD_LEVEL_KEY)
            .await?
            .or(
                GatewayDataState::find_system_config_value(self, LEGACY_REQUEST_LOG_LEVEL_KEY)
                    .await?,
            );
        let request_capture_policy =
            GatewayDataState::find_system_config_value(self, REQUEST_CAPTURE_POLICY_KEY).await?;
        let max_request_body_size =
            GatewayDataState::find_system_config_value(self, MAX_REQUEST_BODY_SIZE_KEY).await?;
        let max_response_body_size =
            GatewayDataState::find_system_config_value(self, MAX_RESPONSE_BODY_SIZE_KEY).await?;
        let policy = request_capture_policy_from_values(
            request_capture_policy.as_ref(),
            value.as_ref(),
            max_request_body_size.as_ref(),
            max_response_body_size.as_ref(),
        );
        let Some(config_value) = request_capture_policy.as_ref() else {
            return Ok(policy.body);
        };
        if self
            .request_capture_scope_matches_user(&policy.scope, user_id)
            .await?
        {
            return Ok(policy.body);
        }

        let mut out_of_scope = UsageBodyCapturePolicy::default();
        out_of_scope.record_level = UsageRequestRecordLevel::Basic;
        if let Some(out_of_scope_value) = config_value
            .as_object()
            .and_then(|object| object.get("out_of_scope_record_level"))
        {
            out_of_scope.record_level =
                usage_request_record_level_from_value(Some(out_of_scope_value));
        }
        Ok(out_of_scope)
    }
}

impl GatewayDataState {
    async fn request_capture_scope_matches_user(
        &self,
        scope: &RequestCaptureScope,
        user_id: Option<&str>,
    ) -> Result<bool, DataLayerError> {
        match scope.mode {
            RequestCaptureScopeMode::All => Ok(true),
            RequestCaptureScopeMode::IncludeGroups => {
                if scope.group_ids.is_empty() {
                    return Ok(false);
                }
                let Some(user_id) = user_id.map(str::trim).filter(|value| !value.is_empty()) else {
                    return Ok(false);
                };
                let groups = GatewayDataState::list_user_groups_for_user(self, user_id).await?;
                Ok(groups
                    .iter()
                    .any(|group| scope.group_ids.iter().any(|id| id == &group.id)))
            }
            RequestCaptureScopeMode::ExcludeGroups => {
                if scope.group_ids.is_empty() {
                    return Ok(true);
                }
                let Some(user_id) = user_id.map(str::trim).filter(|value| !value.is_empty()) else {
                    return Ok(true);
                };
                let groups = GatewayDataState::list_user_groups_for_user(self, user_id).await?;
                Ok(!groups
                    .iter()
                    .any(|group| scope.group_ids.iter().any(|id| id == &group.id)))
            }
        }
    }
}

#[async_trait]
impl aether_usage_runtime::ManualProxyNodeCounter for GatewayDataState {
    async fn increment_manual_proxy_node_requests(
        &self,
        node_id: &str,
        total_delta: i64,
        failed_delta: i64,
        latency_ms: Option<i64>,
    ) -> Result<(), DataLayerError> {
        if let Some(repository) = &self.usage_writer {
            let enqueued = repository
                .enqueue_proxy_node_counter_delta(ProxyNodeCounterDelta {
                    node_id: node_id.to_string(),
                    total_requests_delta: total_delta,
                    failed_requests_delta: failed_delta,
                    dns_failures_delta: 0,
                    stream_errors_delta: 0,
                })
                .await?;
            if enqueued {
                return Ok(());
            }
        }

        match &self.proxy_node_writer {
            Some(repository) => {
                repository
                    .increment_manual_node_requests(node_id, total_delta, failed_delta, latency_ms)
                    .await
            }
            None => Ok(()),
        }
    }
}

#[async_trait]
impl UsageRecordWriter for GatewayDataState {
    async fn upsert_usage_record(
        &self,
        record: UpsertUsageRecord,
    ) -> Result<Option<StoredRequestUsageAudit>, DataLayerError> {
        GatewayDataState::upsert_usage(self, record).await
    }
}

#[cfg(test)]
mod tests {
    use aether_billing::enrich_usage_event_with_billing;
    use aether_data::repository::users::{
        InMemoryUserReadRepository, StoredUserAuthRecord, UpsertUserGroupRecord, UserReadRepository,
    };
    use aether_usage_runtime::UsageRuntimeAccess;
    use chrono::Utc;
    use serde_json::{json, Value};
    use std::sync::Arc;

    use super::GatewayDataState;
    use crate::usage::{UsageEvent, UsageEventData, UsageEventType, UsageRequestRecordLevel};

    #[tokio::test]
    async fn enriches_completed_usage_event_with_billing_snapshot() {
        let state = GatewayDataState::with_billing_reader_for_tests(
            std::sync::Arc::new(
                aether_data::repository::billing::InMemoryBillingReadRepository::seed(vec![
                    aether_data_contracts::repository::billing::StoredBillingModelContext::new(
                        "provider-1".to_string(),
                        Some("pay_as_you_go".to_string()),
                        Some("key-1".to_string()),
                        Some(serde_json::json!({"openai:chat": 0.5})),
                        Some(60),
                        "global-model-1".to_string(),
                        "gpt-5".to_string(),
                        None,
                        Some(0.02),
                        Some(serde_json::json!({"tiers":[{"up_to":null,"input_price_per_1m":3.0,"output_price_per_1m":15.0,"cache_creation_price_per_1m":3.75,"cache_read_price_per_1m":0.30}]})),
                        Some("model-1".to_string()),
                        Some("gpt-5-upstream".to_string()),
                        None,
                        None,
                        None,
                    )
                    .expect("billing context should build"),
                ]),
            ),
        );
        let mut event = UsageEvent::new(
            UsageEventType::Completed,
            "req-billing-1",
            UsageEventData {
                provider_name: "OpenAI".to_string(),
                model: "gpt-5".to_string(),
                provider_id: Some("provider-1".to_string()),
                provider_api_key_id: Some("key-1".to_string()),
                request_type: Some("chat".to_string()),
                api_format: Some("openai:chat".to_string()),
                endpoint_api_format: Some("openai:chat".to_string()),
                input_tokens: Some(1_000),
                output_tokens: Some(500),
                cache_read_input_tokens: Some(100),
                status_code: Some(200),
                ..UsageEventData::default()
            },
        );

        enrich_usage_event_with_billing(&state, &mut event)
            .await
            .expect("billing should succeed");

        assert!(event.data.total_cost_usd.unwrap_or_default() > 0.0);
        assert!(event.data.actual_total_cost_usd.unwrap_or_default() > 0.0);
        assert_eq!(
            event
                .data
                .request_metadata
                .as_ref()
                .and_then(|value| value.get("billing_snapshot"))
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str),
            Some("complete")
        );
    }

    #[tokio::test]
    async fn usage_runtime_access_reads_base_request_record_level_as_basic() {
        let state = GatewayDataState::disabled().with_system_config_values_for_tests([(
            "request_record_level".to_string(),
            json!("base"),
        )]);

        let level = UsageRuntimeAccess::request_record_level(&state)
            .await
            .expect("request record level should read");

        assert_eq!(level, UsageRequestRecordLevel::Basic);
    }

    #[tokio::test]
    async fn usage_runtime_access_falls_back_to_legacy_request_log_level_alias() {
        let state = GatewayDataState::disabled().with_system_config_values_for_tests([(
            "request_log_level".to_string(),
            json!("headers"),
        )]);

        let level = UsageRuntimeAccess::request_record_level(&state)
            .await
            .expect("legacy request log level should read");

        assert_eq!(level, UsageRequestRecordLevel::Basic);
    }

    #[tokio::test]
    async fn usage_runtime_access_defaults_missing_request_record_level_to_full() {
        let state = GatewayDataState::disabled();

        let level = UsageRuntimeAccess::request_record_level(&state)
            .await
            .expect("missing request record level should fall back");

        assert_eq!(level, UsageRequestRecordLevel::Full);
    }

    #[tokio::test]
    async fn usage_runtime_access_reads_body_capture_limits_from_system_config() {
        let state = GatewayDataState::disabled().with_system_config_values_for_tests([
            ("max_request_body_size".to_string(), json!(1234)),
            ("max_response_body_size".to_string(), json!(5678)),
        ]);

        let policy = UsageRuntimeAccess::body_capture_policy(&state)
            .await
            .expect("body capture policy should read");

        assert_eq!(policy.record_level, UsageRequestRecordLevel::Full);
        assert_eq!(policy.max_request_body_bytes, Some(1234));
        assert_eq!(policy.max_response_body_bytes, Some(5678));
    }

    #[tokio::test]
    async fn usage_runtime_access_treats_zero_body_capture_limit_as_unbounded() {
        let state = GatewayDataState::disabled().with_system_config_values_for_tests([
            ("max_request_body_size".to_string(), json!(0)),
            ("max_response_body_size".to_string(), json!(0)),
        ]);

        let policy = UsageRuntimeAccess::body_capture_policy(&state)
            .await
            .expect("body capture policy should read");

        assert_eq!(policy.max_request_body_bytes, None);
        assert_eq!(policy.max_response_body_bytes, None);
    }

    #[tokio::test]
    async fn usage_runtime_access_reads_prompt_capture_policy_from_system_config() {
        let state = GatewayDataState::disabled().with_system_config_values_for_tests([(
            "request_capture_policy".to_string(),
            json!({
                "request_record_level": "basic",
                "max_request_body_bytes": 2048,
                "prompt_capture": {
                    "enabled": true,
                    "include_system": true,
                    "include_developer": false,
                    "include_user": true,
                    "preview_chars": 120,
                    "max_items": 8
                }
            }),
        )]);

        let policy = UsageRuntimeAccess::body_capture_policy_for_user(&state, Some("user-1"))
            .await
            .expect("request capture policy should read");

        assert_eq!(policy.record_level, UsageRequestRecordLevel::Basic);
        assert_eq!(policy.max_request_body_bytes, Some(2048));
        assert!(policy.prompt_capture.enabled);
        assert!(!policy.prompt_capture.include_developer);
        assert_eq!(policy.prompt_capture.preview_chars, 120);
        assert_eq!(policy.prompt_capture.max_items, 8);
    }

    #[tokio::test]
    async fn usage_runtime_access_applies_request_capture_policy_to_included_user_group() {
        let user_repository = Arc::new(InMemoryUserReadRepository::seed_auth_users(vec![
            sample_auth_user("user-in"),
            sample_auth_user("user-out"),
        ]));
        let group = user_repository
            .create_user_group(sample_user_group("Prompt Audit"))
            .await
            .expect("group create should succeed")
            .expect("group should exist");
        user_repository
            .add_user_to_group(&group.id, "user-in")
            .await
            .expect("membership should create");

        let state = GatewayDataState::disabled()
            .with_user_reader(user_repository)
            .with_system_config_values_for_tests([(
                "request_capture_policy".to_string(),
                json!({
                    "request_record_level": "full",
                    "scope": {"mode": "include_groups", "group_ids": [group.id]},
                    "prompt_capture": {"enabled": true}
                }),
            )]);

        let included = UsageRuntimeAccess::body_capture_policy_for_user(&state, Some("user-in"))
            .await
            .expect("included policy should read");
        let excluded = UsageRuntimeAccess::body_capture_policy_for_user(&state, Some("user-out"))
            .await
            .expect("excluded policy should read");

        assert_eq!(included.record_level, UsageRequestRecordLevel::Full);
        assert!(included.prompt_capture.enabled);
        assert_eq!(excluded.record_level, UsageRequestRecordLevel::Basic);
        assert!(!excluded.prompt_capture.enabled);
    }

    fn sample_auth_user(id: &str) -> StoredUserAuthRecord {
        StoredUserAuthRecord {
            id: id.to_string(),
            email: Some(format!("{id}@example.test")),
            email_verified: true,
            username: id.to_string(),
            password_hash: None,
            role: "user".to_string(),
            auth_source: "local".to_string(),
            allowed_providers: None,
            allowed_providers_mode: "unrestricted".to_string(),
            allowed_api_formats: None,
            allowed_api_formats_mode: "unrestricted".to_string(),
            allowed_models: None,
            allowed_models_mode: "unrestricted".to_string(),
            is_active: true,
            is_deleted: false,
            created_at: Some(Utc::now()),
            last_login_at: None,
        }
    }

    fn sample_user_group(name: &str) -> UpsertUserGroupRecord {
        UpsertUserGroupRecord {
            name: name.to_string(),
            description: None,
            priority: 10,
            allowed_providers: None,
            allowed_providers_mode: "unrestricted".to_string(),
            allowed_api_formats: None,
            allowed_api_formats_mode: "unrestricted".to_string(),
            allowed_models: None,
            allowed_models_mode: "unrestricted".to_string(),
            rate_limit: None,
            rate_limit_mode: "inherit".to_string(),
        }
    }
}
