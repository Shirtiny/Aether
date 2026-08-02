use aether_data_contracts::repository::candidates::{
    RequestCandidateStatus, StoredRequestCandidate,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UsageTerminalSyncState {
    pub(crate) pending: bool,
    pub(crate) response_time_ms: Option<u64>,
}

fn candidate_order(candidate: &StoredRequestCandidate) -> (u32, u32, u64, u64) {
    (
        candidate.candidate_index,
        candidate.retry_index,
        candidate
            .started_at_unix_ms
            .unwrap_or(candidate.created_at_unix_ms),
        candidate
            .finished_at_unix_ms
            .unwrap_or(candidate.created_at_unix_ms),
    )
}

fn terminal_candidate_response_time_ms(
    usage_created_at_unix_ms: u64,
    candidate: &StoredRequestCandidate,
) -> Option<u64> {
    let from_usage_start = candidate
        .finished_at_unix_ms
        .and_then(|finished_at_unix_ms| finished_at_unix_ms.checked_sub(usage_created_at_unix_ms))
        .filter(|elapsed_ms| *elapsed_ms > 0);
    candidate
        .latency_ms
        .filter(|elapsed_ms| *elapsed_ms > 0)
        .or(from_usage_start)
}

/// Resolves an explicit "transport is terminal, usage row is not" signal.
///
/// A terminal candidate alone is not sufficient while a later candidate is still
/// available or active: that can be the normal gap between a failed attempt and a
/// fallback. The signal is emitted only after the latest attempted candidate is
/// terminal and no later candidate can still run.
pub(crate) fn resolve_usage_terminal_sync_state(
    usage_status: &str,
    usage_created_at_unix_ms: u64,
    candidates: &[StoredRequestCandidate],
) -> UsageTerminalSyncState {
    if !matches!(
        usage_status.trim().to_ascii_lowercase().as_str(),
        "pending" | "streaming"
    ) {
        return UsageTerminalSyncState::default();
    }

    // Fail closed when any attempt is still live. Candidate index ordering is
    // normally sequential, but concurrent/hedged attempts must never let one
    // terminal candidate freeze the timer of another live stream.
    if candidates.iter().any(|candidate| {
        matches!(
            candidate.status,
            RequestCandidateStatus::Pending | RequestCandidateStatus::Streaming
        )
    }) {
        return UsageTerminalSyncState::default();
    }

    let Some(latest_attempted) = candidates
        .iter()
        .filter(|candidate| candidate.status.is_attempted(candidate.started_at_unix_ms))
        .max_by_key(|candidate| candidate_order(candidate))
    else {
        return UsageTerminalSyncState::default();
    };

    if !matches!(
        latest_attempted.status,
        RequestCandidateStatus::Success
            | RequestCandidateStatus::Failed
            | RequestCandidateStatus::Cancelled
    ) {
        return UsageTerminalSyncState::default();
    }

    let latest_order = candidate_order(latest_attempted);
    let later_candidate_can_run = candidates.iter().any(|candidate| {
        candidate_order(candidate) > latest_order
            && matches!(
                candidate.status,
                RequestCandidateStatus::Available
                    | RequestCandidateStatus::Pending
                    | RequestCandidateStatus::Streaming
            )
    });
    if later_candidate_can_run {
        return UsageTerminalSyncState::default();
    }

    UsageTerminalSyncState {
        pending: true,
        response_time_ms: terminal_candidate_response_time_ms(
            usage_created_at_unix_ms,
            latest_attempted,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        candidate_index: u32,
        status: RequestCandidateStatus,
        started_at_unix_ms: Option<u64>,
        finished_at_unix_ms: Option<u64>,
        latency_ms: Option<u64>,
    ) -> StoredRequestCandidate {
        StoredRequestCandidate {
            id: format!("candidate-{candidate_index}"),
            request_id: "request-1".to_string(),
            user_id: None,
            api_key_id: None,
            username: None,
            api_key_name: None,
            candidate_index,
            retry_index: 0,
            provider_id: None,
            endpoint_id: None,
            key_id: None,
            status,
            skip_reason: None,
            is_cached: false,
            status_code: None,
            error_type: None,
            error_message: None,
            latency_ms,
            concurrent_requests: None,
            extra_data: None,
            required_capabilities: None,
            created_at_unix_ms: 1_000_000,
            started_at_unix_ms,
            finished_at_unix_ms,
        }
    }

    #[test]
    fn detects_terminal_candidate_while_usage_is_still_active() {
        let state = resolve_usage_terminal_sync_state(
            "streaming",
            1_000_000,
            &[candidate(
                0,
                RequestCandidateStatus::Success,
                Some(1_000_100),
                Some(1_005_037),
                Some(5_037),
            )],
        );

        assert_eq!(
            state,
            UsageTerminalSyncState {
                pending: true,
                response_time_ms: Some(5_037),
            }
        );
    }

    #[test]
    fn does_not_misclassify_a_legitimate_long_stream() {
        let state = resolve_usage_terminal_sync_state(
            "streaming",
            1_000_000,
            &[candidate(
                0,
                RequestCandidateStatus::Streaming,
                Some(1_000_100),
                None,
                Some(5_000),
            )],
        );

        assert_eq!(state, UsageTerminalSyncState::default());
    }

    #[test]
    fn keeps_candidate_latency_when_usage_row_was_created_late() {
        let state = resolve_usage_terminal_sync_state(
            "streaming",
            1_003_000,
            &[candidate(
                0,
                RequestCandidateStatus::Success,
                Some(1_000_100),
                Some(1_005_037),
                Some(4_937),
            )],
        );

        assert_eq!(state.response_time_ms, Some(4_937));
    }

    #[test]
    fn waits_when_a_later_fallback_can_still_run() {
        let state = resolve_usage_terminal_sync_state(
            "streaming",
            1_000_000,
            &[
                candidate(
                    0,
                    RequestCandidateStatus::Failed,
                    Some(1_000_100),
                    Some(1_001_000),
                    Some(900),
                ),
                candidate(1, RequestCandidateStatus::Available, None, None, None),
            ],
        );

        assert_eq!(state, UsageTerminalSyncState::default());
    }

    #[test]
    fn waits_when_any_concurrent_attempt_is_still_live() {
        let state = resolve_usage_terminal_sync_state(
            "streaming",
            1_000_000,
            &[
                candidate(
                    1,
                    RequestCandidateStatus::Success,
                    Some(1_000_100),
                    Some(1_005_000),
                    Some(4_900),
                ),
                candidate(
                    0,
                    RequestCandidateStatus::Streaming,
                    Some(1_000_200),
                    None,
                    None,
                ),
            ],
        );

        assert_eq!(state, UsageTerminalSyncState::default());
    }

    #[test]
    fn ignores_candidate_terminal_state_after_usage_finalizes() {
        let state = resolve_usage_terminal_sync_state(
            "completed",
            1_000_000,
            &[candidate(
                0,
                RequestCandidateStatus::Success,
                Some(1_000_100),
                Some(1_005_000),
                Some(4_900),
            )],
        );

        assert_eq!(state, UsageTerminalSyncState::default());
    }
}
