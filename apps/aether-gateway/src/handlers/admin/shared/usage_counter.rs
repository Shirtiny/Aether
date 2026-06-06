use aether_data_contracts::repository::usage::UsageCounterHealthSnapshot;
use serde_json::{json, Value};

pub(crate) fn build_admin_usage_counter_health_payload(
    snapshot: &UsageCounterHealthSnapshot,
    now_unix_secs: u64,
) -> Value {
    let oldest_pending_age_secs = snapshot
        .oldest_pending_created_at_unix_secs
        .map(|created_at| now_unix_secs.saturating_sub(created_at));
    let status = match (snapshot.pending_rows, oldest_pending_age_secs) {
        (0, _) => "idle",
        (_, Some(age)) if age >= 60 => "backlogged",
        _ => "catching_up",
    };

    json!({
        "status": status,
        "outbox_pending_rows": snapshot.pending_rows,
        "outbox_processed_rows": snapshot.processed_rows,
        "oldest_pending_created_at_unix_secs": snapshot.oldest_pending_created_at_unix_secs,
        "oldest_pending_age_secs": oldest_pending_age_secs,
        "latest_processed_at_unix_secs": snapshot.latest_processed_at_unix_secs,
        "pending_by_kind": snapshot.pending_by_kind,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use aether_data_contracts::repository::usage::UsageCounterHealthSnapshot;
    use serde_json::json;

    use super::build_admin_usage_counter_health_payload;

    #[test]
    fn usage_counter_health_payload_reports_idle_without_pending_rows() {
        let snapshot = UsageCounterHealthSnapshot {
            processed_rows: 42,
            latest_processed_at_unix_secs: Some(1_000),
            ..UsageCounterHealthSnapshot::default()
        };

        let payload = build_admin_usage_counter_health_payload(&snapshot, 1_100);

        assert_eq!(payload["status"], json!("idle"));
        assert_eq!(payload["outbox_pending_rows"], json!(0));
        assert_eq!(payload["outbox_processed_rows"], json!(42));
        assert_eq!(payload["oldest_pending_age_secs"], json!(null));
        assert_eq!(payload["latest_processed_at_unix_secs"], json!(1_000));
    }

    #[test]
    fn usage_counter_health_payload_reports_catching_up_for_fresh_backlog() {
        let mut pending_by_kind = BTreeMap::new();
        pending_by_kind.insert("api_key".to_string(), 3);
        let snapshot = UsageCounterHealthSnapshot {
            pending_rows: 3,
            oldest_pending_created_at_unix_secs: Some(1_050),
            pending_by_kind,
            ..UsageCounterHealthSnapshot::default()
        };

        let payload = build_admin_usage_counter_health_payload(&snapshot, 1_100);

        assert_eq!(payload["status"], json!("catching_up"));
        assert_eq!(payload["oldest_pending_age_secs"], json!(50));
        assert_eq!(payload["pending_by_kind"]["api_key"], json!(3));
    }

    #[test]
    fn usage_counter_health_payload_reports_backlogged_for_old_backlog() {
        let snapshot = UsageCounterHealthSnapshot {
            pending_rows: 1,
            oldest_pending_created_at_unix_secs: Some(1_000),
            ..UsageCounterHealthSnapshot::default()
        };

        let payload = build_admin_usage_counter_health_payload(&snapshot, 1_060);

        assert_eq!(payload["status"], json!("backlogged"));
        assert_eq!(payload["oldest_pending_age_secs"], json!(60));
    }
}
