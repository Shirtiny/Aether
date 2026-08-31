use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use futures_util::FutureExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::orchestration::apply_local_codex_quota_headers_effect;
use crate::AppState;

const DEFAULT_CAPACITY: usize = 16_384;
const MIN_CAPACITY: usize = 10_000;
const MAX_CAPACITY: usize = 65_536;
const DEFAULT_WORKER_CONCURRENCY: usize = 32;
const MIN_WORKER_CONCURRENCY: usize = 1;
const MAX_WORKER_CONCURRENCY: usize = 128;
const CAPACITY_ENV: &str = "AETHER_CODEX_WS_USAGE_REPORT_QUEUE_CAPACITY";
const WORKER_CONCURRENCY_ENV: &str = "AETHER_CODEX_WS_USAGE_REPORT_WORKERS";
const DEFAULT_SETTLEMENT_CAPACITY: usize = 16_384;
const MIN_SETTLEMENT_CAPACITY: usize = 10_000;
const MAX_SETTLEMENT_CAPACITY: usize = 65_536;
const DEFAULT_SETTLEMENT_WORKER_CONCURRENCY: usize = 64;
const MIN_SETTLEMENT_WORKER_CONCURRENCY: usize = 1;
const MAX_SETTLEMENT_WORKER_CONCURRENCY: usize = 128;
const DEFAULT_SETTLEMENT_TIMEOUT_MS: u64 = 2_000;
const MIN_SETTLEMENT_TIMEOUT_MS: u64 = 100;
const MAX_SETTLEMENT_TIMEOUT_MS: u64 = 10_000;
const SETTLEMENT_CAPACITY_ENV: &str = "AETHER_CODEX_WS_SETTLEMENT_QUEUE_CAPACITY";
const SETTLEMENT_WORKER_CONCURRENCY_ENV: &str = "AETHER_CODEX_WS_SETTLEMENT_WORKERS";
const SETTLEMENT_TIMEOUT_ENV: &str = "AETHER_CODEX_WS_SETTLEMENT_TIMEOUT_MS";
const DEFAULT_SLOW_SETTLEMENT_CAPACITY: usize = 4_096;
const MIN_SLOW_SETTLEMENT_CAPACITY: usize = 128;
const MAX_SLOW_SETTLEMENT_CAPACITY: usize = 16_384;
const DEFAULT_SLOW_SETTLEMENT_WORKER_CONCURRENCY: usize = 8;
const MIN_SLOW_SETTLEMENT_WORKER_CONCURRENCY: usize = 1;
const MAX_SLOW_SETTLEMENT_WORKER_CONCURRENCY: usize = 32;
const DEFAULT_SLOW_SETTLEMENT_TIMEOUT_MS: u64 = 10_000;
const MIN_SLOW_SETTLEMENT_TIMEOUT_MS: u64 = 500;
const MAX_SLOW_SETTLEMENT_TIMEOUT_MS: u64 = 30_000;
const SLOW_SETTLEMENT_CAPACITY_ENV: &str = "AETHER_CODEX_WS_SLOW_SETTLEMENT_QUEUE_CAPACITY";
const SLOW_SETTLEMENT_WORKER_CONCURRENCY_ENV: &str = "AETHER_CODEX_WS_SLOW_SETTLEMENT_WORKERS";
const SLOW_SETTLEMENT_TIMEOUT_ENV: &str = "AETHER_CODEX_WS_SLOW_SETTLEMENT_TIMEOUT_MS";
// A cancelled provider-reached turn may not have a provider terminal usage
// event. In that case we derive a bounded input estimate in this cold
// settlement path. The relay loop deliberately does not tokenize or retain
// response frames.
const CANCELLED_INPUT_ESTIMATE_MAX_TOKENS: u64 = 8_000_000;
const CANCELLED_INPUT_ESTIMATE_SOURCE: &str = "gateway_cached_input_floor";

type ReporterFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type ReporterHandler<T> = Arc<dyn Fn(T) -> ReporterFuture + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexWsUsageReporterConfig {
    capacity: usize,
    worker_concurrency: usize,
}

impl CodexWsUsageReporterConfig {
    pub(crate) fn from_env() -> Self {
        Self {
            capacity: bounded_env_usize(CAPACITY_ENV, DEFAULT_CAPACITY, MIN_CAPACITY, MAX_CAPACITY),
            worker_concurrency: bounded_env_usize(
                WORKER_CONCURRENCY_ENV,
                DEFAULT_WORKER_CONCURRENCY,
                MIN_WORKER_CONCURRENCY,
                MAX_WORKER_CONCURRENCY,
            ),
        }
    }

    #[cfg(test)]
    fn for_test(capacity: usize, worker_concurrency: usize) -> Self {
        assert!(capacity > 0);
        assert!(worker_concurrency > 0);
        Self {
            capacity,
            worker_concurrency,
        }
    }
}

fn bounded_env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    let raw = std::env::var(name).ok();
    normalize_bounded_usize(raw.as_deref(), default, min, max)
}

fn normalize_bounded_usize(raw: Option<&str>, default: usize, min: usize, max: usize) -> usize {
    raw.and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
        .clamp(min, max)
}

pub(crate) struct CodexWsUsageCommit {
    pub(crate) outcome: aether_usage_runtime::TerminalUsageOutcome,
}

pub(crate) enum CodexWsSettlementCommit {
    Step {
        plan: aether_contracts::ExecutionPlan,
        trace_id: String,
        report_kind: String,
        report_context: Option<serde_json::Value>,
        response_headers: BTreeMap<String, String>,
        terminal_summary: aether_contracts::ExecutionStreamTerminalSummary,
        status_code: u16,
        first_byte_ms: Option<u64>,
        elapsed_ms: Option<u64>,
        cancelled: bool,
        step_settlement: super::CodexWsStepSettlement,
        usage_permit: Option<mpsc::OwnedPermit<CodexWsUsageCommit>>,
    },
    CandidateAbort {
        lifecycle: Arc<super::CodexWsCandidateLifecycle>,
        status: aether_data_contracts::repository::candidates::RequestCandidateStatus,
        status_code: Option<u16>,
        error_type: &'static str,
        error_message: &'static str,
        preserve_sticky_binding: bool,
    },
    HandshakeFailure {
        lifecycle: Arc<super::CodexWsCandidateLifecycle>,
        status_code: u16,
        response_headers: BTreeMap<String, String>,
        error_type: String,
        error_message: String,
        error_body: Option<String>,
        penalize_account: bool,
    },
    UnusedCandidates {
        attempts: Vec<crate::ai_serving::AiStreamAttempt>,
    },
    QuotaHeaders {
        key_id: String,
        headers: BTreeMap<String, String>,
    },
}

