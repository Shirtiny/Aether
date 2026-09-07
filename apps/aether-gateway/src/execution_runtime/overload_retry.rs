//! Bounded retries of an explicitly rejected request, never account failover.
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use tracing::info;

use crate::orchestration::{is_session_preserving_overload, LocalFailoverInput};

#[derive(Default)]
pub(super) struct OverloadRetry {
    attempts: usize,
}

impl OverloadRetry {
    pub(super) async fn wait(
        &mut self,
        request_id: &str,
        status: u16,
        text: Option<&str>,
        headers: &BTreeMap<String, String>,
    ) -> bool {
        let Some(delay) = self.delay(request_id, status, text, headers) else {
            return false;
        };
        self.attempts += 1;
        info!(
            event_name = "upstream_overload_retry",
            request_id,
            retry_attempt = self.attempts,
            delay_ms = delay.as_millis() as u64,
            "retrying capacity rejection on the same prepared upstream plan"
        );
        tokio::time::sleep(delay).await;
        true
    }

    fn delay(
        &self,
        request_id: &str,
        status: u16,
        text: Option<&str>,
        headers: &BTreeMap<String, String>,
    ) -> Option<Duration> {
        if !is_session_preserving_overload(LocalFailoverInput::new(status, text)) {
            return None;
        }
        let Some(base_ms) = [250, 750].get(self.attempts) else {
            info!(
                event_name = "upstream_overload_retry_exhausted",
                request_id, "capacity retry budget exhausted"
            );
            return None;
        };
        let retry_after = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
            .map(|(_, value)| value.trim().parse::<u64>())
            .transpose()
            .ok()?;
        if retry_after.is_some_and(|seconds| seconds > 5) {
            return None;
        }
        let mut hash = DefaultHasher::new();
        request_id.hash(&mut hash);
        self.attempts.hash(&mut hash);
        let millis = (base_ms + hash.finish() % 101).max(retry_after.unwrap_or(0) * 1000);
        Some(Duration::from_millis(millis))
    }

    pub(super) fn recovered(&self, request_id: &str) {
        if self.attempts > 0 {
            info!(
                event_name = "upstream_overload_retry_recovered",
                request_id,
                retries = self.attempts,
                "upstream accepted the retried request"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MESSAGE: &str = "Our servers are currently overloaded. Please try again later.";

    #[tokio::test(start_paused = true)]
    async fn overload_retry_is_bounded_and_only_matches_capacity_rejections() {
        let mut retry = OverloadRetry::default();
        let headers = BTreeMap::new();
        assert!(!retry.wait("req", 200, Some(MESSAGE), &headers).await);
        assert!(
            !retry
                .wait("req", 503, Some("another error"), &headers)
                .await
        );
        for (lower, upper) in [(250, 350), (750, 850)] {
            let delay = retry.delay("req", 503, Some(MESSAGE), &headers).unwrap();
            assert!((lower..=upper).contains(&delay.as_millis()));
            assert!(retry.wait("req", 503, Some(MESSAGE), &headers).await);
        }
        assert!(!retry.wait("req", 503, Some(MESSAGE), &headers).await);
    }

    #[test]
    fn overload_retry_respects_retry_after_without_unbounded_waits() {
        let retry = OverloadRetry::default();
        for value in ["6", "3600", "Wed, 21 Oct 2030 07:28:00 GMT"] {
            assert!(retry
                .delay(
                    "req",
                    503,
                    Some(MESSAGE),
                    &BTreeMap::from([("Retry-After".into(), value.into())])
                )
                .is_none());
        }
        assert_eq!(
            retry.delay(
                "req",
                503,
                Some(MESSAGE),
                &BTreeMap::from([("retry-after".into(), "2".into())])
            ),
            Some(Duration::from_secs(2))
        );
    }
}
