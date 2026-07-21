use async_trait::async_trait;

pub trait AiExecutionAttempt {
    fn execution_plan(&self) -> &aether_contracts::ExecutionPlan;

    fn report_kind(&self) -> Option<String>;

    fn report_context(&self) -> Option<serde_json::Value>;
}

#[derive(Debug)]
pub enum AiAttemptLoopOutcome<Response, Exhaustion> {
    Responded(Response),
    Exhausted(Exhaustion),
    NoPath,
}

#[derive(Debug)]
pub enum AiAttemptExecutionOutcome<Response> {
    Responded(Response),
    FailedAfterExecutionDispatch,
    SkippedBeforeExecutionDispatch,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AiAttemptLoopProgress {
    attempt_count: usize,
    execution_dispatched: bool,
}

impl AiAttemptLoopProgress {
    pub fn record_attempt_started(&mut self) {
        self.attempt_count = self.attempt_count.saturating_add(1);
    }

    pub fn record_non_response<Response>(&mut self, outcome: &AiAttemptExecutionOutcome<Response>) {
        if matches!(
            outcome,
            AiAttemptExecutionOutcome::FailedAfterExecutionDispatch
        ) {
            self.execution_dispatched = true;
        }
    }

    pub fn attempt_count(&self) -> usize {
        self.attempt_count
    }

    pub fn execution_dispatched(&self) -> bool {
        self.execution_dispatched
    }
}

#[async_trait]
pub trait AiAttemptLoopPort<Attempt>: Send + Sync
where
    Attempt: AiExecutionAttempt + Send + Sync + 'static,
{
    type Response: Send;
    type Exhaustion: Send;
    type Error: Send;

    async fn execute_attempt(
        &self,
        attempt: &Attempt,
    ) -> Result<AiAttemptExecutionOutcome<Self::Response>, Self::Error>;

    async fn mark_unused_attempts(&self, attempts: Vec<Attempt>) -> Result<(), Self::Error>;

    async fn build_exhaustion(
        &self,
        last_plan: aether_contracts::ExecutionPlan,
        last_report_context: Option<serde_json::Value>,
        execution_dispatched: bool,
        attempt_count: usize,
    ) -> Result<Self::Exhaustion, Self::Error>;
}

pub async fn run_ai_attempt_loop<Port, Attempt>(
    port: &Port,
    attempts: Vec<Attempt>,
) -> Result<AiAttemptLoopOutcome<Port::Response, Port::Exhaustion>, Port::Error>
where
    Port: AiAttemptLoopPort<Attempt>,
    Attempt: AiExecutionAttempt + Send + Sync + 'static,
{
    let mut remaining = attempts.into_iter();
    let mut last_attempted = None;
    let mut progress = AiAttemptLoopProgress::default();

    while let Some(attempt) = remaining.next() {
        progress.record_attempt_started();
        last_attempted = Some((attempt.execution_plan().clone(), attempt.report_context()));
        match port.execute_attempt(&attempt).await? {
            AiAttemptExecutionOutcome::Responded(response) => {
                port.mark_unused_attempts(remaining.collect()).await?;
                return Ok(AiAttemptLoopOutcome::Responded(response));
            }
            outcome => progress.record_non_response(&outcome),
        }
    }

    let Some((last_plan, last_report_context)) = last_attempted else {
        return Ok(AiAttemptLoopOutcome::NoPath);
    };

    Ok(AiAttemptLoopOutcome::Exhausted(
        port.build_exhaustion(
            last_plan,
            last_report_context,
            progress.execution_dispatched(),
            progress.attempt_count(),
        )
        .await?,
    ))
}

impl AiExecutionAttempt for crate::dto::AiSyncAttempt {
    fn execution_plan(&self) -> &aether_contracts::ExecutionPlan {
        &self.plan
    }

    fn report_kind(&self) -> Option<String> {
        self.report_kind.clone()
    }

    fn report_context(&self) -> Option<serde_json::Value> {
        self.report_context.clone()
    }
}

impl AiExecutionAttempt for crate::dto::AiStreamAttempt {
    fn execution_plan(&self) -> &aether_contracts::ExecutionPlan {
        &self.plan
    }

    fn report_kind(&self) -> Option<String> {
        self.report_kind.clone()
    }