struct CodexWsSlowSettlementCommit {
    plan: aether_contracts::ExecutionPlan,
    payload: crate::usage::GatewayStreamReportRequest,
    step_settlement: super::CodexWsStepSettlement,
}

struct SharedBoundedReporter<T> {
    sender: mpsc::Sender<T>,
    receiver: StdMutex<Option<mpsc::Receiver<T>>>,
    capacity: usize,
    worker_concurrency: usize,
    worker_start_count: AtomicUsize,
}

impl<T> fmt::Debug for SharedBoundedReporter<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedBoundedReporter")
            .field("capacity", &self.capacity)
            .field("worker_concurrency", &self.worker_concurrency)
            .field(
                "worker_start_count",
                &self.worker_start_count.load(Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}

impl<T> SharedBoundedReporter<T>
where
    T: Send + 'static,
{
    fn new(config: CodexWsUsageReporterConfig) -> Self {
        let (sender, receiver) = mpsc::channel(config.capacity);
        Self {
            sender,
            receiver: StdMutex::new(Some(receiver)),
            capacity: config.capacity,
            worker_concurrency: config.worker_concurrency,
            worker_start_count: AtomicUsize::new(0),
        }
    }

    fn sender(&self) -> mpsc::Sender<T> {
        self.sender.clone()
    }

    async fn reserve_owned(&self) -> Result<mpsc::OwnedPermit<T>, mpsc::error::SendError<()>> {
        self.sender.clone().reserve_owned().await
    }

    fn try_reserve_owned(
        &self,
    ) -> Result<mpsc::OwnedPermit<T>, mpsc::error::TrySendError<mpsc::Sender<T>>> {
        self.sender.clone().try_reserve_owned()
    }

    fn start_with_handler(
        &self,
        task_name: &'static str,
        handler: ReporterHandler<T>,
    ) -> Result<CodexWsUsageReporterWorker, CodexWsUsageReporterStartError> {
        let receiver = self
            .receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(CodexWsUsageReporterStartError::AlreadyStarted)?;
        self.worker_start_count.fetch_add(1, Ordering::AcqRel);
        let shutdown = CancellationToken::new();
        let task = aether_runtime::task::spawn_named(
            task_name,
            run_worker_pool(receiver, self.worker_concurrency, shutdown.clone(), handler),
        );
        Ok(CodexWsUsageReporterWorker {
            shutdown: vec![shutdown],
            tasks: vec![task],
        })
    }
}

#[derive(Debug)]
pub(crate) struct CodexWsUsageReporter {
    inner: SharedBoundedReporter<CodexWsUsageCommit>,
    settlement: SharedBoundedReporter<CodexWsSettlementCommit>,
    slow_settlement: SharedBoundedReporter<CodexWsSlowSettlementCommit>,
    settlement_timeout: Duration,
    slow_settlement_timeout: Duration,
}

impl CodexWsUsageReporter {
    pub(crate) fn from_env() -> Self {
        let settlement_config = CodexWsUsageReporterConfig {
            capacity: bounded_env_usize(
                SETTLEMENT_CAPACITY_ENV,
                DEFAULT_SETTLEMENT_CAPACITY,
                MIN_SETTLEMENT_CAPACITY,
                MAX_SETTLEMENT_CAPACITY,
            ),
            worker_concurrency: bounded_env_usize(
                SETTLEMENT_WORKER_CONCURRENCY_ENV,
                DEFAULT_SETTLEMENT_WORKER_CONCURRENCY,
                MIN_SETTLEMENT_WORKER_CONCURRENCY,
                MAX_SETTLEMENT_WORKER_CONCURRENCY,
            ),
        };
        let settlement_timeout = Duration::from_millis(bounded_env_usize(
            SETTLEMENT_TIMEOUT_ENV,
            DEFAULT_SETTLEMENT_TIMEOUT_MS as usize,
            MIN_SETTLEMENT_TIMEOUT_MS as usize,
            MAX_SETTLEMENT_TIMEOUT_MS as usize,
        ) as u64);
        let slow_settlement_config = CodexWsUsageReporterConfig {
            capacity: bounded_env_usize(
                SLOW_SETTLEMENT_CAPACITY_ENV,
                DEFAULT_SLOW_SETTLEMENT_CAPACITY,
                MIN_SLOW_SETTLEMENT_CAPACITY,
                MAX_SLOW_SETTLEMENT_CAPACITY,
            ),
            worker_concurrency: bounded_env_usize(
                SLOW_SETTLEMENT_WORKER_CONCURRENCY_ENV,
                DEFAULT_SLOW_SETTLEMENT_WORKER_CONCURRENCY,
                MIN_SLOW_SETTLEMENT_WORKER_CONCURRENCY,
                MAX_SLOW_SETTLEMENT_WORKER_CONCURRENCY,
            ),
        };
        let slow_settlement_timeout = Duration::from_millis(bounded_env_usize(
            SLOW_SETTLEMENT_TIMEOUT_ENV,
            DEFAULT_SLOW_SETTLEMENT_TIMEOUT_MS as usize,
            MIN_SLOW_SETTLEMENT_TIMEOUT_MS as usize,
            MAX_SLOW_SETTLEMENT_TIMEOUT_MS as usize,
        ) as u64);
        Self::new(
            CodexWsUsageReporterConfig::from_env(),
            settlement_config,
            settlement_timeout,
            slow_settlement_config,
            slow_settlement_timeout,
        )
    }

    fn new(
        config: CodexWsUsageReporterConfig,
        settlement_config: CodexWsUsageReporterConfig,
        settlement_timeout: Duration,
        slow_settlement_config: CodexWsUsageReporterConfig,
        slow_settlement_timeout: Duration,
    ) -> Self {
        Self {
            inner: SharedBoundedReporter::new(config),
            settlement: SharedBoundedReporter::new(settlement_config),
            slow_settlement: SharedBoundedReporter::new(slow_settlement_config),
            settlement_timeout,
            slow_settlement_timeout,
        }
    }

    pub(crate) fn sender(&self) -> mpsc::Sender<CodexWsUsageCommit> {
        self.inner.sender()
    }

    pub(crate) fn settlement_sender(&self) -> mpsc::Sender<CodexWsSettlementCommit> {
        self.settlement.sender()
    }

    pub(crate) async fn reserve_owned(
        &self,
    ) -> Result<mpsc::OwnedPermit<CodexWsUsageCommit>, mpsc::error::SendError<()>> {
        self.inner.reserve_owned().await
    }

    pub(crate) fn try_reserve_owned(
        &self,
    ) -> Result<
        mpsc::OwnedPermit<CodexWsUsageCommit>,
        mpsc::error::TrySendError<mpsc::Sender<CodexWsUsageCommit>>,
    > {
        self.inner.try_reserve_owned()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.inner.capacity
    }

    pub(crate) fn worker_concurrency(&self) -> usize {
        self.inner.worker_concurrency
    }

    pub(crate) fn start(
        &self,
        state: AppState,
    ) -> Result<CodexWsUsageReporterWorker, CodexWsUsageReporterStartError> {
        let usage_state = state.clone();
        let handler = Arc::new(move |commit| {
            let state = usage_state.clone();
            Box::pin(process_usage_commit(state, commit)) as ReporterFuture
        });
        let settlement_timeout = self.settlement_timeout;
        let slow_settlement_tx = self.slow_settlement.sender();
        let settlement_state = state.clone();
        let settlement_handler = Arc::new(move |commit| {
            let state = settlement_state.clone();
            let slow_settlement_tx = slow_settlement_tx.clone();
            Box::pin(process_settlement_commit(
                state,
                commit,
                settlement_timeout,
                slow_settlement_tx,
            )) as ReporterFuture
        });
        let slow_settlement_timeout = self.slow_settlement_timeout;
        let slow_state = state;
        let slow_settlement_handler = Arc::new(move |commit| {
            let state = slow_state.clone();
            Box::pin(process_slow_settlement_commit(
                state,
                commit,
                slow_settlement_timeout,
            )) as ReporterFuture
        });
        let usage_worker = self
            .inner
            .start_with_handler("codex-ws-usage-reporter", handler)?;
        let settlement_worker = self
            .settlement
            .start_with_handler("codex-ws-settlement-reporter", settlement_handler)?;
        let slow_settlement_worker = self
            .slow_settlement
            .start_with_handler("codex-ws-slow-settlement-reporter", slow_settlement_handler)?;
        Ok(usage_worker
            .combine(settlement_worker)
            .combine(slow_settlement_worker))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexWsUsageReporterStartError {
    AlreadyStarted,
}

impl fmt::Display for CodexWsUsageReporterStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyStarted => formatter.write_str("Codex WS usage reporter already started"),
        }
    }
}

