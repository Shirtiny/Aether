use std::collections::HashSet;

use chrono::Utc;

use crate::data::GatewayDataState;
use aether_data_contracts::DataLayerError;

use super::{pending_cleanup_batch_size, pending_cleanup_timeout_minutes};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PendingCleanupSummary {
    pub(crate) failed: usize,
    pub(crate) recovered: usize,
    pub(crate) reconciliation_required: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StalePendingUsageRow {
    pub(super) id: String,
    pub(super) request_id: String,
    pub(super) status: String,
    pub(super) billing_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FailedPendingUsageRow {
    pub(super) id: String,
    pub(super) error_message: String,
    pub(super) should_void_billing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct PendingCleanupBatchPlan {
    pub(super) recovered_usage_ids: Vec<String>,
    pub(super) recovered_request_ids: Vec<String>,
    pub(super) failed_usage_rows: Vec<FailedPendingUsageRow>,
    pub(super) failed_request_ids: Vec<String>,
}

pub(crate) async fn cleanup_stale_pending_requests_once(
    data: &GatewayDataState,
) -> Result<PendingCleanupSummary, DataLayerError> {
    if !data.has_usage_writer() {
        return Ok(PendingCleanupSummary::default());
    }

    let timeout_minutes = pending_cleanup_timeout_minutes(data).await?;
    let batch_size = pending_cleanup_batch_size(data).await?;
    let now_unix_secs = Utc::now().timestamp().max(0) as u64;
    let cutoff_unix_secs = now_unix_secs.saturating_sub(timeout_minutes.saturating_mul(60));
    let summary = data
        .cleanup_stale_pending_requests(
            cutoff_unix_secs,
            now_unix_secs,
            timeout_minutes,
            batch_size,
        )
        .await?;

    Ok(PendingCleanupSummary {
        failed: summary.failed,
        recovered: summary.recovered,
        reconciliation_required: summary.reconciliation_required,
    })
}

pub(super) fn plan_pending_cleanup_batch(
    stale_rows: Vec<StalePendingUsageRow>,
    completed_request_ids: &HashSet<String>,
    timeout_minutes: u64,
) -> PendingCleanupBatchPlan {
    let mut plan = PendingCleanupBatchPlan::default();
    for row in stale_rows {
        if completed_request_ids.contains(&row.request_id) {
            plan.recovered_usage_ids.push(row.id);
            plan.recovered_request_ids.push(row.request_id);
            continue;
        }

        plan.failed_request_ids.push(row.request_id);
        plan.failed_usage_rows.push(FailedPendingUsageRow {
            id: row.id,
            error_message: format!(
                "请求超时: 状态 '{}' 超过 {} 分钟未完成",
                row.status, timeout_minutes
            ),
            should_void_billing: row.billing_status == "pending",
        });
    }
    plan
}