    fn report_context(&self) -> Option<serde_json::Value> {
        self.report_context.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aether_contracts::{ExecutionPlan, RequestBody};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct TestAttempt {
        plan: ExecutionPlan,
    }

    impl AiExecutionAttempt for TestAttempt {
        fn execution_plan(&self) -> &ExecutionPlan {
            &self.plan
        }

        fn report_kind(&self) -> Option<String> {
            None
        }

        fn report_context(&self) -> Option<serde_json::Value> {
            None
        }
    }

    struct TestPort {
        outcomes: Mutex<VecDeque<AiAttemptExecutionOutcome<&'static str>>>,
        exhaustion_flags: Mutex<Vec<bool>>,
        unused_attempt_counts: Mutex<Vec<usize>>,
    }

    #[async_trait]
    impl AiAttemptLoopPort<TestAttempt> for TestPort {
        type Response = &'static str;
        type Exhaustion = (bool, usize);
        type Error = ();

        async fn execute_attempt(
            &self,
            _attempt: &TestAttempt,
        ) -> Result<AiAttemptExecutionOutcome<Self::Response>, Self::Error> {
            Ok(self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .expect("test outcome"))
        }

        async fn mark_unused_attempts(
            &self,
            attempts: Vec<TestAttempt>,
        ) -> Result<(), Self::Error> {
            self.unused_attempt_counts
                .lock()
                .unwrap()
                .push(attempts.len());
            Ok(())
        }

        async fn build_exhaustion(
            &self,
            _last_plan: ExecutionPlan,
            _last_report_context: Option<serde_json::Value>,
            execution_dispatched: bool,
            attempt_count: usize,
        ) -> Result<Self::Exhaustion, Self::Error> {
            self.exhaustion_flags
                .lock()
                .unwrap()
                .push(execution_dispatched);
            Ok((execution_dispatched, attempt_count))
        }
    }

    fn test_attempt(index: usize) -> TestAttempt {
        TestAttempt {
            plan: ExecutionPlan {
                request_id: format!("request-{index}"),
                candidate_id: Some(format!("candidate-{index}")),
                provider_name: Some("provider".to_string()),
                provider_id: "provider-id".to_string(),
                endpoint_id: "endpoint-id".to_string(),
                key_id: "key-id".to_string(),
                method: "POST".to_string(),
                url: "https://example.com/v1/chat/completions".to_string(),
                headers: Default::default(),
                content_type: Some("application/json".to_string()),
                content_encoding: None,
                body: RequestBody::from_json(serde_json::json!({"model": "model"})),
                stream: false,
                client_api_format: "openai:chat".to_string(),
                provider_api_format: "openai:chat".to_string(),
                model_name: Some("model".to_string()),
                proxy: None,
                transport_profile: None,
                timeouts: None,
            },
        }
    }

    #[tokio::test]
    async fn attempt_loop_reports_dispatch_only_after_an_execution_attempt() {
        let port = TestPort {
            outcomes: Mutex::new(VecDeque::from([
                AiAttemptExecutionOutcome::SkippedBeforeExecutionDispatch,
                AiAttemptExecutionOutcome::FailedAfterExecutionDispatch,
            ])),
            exhaustion_flags: Mutex::new(Vec::new()),
            unused_attempt_counts: Mutex::new(Vec::new()),
        };
        let outcome = run_ai_attempt_loop(&port, vec![test_attempt(1), test_attempt(2)])
            .await
            .expect("attempt loop should complete");

        assert!(matches!(
            outcome,
            AiAttemptLoopOutcome::Exhausted((true, 2))
        ));
        assert_eq!(*port.exhaustion_flags.lock().unwrap(), vec![true]);
    }

    #[tokio::test]
    async fn attempt_loop_preserves_pre_dispatch_skip_state_when_all_attempts_skip() {
        let port = TestPort {
            outcomes: Mutex::new(VecDeque::from([
                AiAttemptExecutionOutcome::SkippedBeforeExecutionDispatch,
                AiAttemptExecutionOutcome::SkippedBeforeExecutionDispatch,
            ])),
            exhaustion_flags: Mutex::new(Vec::new()),
            unused_attempt_counts: Mutex::new(Vec::new()),
        };
        let outcome = run_ai_attempt_loop(&port, vec![test_attempt(1), test_attempt(2)])
            .await
            .expect("attempt loop should complete");

        assert!(matches!(
            outcome,
            AiAttemptLoopOutcome::Exhausted((false, 2))
        ));
        assert_eq!(*port.exhaustion_flags.lock().unwrap(), vec![false]);
    }

    #[tokio::test]
    async fn attempt_loop_continues_fallback_until_a_candidate_responds() {
        let port = TestPort {
            outcomes: Mutex::new(VecDeque::from([
                AiAttemptExecutionOutcome::SkippedBeforeExecutionDispatch,
                AiAttemptExecutionOutcome::FailedAfterExecutionDispatch,
                AiAttemptExecutionOutcome::Responded("ok"),
            ])),
            exhaustion_flags: Mutex::new(Vec::new()),
            unused_attempt_counts: Mutex::new(Vec::new()),
        };
        let outcome = run_ai_attempt_loop(
            &port,
            vec![
                test_attempt(1),
                test_attempt(2),
                test_attempt(3),
                test_attempt(4),
            ],
        )
        .await
        .expect("attempt loop should complete");

        assert!(matches!(outcome, AiAttemptLoopOutcome::Responded("ok")));
        assert!(port.exhaustion_flags.lock().unwrap().is_empty());
        assert_eq!(*port.unused_attempt_counts.lock().unwrap(), vec![1]);
    }
}