impl std::error::Error for CodexWsUsageReporterStartError {}

#[derive(Debug)]
pub(crate) struct CodexWsUsageReporterWorker {
    shutdown: Vec<CancellationToken>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl CodexWsUsageReporterWorker {
    fn combine(mut self, mut other: Self) -> Self {
        self.shutdown.append(&mut other.shutdown);
        self.tasks.append(&mut other.tasks);
        self
    }

    pub(crate) async fn shutdown(mut self, timeout: Duration) -> bool {
        for shutdown in &self.shutdown {
            shutdown.cancel();
        }
        let mut tasks = std::mem::take(&mut self.tasks);
        match tokio::time::timeout(timeout, async {
            let mut clean = true;
            for task in &mut tasks {
                if let Err(error) = task.await {
                    warn!(error = ?error, "Codex WS background reporter worker failed during shutdown");
                    clean = false;
                }
            }
            clean
        })
        .await
        {
            Ok(clean) => clean,
            Err(_) => {
                for task in tasks {
                    task.abort();
                }
                false
            }
        }
    }
}

impl Drop for CodexWsUsageReporterWorker {
    fn drop(&mut self) {
        for shutdown in &self.shutdown {
            shutdown.cancel();
        }
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

async fn run_worker_pool<T>(
    mut receiver: mpsc::Receiver<T>,
    worker_concurrency: usize,
    shutdown: CancellationToken,
    handler: ReporterHandler<T>,
) where
    T: Send + 'static,
{
    let mut closing = false;
    let mut in_flight = FuturesUnordered::<ReporterFuture>::new();
    loop {
        if in_flight.len() >= worker_concurrency {
            if closing {
                let _ = in_flight.next().await;
            } else {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        receiver.close();
                        closing = true;
                    }
                    _ = in_flight.next() => {}
                }
            }
            continue;
        }

        if closing {
            match receiver.recv().await {
                Some(item) => in_flight.push(guarded_reporter_future(Arc::clone(&handler), item)),
                None => break,
            }
            continue;
        }

        if in_flight.is_empty() {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    receiver.close();
                    closing = true;
                }
                item = receiver.recv() => match item {
                    Some(item) => in_flight.push(guarded_reporter_future(Arc::clone(&handler), item)),
                    None => break,
                }
            }
        } else {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    receiver.close();
                    closing = true;
                }
                item = receiver.recv() => match item {
                    Some(item) => in_flight.push(guarded_reporter_future(Arc::clone(&handler), item)),
                    None => break,
                },
                _ = in_flight.next() => {}
            }
        }
    }

    while in_flight.next().await.is_some() {}
}

fn guarded_reporter_future<T>(handler: ReporterHandler<T>, item: T) -> ReporterFuture
where
    T: Send + 'static,
{
    Box::pin(async move {
        if AssertUnwindSafe(async move { handler(item).await })
            .catch_unwind()
            .await
            .is_err()
        {
            tracing::error!(
                event_name = "codex_ws_reporter_handler_panicked",
                log_type = "ops",
                "Codex WebSocket reporter isolated a handler panic"
            );
        }
    })
}

