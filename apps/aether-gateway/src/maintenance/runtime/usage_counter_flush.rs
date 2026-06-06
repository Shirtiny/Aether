use crate::data::GatewayDataState;
use aether_data::DataLayerError;
use aether_data_contracts::repository::usage::UsageCounterFlushSummary;

pub(crate) async fn run_usage_counter_flush_once(
    data: &GatewayDataState,
    batch_size: usize,
) -> Result<UsageCounterFlushSummary, DataLayerError> {
    data.flush_usage_counter_deltas(batch_size).await
}

pub(crate) async fn cleanup_processed_usage_counter_deltas_once(
    data: &GatewayDataState,
    retention_secs: u64,
    batch_size: usize,
) -> Result<usize, DataLayerError> {
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    let cutoff = now.saturating_sub(retention_secs);
    data.cleanup_processed_usage_counter_deltas(cutoff, batch_size)
        .await
}
