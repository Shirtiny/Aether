// Included in execution::tests to exercise the real direct HTTP, conversion,
// candidate and settlement path with its existing in-memory fixtures.
#[test]
fn overload_retry_terminal_tracker_recognizes_untyped_chat_errors_but_not_text() {
    let mut tracker = ClientVisibleStreamCompletionTracker::default();
    assert!(!tracker.observe_chunk(b"data: {\"error\":{\"code\":503,"));
    assert!(tracker.observe_chunk(b"\"message\":\"overloaded\"}}\n\n"));
    let mut content = ClientVisibleStreamCompletionTracker::default();
    assert!(!content.observe_chunk(
        b"data: {\"choices\":[{\"delta\":{\"content\":\"error: overloaded\"}}]}\n\n"
    ));
}

fn overload_retry_success_response() -> Value {
    json!({
        "id": "resp-overload-winner", "object": "response", "status": "completed",
        "model": "gpt-5.5",
        "output": [{"type":"message", "id":"msg-winner", "role":"assistant", "status":"completed",
            "content":[{"type":"output_text", "text":"hello", "annotations":[]}]}],
        "usage": {"input_tokens":10, "output_tokens":1, "total_tokens":11}
    })
}

fn overload_retry_success_sse() -> String {
    [
        json!({"type":"response.created", "response":{"id":"resp-overload-winner", "status":"in_progress", "output":[]}}),
        json!({"type":"response.output_item.added", "output_index":0, "item":{"type":"message", "id":"msg-winner", "role":"assistant", "content":[]}}),
        json!({"type":"response.content_part.added", "output_index":0, "content_index":0, "part":{"type":"output_text", "text":"", "annotations":[]}}),
        json!({"type":"response.output_text.delta", "item_id":"msg-winner", "output_index":0, "content_index":0, "delta":"hello"}),
        json!({"type":"response.completed", "response":overload_retry_success_response()}),
    ].into_iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect()
}

#[tokio::test]
async fn overload_retry_real_http_late_sse_preserves_identity_conversion_and_single_settlement() {
    for (client_format, plan_kind, disconnect, persistent) in [
        ("openai:responses", "openai_responses_stream", false, false),
        ("openai:chat", "openai_chat_stream", false, false),
        ("claude:messages", "claude_cli_stream", false, false),
        ("openai:responses", "openai_responses_stream", true, false),
        ("openai:responses", "openai_responses_stream", false, true),
    ] {
        let listener = crate::test_support::bind_loopback_listener().await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let reject = Arc::new(Notify::new());
        let calls_handler = calls.clone();
        let reject_handler = reject.clone();
        let app = Router::new().route("/responses", any(move |request: Request| {
            let calls = calls_handler.clone();
            let reject = reject_handler.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = to_bytes(body, 1_000_000).await.unwrap();
                let mut seen = calls.lock().await;
                let first = seen.is_empty();
                seen.push((parts.headers, body));
                drop(seen);
                let body = if first || persistent {
                    Body::from_stream(stream! {
                        yield Ok::<Bytes, Infallible>(Bytes::from_static(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp-hidden-failed\",\"status\":\"in_progress\",\"output\":[]}}\n\n"));
                        if first { reject.notified().await; }
                        yield Ok(Bytes::from_static(b"data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":503,\"message\":\"Our servers are currently overloaded. Please try again later.\"}}}\n\n"));
                    })
                } else { Body::from(overload_retry_success_sse()) };
                axum::http::Response::builder().header("content-type", "text/event-stream").body(body).unwrap()
            }
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (state, usage_repository, candidate_repository) = test_stream_state();
        let mut plan = test_responses_stream_plan("req-overload-wire", "cand-overload-wire");
        plan.url = format!("http://{addr}/responses");
        plan.client_api_format = client_format.into();
        plan.headers
            .insert("authorization".into(), "Bearer local-mock-only".into());
        plan.headers
            .insert("session_id".into(), "same-session".into());
        let mut context = test_prefetch_report_context(&plan.request_id, "cand-overload-wire");
        context["client_api_format"] = json!(client_format);
        context["needs_conversion"] = json!(client_format != "openai:responses");
        context["mapped_model"] = json!("gpt-5.5");

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            execute_execution_runtime_stream(
                &state,
                plan,
                "trace-overload-wire",
                &test_decision(),
                plan_kind,
                None,
                Some(context),
            ),
        )
        .await
        .expect("headers must not wait for content or a late rejection")
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(calls.lock().await.len(), 1);
        assert!(usage_repository
            .find_by_request_id("req-overload-wire")
            .await
            .unwrap()
            .is_none_or(|usage| matches!(usage.status.as_str(), "pending" | "streaming")));
        if disconnect {
            drop(response);
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if usage_repository
                        .find_by_request_id("req-overload-wire")
                        .await
                        .unwrap()
                        .is_some_and(|usage| usage.status == "cancelled")
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("disconnect before content must drop the retry source without drain grace");
            reject.notify_one();
            tokio::time::sleep(Duration::from_millis(450)).await;
            assert_eq!(
                calls.lock().await.len(),
                1,
                "no retry after downstream disconnect"
            );
            server.abort();
            continue;
        }
        // HTTP prefetch has already ended, but no content has been sent. This
        // exercises the late retry inside the returned streaming response body.
        reject.notify_one();
        let body = tokio::time::timeout(
            Duration::from_secs(5),
            to_bytes(response.into_body(), 1_000_000),
        )
        .await
        .unwrap()
        .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        if persistent {
            assert_eq!(
                body.matches("Our servers are currently overloaded").count(),
                1,
                "{body}"
            );
            assert!(!body.contains("response.completed"), "{body}");
        } else {
            assert!(body.contains("hello"), "{client_format}: {body}");
            assert!(!body.contains("resp-hidden-failed"), "{body}");
            assert!(!body.contains("overloaded"), "{body}");
        }
        let seen = calls.lock().await;
        assert_eq!(seen.len(), if persistent { 3 } else { 2 });
        for pair in seen.windows(2) {
            assert_eq!(
                pair[0], pair[1],
                "retry must reuse exact headers/body and session identity"
            );
        }
        drop(seen);
        let usage = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(usage) = usage_repository
                    .find_by_request_id("req-overload-wire")
                    .await
                    .unwrap()
                    .filter(|u| matches!(u.status.as_str(), "completed" | "failed" | "cancelled"))
                {
                    break usage;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            usage.status,
            if persistent { "failed" } else { "completed" },
            "{client_format}: {usage:?}"
        );
        assert_eq!(usage.output_tokens, if persistent { 0 } else { 1 });
        let candidates = candidate_repository
            .list_by_request_id("req-overload-wire")
            .await
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].status,
            if persistent {
                RequestCandidateStatus::Failed
            } else {
                RequestCandidateStatus::Success
            }
        );
        server.abort();
    }
}