async fn process_settlement_commit(
    state: AppState,
    commit: CodexWsSettlementCommit,
    settlement_timeout: Duration,
    slow_settlement_tx: mpsc::Sender<CodexWsSlowSettlementCommit>,
) {
    let CodexWsSettlementCommit::Step {
        plan,
        trace_id,
        report_kind,
        report_context,
        response_headers,
        terminal_summary,
        status_code,
        first_byte_ms,
        elapsed_ms,
        cancelled,
        step_settlement,
        usage_permit,
    } = commit
    else {
        match commit {
            CodexWsSettlementCommit::CandidateAbort {
                lifecycle,
                status,
                status_code,
                error_type,
                error_message,
                preserve_sticky_binding,
            } => {
                lifecycle
                    .abort_before_write(
                        &state,
                        status,
                        status_code,
                        error_type,
                        error_message,
                        preserve_sticky_binding,
                    )
                    .await;
            }
            CodexWsSettlementCommit::HandshakeFailure {
                lifecycle,
                status_code,
                response_headers,
                error_type,
                error_message,
                error_body,
                penalize_account,
            } => {
                lifecycle
                    .settle_handshake_failure(
                        &state,
                        status_code,
                        response_headers,
                        error_type,
                        error_message,
                        error_body,
                        penalize_account,
                    )
                    .await;
            }
            CodexWsSettlementCommit::UnusedCandidates { attempts } => {
                crate::executor::candidate_loop::mark_unused_local_candidates(&state, attempts)
                    .await;
            }
            CodexWsSettlementCommit::QuotaHeaders { key_id, headers } => {
                apply_local_codex_quota_headers_effect(&state, &key_id, &headers).await;
            }
            CodexWsSettlementCommit::Step { .. } => unreachable!(),
        }
        return;
    };
    let mut payload = crate::usage::GatewayStreamReportRequest {
        trace_id,
        report_kind,
        report_context,
        status_code,
        headers: response_headers,
        provider_body_base64: None,
        provider_body_state: None,
        client_body_base64: None,
        client_body_state: None,
        terminal_summary: Some(terminal_summary),
        telemetry: Some(aether_contracts::ExecutionTelemetry {
            ttfb_ms: first_byte_ms,
            elapsed_ms,
            upstream_bytes: None,
        }),
    };

    let settlement_attempt = step_settlement.clone();
    if !run_with_hard_timeout(
        settlement_timeout,
        settlement_attempt.settle_fast(&state, &plan, payload.report_context.as_ref(), &payload),
    )
    .await
    {
        warn!(
            event_name = "codex_ws_step_settlement_timeout",
            log_type = "ops",
            provider_id = %plan.provider_id,
            key_id = %plan.key_id,
            timeout_ms = settlement_timeout.as_millis(),
            retry_lane = "bounded_slow_settlement",
            "Codex WebSocket settlement exceeded its primary deadline"
        );
        let mut usage_payload = payload.clone();
        let outcome = build_usage_outcome(&plan, &mut usage_payload, cancelled);
        let slow_retry = CodexWsSlowSettlementCommit {
            plan,
            payload,
            step_settlement,
        };
        if let Err(error) = slow_settlement_tx.try_send(slow_retry) {
            warn!(
                event_name = "codex_ws_slow_settlement_enqueue_failed",
                log_type = "ops",
                error = ?error,
                "Codex WebSocket bounded slow-settlement queue rejected a retry"
            );
        }
        if let Some(permit) = usage_permit {
            permit.send(CodexWsUsageCommit { outcome });
        }
        return;
    }

    let outcome = build_usage_outcome(&plan, &mut payload, cancelled);
    if let Some(permit) = usage_permit {
        permit.send(CodexWsUsageCommit { outcome });
    }
}

async fn process_slow_settlement_commit(
    state: AppState,
    commit: CodexWsSlowSettlementCommit,
    timeout: Duration,
) {
    let CodexWsSlowSettlementCommit {
        plan,
        payload,
        step_settlement,
    } = commit;
    if !run_with_hard_timeout(
        timeout,
        step_settlement.settle_fast(&state, &plan, payload.report_context.as_ref(), &payload),
    )
    .await
    {
        warn!(
            event_name = "codex_ws_slow_settlement_timeout",
            log_type = "ops",
            provider_id = %plan.provider_id,
            key_id = %plan.key_id,
            timeout_ms = timeout.as_millis(),
            final_strategy = "idempotent_usage_record_and_lease_ttl",
            "Codex WebSocket slow settlement exhausted its one bounded retry"
        );
    }
}

fn build_usage_outcome(
    plan: &aether_contracts::ExecutionPlan,
    payload: &mut crate::usage::GatewayStreamReportRequest,
    cancelled: bool,
) -> aether_usage_runtime::TerminalUsageOutcome {
    payload.report_context = compact_usage_report_context(payload.report_context.take());
    if cancelled {
        apply_cancelled_input_estimate(plan, payload);
    }
    let context_seed = aether_usage_runtime::build_terminal_usage_context_seed(
        plan,
        payload.report_context.as_ref(),
    );
    let payload_seed = aether_usage_runtime::build_stream_terminal_usage_payload_seed(payload);
    aether_usage_runtime::build_stream_terminal_usage_seed(context_seed, payload_seed, cancelled)
}

/// Add an input-token estimate only after a provider write has been
/// attempted and the turn is being settled as cancelled.  This is intentionally
/// kept out of the streaming relay hot path: it walks the already materialized
/// request body once in the settlement worker, and never parses response
/// chunks.  Provider-reported usage, when present, remains authoritative.
fn apply_cancelled_input_estimate(
    plan: &aether_contracts::ExecutionPlan,
    payload: &mut crate::usage::GatewayStreamReportRequest,
) {
    let Some(input_tokens) = cancelled_input_estimate(plan, payload.report_context.as_ref()) else {
        return;
    };

    let summary = payload
        .terminal_summary
        .get_or_insert_with(Default::default);
    let mut usage = summary.standardized_usage.take().unwrap_or_default();
    // Preserve an authoritative provider input count. A provider terminal
    // event may contain only output usage, in which case the estimate fills
    // the missing input side without discarding the observed output count.
    if usage.input_tokens <= 0 {
        usage.input_tokens = i64::try_from(input_tokens).unwrap_or(i64::MAX);
        // The provider did not disclose whether the prompt cache hit. Price
        // the estimate at the cheaper cache-read rate as a conservative floor
        // instead of treating the entire context as fresh input.
        if usage.cache_read_tokens <= 0 && usage.cache_creation_tokens <= 0 {
            usage.cache_read_tokens = usage.input_tokens;
        }
    } else {
        summary.standardized_usage = Some(usage);
        return;
    }
    usage.dimensions.insert(
        "usage_source".to_string(),
        json!(CANCELLED_INPUT_ESTIMATE_SOURCE),
    );
    usage
        .dimensions
        .insert("usage_confidence".to_string(), json!("billing_floor"));
    summary.standardized_usage = Some(usage);
}

