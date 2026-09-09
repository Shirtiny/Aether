//! Small, body-free per-plan retry diagnostics. Updated only at retry/window
//! boundaries, then attached to existing terminal usage/candidate persistence.
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Default)]
pub(super) struct RetryAudit(Arc<Mutex<AuditState>>);
#[derive(Default)]
struct AuditState {
    retry_count: u64,
    failures: Vec<Value>,
    stop_reason: Option<&'static str>,
    stop_event_type: Option<String>,
}
impl RetryAudit {
    pub(super) fn rejection(&self, status: u16, reason: &'static str, wait_ms: Option<u64>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.failures.len() < 3 {
            let attempt = state.retry_count + 1;
            state.failures.push(
                json!({"attempt":attempt,"status_code":status,"reason":reason,
                "planned_wait_ms":wait_ms,"wait_ms":null,"retry_started":false}),
            );
        }
    }
    pub(super) fn waited(&self, started: Instant) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last) = state.failures.last_mut() {
            last["wait_ms"] = json!(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        }
    }
    pub(super) fn dispatched(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.retry_count += 1;
        if let Some(last) = state.failures.last_mut() {
            last["retry_started"] = json!(true);
        }
    }
    pub(super) fn close(&self, reason: &'static str, event_type: Option<&str>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.stop_reason.is_some() {
            return;
        }
        state.stop_reason = Some(reason);
        state.stop_event_type = event_type
            .filter(|s| {
                s.len() <= 96
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            })
            .map(str::to_owned);
    }
    pub(super) fn snapshot(&self) -> Value {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        json!({"version":1,"scope":"aether","kind":"same_plan_overload",
            "retry_count":state.retry_count,"failures":state.failures,
            "stop_reason":state.stop_reason,"stop_event_type":state.stop_event_type})
    }
    pub(super) fn context(&self, context: Option<Value>) -> Option<Value> {
        let mut context = match context {
            Some(Value::Object(context)) => context,
            _ => serde_json::Map::new(),
        };
        context.insert("internal_retry".into(), self.snapshot());
        Some(Value::Object(context))
    }
}

pub(super) fn with_retry_audit(
    context: Option<Value>,
    audit: Option<&RetryAudit>,
) -> Option<Value> {
    match audit {
        Some(audit) => audit.context(context),
        None => context,
    }
}