#[tokio::test]
async fn overload_retry_real_http_503_sync_and_stream_reuse_the_prepared_request() {
    for stream in [false, true] {
        let listener = crate::test_support::bind_loopback_listener().await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let handler_calls = calls.clone();
        let app = Router::new().route("/responses", any(move |request: Request| {
            let calls = handler_calls.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = to_bytes(body, 1_000_000).await.unwrap();
                let mut calls = calls.lock().await;
                let first = calls.is_empty();
                calls.push((parts.headers, body));
                if first {
                    axum::http::Response::builder().status(503).header("content-type", "application/json").body(Body::from(r#"{"error":{"message":"Our servers are currently overloaded. Please try again later."}}"#)).unwrap()
                } else if stream {
                    axum::http::Response::builder().header("content-type", "text/event-stream").body(Body::from(overload_retry_success_sse())).unwrap()
                } else {
                    axum::http::Response::builder().header("content-type", "application/json").body(Body::from(overload_retry_success_response().to_string())).unwrap()
                }
            }
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (state, usage_repository, candidate_repository) = test_stream_state();
        let mut plan = test_responses_stream_plan("req-overload-http", "cand-overload-http");
        plan.url = format!("http://{addr}/responses");
        plan.stream = stream;
        plan.body = RequestBody::from_json(json!({"model":"gpt-5.5", "input":[], "stream":stream}));
        let context = Some(test_prefetch_report_context(
            &plan.request_id,
            "cand-overload-http",
        ));
        let response = if stream {
            execute_execution_runtime_stream(
                &state,
                plan,
                "trace-overload-http",
                &test_decision(),
                "openai_responses_stream",
                None,
                context,
            )
            .await
        } else {
            crate::execution_runtime::execute_execution_runtime_sync(
                &state,
                "/v1/responses",
                plan,
                "trace-overload-http",
                &test_decision(),
                "openai_responses_sync",
                None,
                context,
            )
            .await
        }
        .unwrap()
        .expect("same-plan retry should succeed");
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("hello"));
        let seen = calls.lock().await;
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1]);
        drop(seen);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if usage_repository
                    .find_by_request_id("req-overload-http")
                    .await
                    .unwrap()
                    .is_some_and(|u| u.status == "completed")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let candidates = candidate_repository
            .list_by_request_id("req-overload-http")
            .await
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].status, RequestCandidateStatus::Success);
        server.abort();
    }
}