fn cancelled_input_estimate(
    plan: &aether_contracts::ExecutionPlan,
    report_context: Option<&Value>,
) -> Option<u64> {
    let body = plan
        .body
        .json_body
        .as_ref()
        .or_else(|| report_context.and_then(|value| value.get("original_request_body")))?;
    estimate_request_input_tokens(body)
}

fn estimate_request_input_tokens(value: &Value) -> Option<u64> {
    let is_continuation = value
        .get("previous_response_id")
        .is_some_and(|previous| !previous.is_null());
    // A Responses WebSocket continuation sends only the newly-added input;
    // the previous response context is provider-side state and is normally
    // cache-priced. Counting it again here would overcharge a cancelled turn.
    // For an initial turn, include the complete request-side prompt fields.
    let fields: &[&str] = if is_continuation {
        &["input"]
    } else {
        &[
            "instructions",
            "input",
            "messages",
            "prompt",
            "contents",
            "system",
            "tools",
        ]
    };
    let preferred_total = value
        .as_object()
        .map(|object| {
            fields
                .iter()
                .copied()
                .filter_map(|field| object.get(field))
                .map(estimate_json_tokens)
                .fold(0_u64, u64::saturating_add)
        })
        .unwrap_or_default();
    let estimate = if preferred_total > 0 {
        preferred_total
    } else {
        estimate_json_tokens(value)
    };
    Some(estimate.min(CANCELLED_INPUT_ESTIMATE_MAX_TOKENS)).filter(|value| *value > 0)
}

fn estimate_json_tokens(value: &Value) -> u64 {
    match value {
        Value::String(text) => estimate_text_tokens(text),
        Value::Array(items) => items
            .iter()
            .map(estimate_json_tokens)
            .fold(0_u64, u64::saturating_add),
        Value::Object(object) => object
            .iter()
            .map(|(key, value)| {
                estimate_text_tokens(key).saturating_add(estimate_json_tokens(value))
            })
            .fold(0_u64, u64::saturating_add),
        Value::Null => 0,
        _ => 1,
    }
}

fn estimate_text_tokens(text: &str) -> u64 {
    if text.is_empty() || is_inline_binary_data(text) {
        0
    } else {
        // Byte length is O(1) for a Rust string. It keeps this cancellation-only
        // fallback from rescanning large prompt strings on a Tokio worker.
        (text.len() as u64).div_ceil(4).max(1)
    }
}

fn is_inline_binary_data(text: &str) -> bool {
    let prefix = &text.as_bytes()[..text.len().min(128)];
    prefix.starts_with(b"data:")
        && prefix
            .windows(b";base64,".len())
            .any(|window| window == b";base64,")
}

async fn run_with_hard_timeout<F>(timeout: Duration, future: F) -> bool
where
    F: Future<Output = ()>,
{
    tokio::time::timeout(timeout, future).await.is_ok()
}

async fn process_usage_commit(state: AppState, commit: CodexWsUsageCommit) {
    let quota_key_id = commit.outcome.provider_api_key_id.clone();
    let quota_headers = usage_headers_from_value(commit.outcome.provider_response_headers.as_ref());
    if let (Some(key_id), Some(headers)) = (quota_key_id.as_deref(), quota_headers.as_ref()) {
        apply_local_codex_quota_headers_effect(&state, key_id, headers).await;
    }

    match aether_usage_runtime::build_terminal_usage_event_from_outcome(commit.outcome) {
        Ok(event) => {
            state
                .usage_runtime
                .record_terminal_event(state.data.as_ref(), event)
                .await;
        }
        Err(error) => {
            warn!(
                event_name = "codex_ws_usage_event_build_failed",
                log_type = "ops",
                error = ?error,
                "Codex WebSocket usage event build failed"
            );
        }
    }
}

fn compact_usage_report_context(context: Option<serde_json::Value>) -> Option<serde_json::Value> {
    const FIELDS: &[&str] = &[
        "request_id",
        "trace_id",
        "user_id",
        "api_key_id",
        "username",
        "api_key_name",
        "provider_name",
        "model",
        "mapped_model",
        "model_id",
        "global_model_id",
        "provider_id",
        "endpoint_id",
        "key_id",
        "client_contract",
        "client_api_format",
        "provider_contract",
        "provider_api_format",
        "candidate_id",
        "candidate_index",
        "key_name",
        "planner_kind",
        "route_family",
        "route_kind",
        "execution_path",
        "local_execution_runtime_miss_reason",
        "needs_conversion",
        "client_ip",
        "user_agent",
        "cafecode_uid",
        "cafecode_uname",
        "client_requested_stream",
        "upstream_is_stream",
        "ws_step",
        "is_compaction",
        "compaction_version",
        "client_session_affinity",
        "api_key_is_standalone",
        "original_request_body",
        "request_body_ref",
        "provider_request_body_ref",
        "response_body_ref",
        "client_response_body_ref",
    ];
    let mut object = match context? {
        serde_json::Value::Object(object) => object,
        _ => return None,
    };
    let mut compact = serde_json::Map::new();
    for field in FIELDS {
        if let Some(value) = object.remove(*field) {
            compact.insert((*field).to_string(), value);
        }
    }
    Some(serde_json::Value::Object(compact))
}

