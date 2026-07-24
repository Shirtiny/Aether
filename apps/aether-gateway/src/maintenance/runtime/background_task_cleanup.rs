use aether_data_contracts::DataLayerError;
use serde_json::json;
use std::time::Instant;
use tracing::info;

use crate::data::GatewayDataState;

use super::{
    now_unix_secs, record_completed_cleanup_run, record_failed_cleanup_run, system_config_bool,
    system_config_u64, system_config_usize, BACKGROUND_TASK_BOOT_RUN_STALE_AFTER_SECS,
};

const SECS_PER_DAY: u64 = 24 * 60 * 60;
const BACKGROUND_TASK_RUN_RETENTION_DAYS_DEFAULT: u64 = 30;
const BACKGROUND_TASK_RUN_RETENTION_DAYS_MIN: u64 = 1;
const BACKGROUND_TASK_RUN_RETENTION_DAYS_MAX: u64 = 365;
const BACKGROUND_TASK_CLEANUP_BATCH_SIZE_DEFAULT: usize = 1_000;
const BACKGROUND_TASK_CLEANUP_BATCH_SIZE_MAX: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct BackgroundTaskCleanupSummary {
    pub(crate) deleted_runs: usize,
    pub(crate) deleted_boot_runs: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BackgroundTaskCleanupSettings {
    pub retention_days: u64,
    pub batch_size: usize,
}

pub(super) async fn background_task_cleanup_settings(
    data: &GatewayDataState,
) -> Result<BackgroundTaskCleanupSettings, DataLayerError> {
    let retention_days = system_config_u64(
        data,
        "background_task_runs_retention_days",
        BACKGROUND_TASK_RUN_RETENTION_DAYS_DEFAULT,
    )
    .await?
    .clamp(
        BACKGROUND_TASK_RUN_RETENTION_DAYS_MIN,
        BACKGROUND_TASK_RUN_RETENTION_DAYS_MAX,
    );
    let cleanup_batch_size =
        system_config_usize(data, "background_task_runs_cleanup_batch_size", 0).await?;
    let fallback_batch_size = system_config_usize(
        data,
        "cleanup_batch_size",
        BACKGROUND_TASK_CLEANUP_BATCH_SIZE_DEFAULT,
    )
    .await?;
    let batch_size = (if cleanup_batch_size > 0 {
        cleanup_batch_size
    } else {
        fallback_batch_size
    })
    .clamp(1, BACKGROUND_TASK_CLEANUP_BATCH_SIZE_MAX);

    Ok(BackgroundTaskCleanupSettings {
        retention_days,
        batch_size,
    })
}

pub(crate) async fn cleanup_background_task_runs_once(
    data: &GatewayDataState,
) -> Result<BackgroundTaskCleanupSummary, DataLayerError> {
    cleanup_background_task_runs_at(data, now_unix_secs()).await
}

pub(crate) async fn cleanup_background_task_runs_at(
    data: &GatewayDataState,
    now_unix_secs: u64,
) -> Result<BackgroundTaskCleanupSummary, DataLayerError> {
    if !system_config_bool(data, "enable_auto_cleanup", true).await? {
        return Ok(BackgroundTaskCleanupSummary::default());
    }

    // Boot rows written by an earlier process stay `running` forever because no
    // one ever closes them, and they carry no useful information. Drop those
    // outright so the running count and the table both stop carrying ghosts of
    // dead processes.
    let deleted_boot_runs = data
        .delete_stale_worker_boot_runs(
            now_unix_secs.saturating_sub(BACKGROUND_TASK_BOOT_RUN_STALE_AFTER_SECS),
        )
        .await?;

    let settings = background_task_cleanup_settings(data).await?;
    let retain_from_unix_secs =
        now_unix_secs.saturating_sub(settings.retention_days.saturating_mul(SECS_PER_DAY));

    let mut deleted_runs = 0_usize;
    loop {
        let deleted = data
            .delete_background_task_runs_updated_before(retain_from_unix_secs, settings.batch_size)
            .await?;
        deleted_runs = deleted_runs.saturating_add(deleted);
        if deleted < settings.batch_size {
            break;
        }
        tokio::task::yield_now().await;
    }

    Ok(BackgroundTaskCleanupSummary {
        deleted_runs,
        deleted_boot_runs,
    })
}

pub(super) async fn run_background_task_cleanup_once(
    data: &GatewayDataState,
) -> Result<(), DataLayerError> {
    let started_at_unix_secs = now_unix_secs();
    let started_at = Instant::now();
    let summary = match cleanup_background_task_runs_once(data).await {
        Ok(summary) => summary,
        Err(err) => {
            record_failed_cleanup_run(
                data,
                "background_task_cleanup",
                "auto",
                started_at_unix_secs,
                started_at,
                &err,
            )
            .await;
            return Err(err);
        }
    };
    record_completed_cleanup_run(
        data,
        "background_task_cleanup",
        "auto",
        started_at_unix_secs,
        started_at,
        json!({
            "background_task_runs_deleted": summary.deleted_runs,
            "stale_worker_boot_runs_deleted": summary.deleted_boot_runs,
        }),
        format!(
            "后台任务记录自动清理完成，删除 {} 行过期记录，清除 {} 条残留启动记录",
            summary.deleted_runs, summary.deleted_boot_runs
        ),
    )
    .await;
    if summary.deleted_runs > 0 || summary.deleted_boot_runs > 0 {
        info!(
            deleted = summary.deleted_runs,
            deleted_boot_runs = summary.deleted_boot_runs,
            "gateway cleaned up background task runs"
        );
    }
    Ok(())
}