fn usage_headers_from_value(value: Option<&serde_json::Value>) -> Option<BTreeMap<String, String>> {
    let headers = value?.as_object()?;
    let headers = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.clone(), value.to_string()))
        })
        .collect::<BTreeMap<_, _>>();
    (!headers.is_empty()).then_some(headers)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn compact_usage_report_context_preserves_ws_step() {
        let context = serde_json::json!({
            "request_id": "req-1",
            "ws_step": true,
            "is_compaction": true,
            "compaction_version": "v2",
            "cafecode_uid": "372",
            "cafecode_uname": "xiapeng8618",
            "client_session_affinity": {
                "client_family": "codex",
                "session_key": "session=session-1"
            },
            "original_request_body": {
                "type": "response.create",
                "input": [{
                    "type": "custom_tool_call_output",
                    "call_id": "call-1",
                    "output": "ok"
                }]
            },
            "secret": "drop-me"
        });

        let compact = compact_usage_report_context(Some(context)).expect("context should remain");

        assert_eq!(compact["request_id"], "req-1");
        assert_eq!(compact["ws_step"], true);
        assert_eq!(compact["is_compaction"], true);
        assert_eq!(compact["compaction_version"], "v2");
        assert_eq!(compact["cafecode_uid"], "372");
        assert_eq!(compact["cafecode_uname"], "xiapeng8618");
        assert_eq!(
            compact["client_session_affinity"]["session_key"],
            "session=session-1"
        );
        assert_eq!(compact["client_session_affinity"]["client_family"], "codex");
        assert_eq!(
            compact["original_request_body"]["input"][0]["call_id"],
            "call-1"
        );
        assert!(compact.get("secret").is_none());
    }

    #[test]
    fn ws_usage_seed_captures_client_and_materialized_provider_bodies() {
        let original_request_body = serde_json::json!({
            "type": "response.create",
            "previous_response_id": "resp-1",
            "input": [{
                "type": "custom_tool_call_output",
                "call_id": "call-1",
                "output": "ok"
            }]
        });
        let provider_request_body = serde_json::json!({
            "type": "response.create",
            "model": "gpt-5.6-terra",
            "previous_response_id": "resp-1",
            "stream": true,
            "input": [{
                "type": "custom_tool_call_output",
                "call_id": "call-1",
                "output": "ok"
            }]
        });
        let context = compact_usage_report_context(Some(serde_json::json!({
            "request_id": "ws-request-1",
            "ws_step": true,
            "client_session_affinity": {
                "client_family": "codex",
                "session_key": "session=session-1"
            },
            "original_request_body": original_request_body
        })))
        .expect("WS report context should remain");
        let plan = aether_contracts::ExecutionPlan {
            request_id: "ws-request-1".into(),
            candidate_id: Some("candidate-1".into()),
            provider_name: Some("Codex".into()),
            provider_id: "provider-1".into(),
            endpoint_id: "endpoint-1".into(),
            key_id: "key-1".into(),
            method: "GET".into(),
            url: "wss://chatgpt.com/backend-api/codex/responses".into(),
            headers: BTreeMap::new(),
            content_type: Some("application/json".into()),
            content_encoding: None,
            body: aether_contracts::RequestBody::from_json(provider_request_body.clone()),
            stream: true,
            client_api_format: "openai:responses".into(),
            provider_api_format: "openai:responses".into(),
            model_name: Some("gpt-5.6-terra".into()),
            proxy: None,
            transport_profile: None,
            timeouts: None,
        };

        let seed = aether_usage_runtime::build_terminal_usage_context_seed(&plan, Some(&context));

        assert_eq!(seed.request_body, Some(original_request_body));
        assert_eq!(seed.provider_request, Some(provider_request_body));
        assert_eq!(
            seed.request_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("client_session_affinity"))
                .and_then(|affinity| affinity.get("session_key")),
            Some(&serde_json::json!("session=session-1"))
        );
    }

    #[test]
    fn cancelled_input_estimate_is_confined_to_the_cold_settlement_path() {
        let request_body = serde_json::json!({
            "model": "gpt-5.6-sol",
            "input": "A deliberately long enough prompt to produce an estimate",
            "stream": true
        });
        let plan = aether_contracts::ExecutionPlan {
            request_id: "ws-cancel-estimate-test".into(),
            candidate_id: Some("candidate-1".into()),
            provider_name: Some("Codex".into()),
            provider_id: "provider-1".into(),
            endpoint_id: "endpoint-1".into(),
            key_id: "key-1".into(),
            method: "GET".into(),
            url: "wss://chatgpt.com/backend-api/codex/responses".into(),
            headers: BTreeMap::new(),
            content_type: Some("application/json".into()),
            content_encoding: None,
            body: aether_contracts::RequestBody::from_json(request_body),
            stream: true,
            client_api_format: "openai:responses".into(),
            provider_api_format: "openai:responses".into(),
            model_name: Some("gpt-5.6-sol".into()),
            proxy: None,
            transport_profile: None,
            timeouts: None,
        };

        let mut cancelled = crate::usage::GatewayStreamReportRequest {
            trace_id: plan.request_id.clone(),
            report_kind: "openai_responses_stream_cancelled".into(),
            report_context: None,
            status_code: 499,
            headers: BTreeMap::new(),
            provider_body_base64: None,
            provider_body_state: None,
            client_body_base64: None,
            client_body_state: None,
            terminal_summary: None,
            telemetry: None,
        };
        apply_cancelled_input_estimate(&plan, &mut cancelled);
        let estimated_input = cancelled
            .terminal_summary
            .as_ref()
            .and_then(|summary| summary.standardized_usage.as_ref())
            .map(|usage| usage.input_tokens)
            .unwrap_or_default();
        assert!(estimated_input > 0);
        assert_eq!(
            cancelled
                .terminal_summary
                .as_ref()
                .and_then(|summary| summary.standardized_usage.as_ref())
                .map(|usage| usage.cache_read_tokens),
            Some(estimated_input)
        );
        assert_eq!(
            cancelled
                .terminal_summary
                .as_ref()
                .and_then(|summary| summary.standardized_usage.as_ref())
                .and_then(|usage| usage.dimensions.get("usage_source"))
                .and_then(Value::as_str),
            Some(CANCELLED_INPUT_ESTIMATE_SOURCE)
        );
        let estimated_outcome = build_usage_outcome(&plan, &mut cancelled, true);
        assert!(estimated_outcome
            .standardized_usage
            .as_ref()
            .is_some_and(|usage| usage.input_tokens > 0));

        // The helper is called only for cancelled settlement commits. A normal
        // completed turn never receives this fallback estimate.
        let mut completed = crate::usage::GatewayStreamReportRequest {
            trace_id: plan.request_id.clone(),
            report_kind: "openai_responses_stream_success".into(),
            report_context: None,
            status_code: 200,
            headers: BTreeMap::new(),
            provider_body_base64: None,
            provider_body_state: None,
            client_body_base64: None,
            client_body_state: None,
            terminal_summary: None,
            telemetry: None,
        };
        // Do not invoke the fallback for completed requests; this mirrors
        // build_usage_outcome's `if cancelled` guard.
        let outcome = build_usage_outcome(&plan, &mut completed, false);
        assert!(outcome.standardized_usage.is_none());
    }

    #[test]
    fn cancelled_input_estimate_fills_only_missing_input_usage() {
        let plan = aether_contracts::ExecutionPlan {
            request_id: "ws-cancel-output-only-test".into(),
            candidate_id: None,
            provider_name: Some("Codex".into()),
            provider_id: "provider-1".into(),
            endpoint_id: "endpoint-1".into(),
            key_id: "key-1".into(),
            method: "GET".into(),
            url: "wss://chatgpt.com/backend-api/codex/responses".into(),
            headers: BTreeMap::new(),
            content_type: Some("application/json".into()),
            content_encoding: None,
            body: aether_contracts::RequestBody::from_json(serde_json::json!({
                "input": "short prompt"
            })),
            stream: true,
            client_api_format: "openai:responses".into(),
            provider_api_format: "openai:responses".into(),
            model_name: Some("gpt-5.6-sol".into()),
            proxy: None,
            transport_profile: None,
            timeouts: None,
        };
        let mut provider_usage = aether_contracts::StandardizedUsage::new();
        provider_usage.output_tokens = 7;
        let mut payload = crate::usage::GatewayStreamReportRequest {
            trace_id: plan.request_id.clone(),
            report_kind: "openai_responses_stream_cancelled".into(),
            report_context: None,
            status_code: 499,
            headers: BTreeMap::new(),
            provider_body_base64: None,
            provider_body_state: None,
            client_body_base64: None,
            client_body_state: None,
            terminal_summary: Some(aether_contracts::ExecutionStreamTerminalSummary {
                standardized_usage: Some(provider_usage),
                ..Default::default()
            }),
            telemetry: None,
        };
        apply_cancelled_input_estimate(&plan, &mut payload);
        let usage = payload
            .terminal_summary
            .as_ref()
            .and_then(|summary| summary.standardized_usage.as_ref())
            .expect("usage should remain present");
        assert!(usage.input_tokens > 0);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.cache_read_tokens, usage.input_tokens);
    }

    #[test]
    fn continuation_estimate_counts_only_new_input() {
        let continued = serde_json::json!({
            "previous_response_id": "resp-1",
            "instructions": "This inherited instruction must not be charged again",
            "tools": [{"type": "function", "name": "large_tool_schema"}],
            "input": "new"
        });
        let initial = serde_json::json!({
            "instructions": "This initial instruction is part of the first request",
            "tools": [{"type": "function", "name": "large_tool_schema"}],
            "input": "new"
        });

        let continued_tokens = estimate_request_input_tokens(&continued).expect("estimate");
        let initial_tokens = estimate_request_input_tokens(&initial).expect("estimate");

        assert_eq!(continued_tokens, estimate_json_tokens(&continued["input"]));
        assert!(initial_tokens > continued_tokens);
    }

    #[test]
    fn production_config_enforces_capacity_and_worker_bounds() {
        assert_eq!(
            normalize_bounded_usize(None, DEFAULT_CAPACITY, MIN_CAPACITY, MAX_CAPACITY),
            DEFAULT_CAPACITY
        );
        assert_eq!(
            normalize_bounded_usize(Some("1"), DEFAULT_CAPACITY, MIN_CAPACITY, MAX_CAPACITY),
            MIN_CAPACITY
        );
        assert_eq!(
            normalize_bounded_usize(Some("999999"), DEFAULT_CAPACITY, MIN_CAPACITY, MAX_CAPACITY,),
            MAX_CAPACITY
        );
        assert_eq!(
            normalize_bounded_usize(
                Some("invalid"),
                DEFAULT_CAPACITY,
                MIN_CAPACITY,
                MAX_CAPACITY,
            ),
            DEFAULT_CAPACITY
        );
        let config = CodexWsUsageReporterConfig::from_env();
        assert!((MIN_CAPACITY..=MAX_CAPACITY).contains(&config.capacity));
        assert!(
            (MIN_WORKER_CONCURRENCY..=MAX_WORKER_CONCURRENCY).contains(&config.worker_concurrency)
        );
        assert_eq!(
            normalize_bounded_usize(
                Some("1"),
                DEFAULT_SLOW_SETTLEMENT_CAPACITY,
                MIN_SLOW_SETTLEMENT_CAPACITY,
                MAX_SLOW_SETTLEMENT_CAPACITY,
            ),
            MIN_SLOW_SETTLEMENT_CAPACITY
        );
        assert_eq!(
            normalize_bounded_usize(
                Some("999999"),
                DEFAULT_SLOW_SETTLEMENT_WORKER_CONCURRENCY,
                MIN_SLOW_SETTLEMENT_WORKER_CONCURRENCY,
                MAX_SLOW_SETTLEMENT_WORKER_CONCURRENCY,
            ),
            MAX_SLOW_SETTLEMENT_WORKER_CONCURRENCY
        );
    }

    #[tokio::test]
    async fn ten_thousand_sender_clones_share_one_queue_and_one_fixed_worker_pool() {
        let reporter =
            SharedBoundedReporter::<usize>::new(CodexWsUsageReporterConfig::for_test(10_001, 3));
        let first = reporter.sender();
        for _ in 0..10_000 {
            assert!(first.same_channel(&reporter.sender()));
        }

        let processed = Arc::new(AtomicUsize::new(0));
        let processed_for_handler = Arc::clone(&processed);
        let handler = Arc::new(move |_item| {
            let processed = Arc::clone(&processed_for_handler);
            Box::pin(async move {
                processed.fetch_add(1, Ordering::AcqRel);
            }) as ReporterFuture
        });
        let worker = reporter
            .start_with_handler("codex-ws-usage-reporter-test", handler)
            .expect("worker should start once");
        assert_eq!(reporter.worker_concurrency, 3);
        assert_eq!(reporter.worker_start_count.load(Ordering::Acquire), 1);
        assert_eq!(
            reporter
                .start_with_handler(
                    "codex-ws-usage-reporter-test-duplicate",
                    Arc::new(|_| Box::pin(async {}) as ReporterFuture),
                )
                .expect_err("second worker pool must be rejected"),
            CodexWsUsageReporterStartError::AlreadyStarted
        );
        assert!(worker.shutdown(Duration::from_secs(1)).await);
        assert_eq!(processed.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn capacity_backpressure_is_global_and_dropped_reservations_release_capacity() {
        let reporter =
            SharedBoundedReporter::<usize>::new(CodexWsUsageReporterConfig::for_test(2, 1));
        let first = reporter.reserve_owned().await.expect("first reservation");
        let second = reporter.reserve_owned().await.expect("second reservation");
        assert!(matches!(
            reporter.try_reserve_owned(),
            Err(mpsc::error::TrySendError::Full(_))
        ));

        drop(first);
        let replacement = reporter
            .try_reserve_owned()
            .expect("dropping a reservation should release capacity");
        drop(replacement);
        drop(second);
    }

    #[tokio::test]
    async fn worker_pool_never_exceeds_configured_processing_concurrency() {
        let reporter =
            SharedBoundedReporter::<usize>::new(CodexWsUsageReporterConfig::for_test(16, 3));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let active_for_handler = Arc::clone(&active);
        let maximum_for_handler = Arc::clone(&maximum);
        let release_for_handler = Arc::clone(&release);
        let handler = Arc::new(move |_item| {
            let active = Arc::clone(&active_for_handler);
            let maximum = Arc::clone(&maximum_for_handler);
            let release = Arc::clone(&release_for_handler);
            Box::pin(async move {
                let now = active.fetch_add(1, Ordering::AcqRel).saturating_add(1);
                maximum.fetch_max(now, Ordering::AcqRel);
                let permit = release.acquire().await.expect("release semaphore open");
                permit.forget();
                active.fetch_sub(1, Ordering::AcqRel);
            }) as ReporterFuture
        });
        let worker = reporter
            .start_with_handler("codex-ws-usage-reporter-concurrency-test", handler)
            .expect("worker should start");
        for item in 0..9 {
            reporter
                .sender()
                .send(item)
                .await
                .expect("queue should accept test item");
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            while maximum.load(Ordering::Acquire) < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("configured worker concurrency should become active");
        assert_eq!(maximum.load(Ordering::Acquire), 3);
        release.add_permits(9);
        assert!(worker.shutdown(Duration::from_secs(1)).await);
        assert_eq!(maximum.load(Ordering::Acquire), 3);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn handler_panic_isolated_and_later_items_still_drain() {
        let reporter =
            SharedBoundedReporter::<usize>::new(CodexWsUsageReporterConfig::for_test(4, 1));
        let processed = Arc::new(AtomicUsize::new(0));
        let processed_for_handler = Arc::clone(&processed);
        let handler = Arc::new(move |item| {
            let processed = Arc::clone(&processed_for_handler);
            Box::pin(async move {
                if item == 1 {
                    panic!("intentional reporter test panic");
                }
                processed.fetch_add(item, Ordering::AcqRel);
            }) as ReporterFuture
        });
        let worker = reporter
            .start_with_handler("codex-ws-reporter-panic-test", handler)
            .expect("worker should start");
        reporter.sender().send(1).await.expect("panic item");
        reporter.sender().send(2).await.expect("later item");

        assert!(worker.shutdown(Duration::from_secs(1)).await);
        assert_eq!(processed.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn hard_timeout_releases_worker_capacity_for_later_items() {
        let reporter =
            SharedBoundedReporter::<usize>::new(CodexWsUsageReporterConfig::for_test(4, 1));
        let processed = Arc::new(AtomicUsize::new(0));
        let processed_for_handler = Arc::clone(&processed);
        let handler = Arc::new(move |item| {
            let processed = Arc::clone(&processed_for_handler);
            Box::pin(async move {
                if item == 1 {
                    assert!(
                        !run_with_hard_timeout(
                            Duration::from_millis(10),
                            std::future::pending::<()>(),
                        )
                        .await
                    );
                }
                processed.fetch_add(item, Ordering::AcqRel);
            }) as ReporterFuture
        });
        let worker = reporter
            .start_with_handler("codex-ws-reporter-hard-timeout-test", handler)
            .expect("worker should start");
        reporter.sender().send(1).await.expect("timeout item");
        reporter.sender().send(2).await.expect("later item");

        assert!(worker.shutdown(Duration::from_secs(1)).await);
        assert_eq!(processed.load(Ordering::Acquire), 3);
    }

    #[tokio::test]
    async fn committed_items_drain_on_shutdown_while_dropped_permits_emit_nothing() {
        let reporter =
            SharedBoundedReporter::<usize>::new(CodexWsUsageReporterConfig::for_test(4, 2));
        let sum = Arc::new(AtomicUsize::new(0));
        let sum_for_handler = Arc::clone(&sum);
        let handler = Arc::new(move |item| {
            let sum = Arc::clone(&sum_for_handler);
            Box::pin(async move {
                sum.fetch_add(item, Ordering::AcqRel);
            }) as ReporterFuture
        });
        let worker = reporter
            .start_with_handler("codex-ws-usage-reporter-drain-test", handler)
            .expect("worker should start");

        let committed = reporter.reserve_owned().await.expect("commit reservation");
        committed.send(7);
        let dropped = reporter.reserve_owned().await.expect("drop reservation");
        drop(dropped);

        assert!(worker.shutdown(Duration::from_secs(1)).await);
        assert_eq!(sum.load(Ordering::Acquire), 7);
        assert!(reporter.reserve_owned().await.is_err());
    }
}
