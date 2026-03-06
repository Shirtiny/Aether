from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from src.core.exceptions import EmbeddedErrorException
from src.services.candidate.schema import CandidateKey
from src.services.candidate.submit import SubmitOutcome
from src.services.request.executor import ExecutionContext, ExecutionError
from src.services.task import service as task_service_module
from src.services.task.context import TaskMode
from src.services.task.protocol import AttemptKind
from src.services.task.service import TaskService


@pytest.mark.asyncio
async def test_task_service_execute_async_requires_extract_external_task_id() -> None:
    svc = TaskService(MagicMock())
    with pytest.raises(ValueError):
        await svc.execute(
            task_type="video",
            task_mode=TaskMode.ASYNC,
            api_format="openai:video",
            model_name="m",
            user_api_key=MagicMock(id="u", user_id="user"),
            request_func=AsyncMock(),
            request_id="rid",
        )


@pytest.mark.asyncio
async def test_task_service_execute_async_returns_execution_result() -> None:
    db = MagicMock()
    svc = TaskService(db)

    candidate = SimpleNamespace(
        provider=SimpleNamespace(id="p1", name="prov"),
        endpoint=SimpleNamespace(id="e1"),
        key=SimpleNamespace(id="k1"),
    )
    outcome = SubmitOutcome(
        candidate=candidate,  # type: ignore[arg-type]
        candidate_keys=[{"index": 0, "provider_id": "p1"}],
        external_task_id="task_123",
        rule_lookup=None,
        upstream_payload={"id": "x"},
        upstream_headers={"x-test": "1"},
        upstream_status_code=200,
    )

    svc.submit_with_failover = AsyncMock(return_value=outcome)  # type: ignore[method-assign]
    svc._recorder.get_candidate_keys = MagicMock(  # type: ignore[attr-defined, method-assign]
        return_value=[
            CandidateKey(candidate_index=0, retry_index=0, status="success", provider_id="p1")
        ]
    )

    result = await svc.execute(
        task_type="video",
        task_mode=TaskMode.ASYNC,
        api_format="openai:video",
        model_name="m",
        user_api_key=MagicMock(id="u", user_id="user"),
        request_func=AsyncMock(),
        request_id="rid",
        extract_external_task_id=MagicMock(),
        allow_format_conversion=True,
    )

    assert result.success is True
    assert result.attempt_result is not None
    assert result.attempt_result.kind == AttemptKind.ASYNC_SUBMIT
    assert result.provider_task_id == "task_123"
    assert result.provider_id == "p1"
    assert result.candidate_index == 0


@pytest.mark.asyncio
async def test_task_service_execute_sync_passes_request_headers_and_body() -> None:
    db = MagicMock()
    svc = TaskService(db)
    sentinel_result = object()
    svc._execute_sync_unified = AsyncMock(  # type: ignore[method-assign]
        return_value=sentinel_result
    )

    request_headers = {"authorization": "Bearer test", "x-trace-id": "abc123"}
    request_body = {"model": "gpt-4o-mini", "messages": [{"role": "user", "content": "hi"}]}
    request_body_ref = {"body": request_body}

    result = await svc.execute(
        task_type="chat",
        task_mode=TaskMode.SYNC,
        api_format="openai:chat",
        model_name="gpt-4o-mini",
        user_api_key=MagicMock(id="u", user_id="user"),
        request_func=AsyncMock(),
        request_id="rid-sync",
        is_stream=True,
        request_headers=request_headers,
        request_body=request_body,
        request_body_ref=request_body_ref,
    )

    assert result is sentinel_result
    svc._execute_sync_unified.assert_awaited_once()  # type: ignore[attr-defined]
    kwargs = svc._execute_sync_unified.await_args.kwargs  # type: ignore[attr-defined, union-attr]
    assert kwargs["request_headers"] == request_headers
    assert kwargs["request_body"] == request_body
    assert kwargs["request_body_ref"] == request_body_ref


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("is_client_error", "retry_index", "max_retries_for_candidate", "expected_action"),
    [
        (True, 0, 1, "break"),
        (False, 0, 2, "continue"),
    ],
)
async def test_task_service_embedded_error_branch_applies_pool_health_policy(
    monkeypatch: pytest.MonkeyPatch,
    is_client_error: bool,
    retry_index: int,
    max_retries_for_candidate: int,
    expected_action: str,
) -> None:
    db = MagicMock()
    svc = TaskService(db)

    monkeypatch.setattr(
        task_service_module.RequestCandidateService, "mark_candidate_failed", MagicMock()
    )
    monkeypatch.setattr(
        "src.services.proxy_node.resolver.resolve_effective_proxy",
        lambda provider_proxy, key_proxy: provider_proxy or key_proxy,
    )
    monkeypatch.setattr("src.services.proxy_node.resolver.resolve_proxy_info", lambda _proxy: None)

    pool_on_error = AsyncMock()
    monkeypatch.setattr(svc, "_pool_on_error", pool_on_error)

    candidate = SimpleNamespace(
        provider=SimpleNamespace(id="p1", name="prov", proxy=None),
        endpoint=SimpleNamespace(id="e1"),
        key=SimpleNamespace(id="k1", proxy=None),
    )
    cause = EmbeddedErrorException(
        provider_name="prov",
        error_code=429,
        error_message="usage_limit_reached",
        error_status="RESOURCE_EXHAUSTED",
    )
    context = ExecutionContext(
        candidate_id="cid-1",
        candidate_index=0,
        provider_id="p1",
        endpoint_id="e1",
        key_id="k1",
        user_id=None,
        api_key_id=None,
        is_cached_user=False,
        elapsed_ms=12,
        concurrent_requests=3,
    )
    exec_err = ExecutionError(cause, context)
    classifier = SimpleNamespace(is_client_error=lambda _text: is_client_error)

    action = await svc._handle_candidate_error(
        exec_err=exec_err,
        candidate=candidate,
        candidate_record_id="cand-1",
        retry_index=retry_index,
        max_retries_for_candidate=max_retries_for_candidate,
        affinity_key="provider-test:p1",
        api_format="openai:chat",
        global_model_id="gpt-4o-mini",
        request_id="req-1",
        attempt=1,
        max_attempts=3,
        error_classifier=classifier,
    )

    assert action == expected_action
    pool_on_error.assert_awaited_once_with(candidate.provider, candidate.key, 429, cause)


@pytest.mark.asyncio
async def test_task_service_pool_on_error_uses_embedded_error_message_fallback(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = MagicMock()
    svc = TaskService(db)

    parsed_pool_cfg = object()
    apply_health_policy = AsyncMock()
    monkeypatch.setattr(
        "src.services.provider.pool.config.parse_pool_config", lambda _cfg: parsed_pool_cfg
    )
    monkeypatch.setattr(
        "src.services.provider.pool.health_policy.apply_health_policy",
        apply_health_policy,
    )

    provider = SimpleNamespace(id="p1", config={})
    key = SimpleNamespace(id="k1")
    cause = EmbeddedErrorException(
        provider_name="prov",
        error_code=429,
        error_message="usage_limit_reached",
    )

    await svc._pool_on_error(provider, key, 429, cause)

    apply_health_policy.assert_awaited_once()
    kwargs = apply_health_policy.await_args.kwargs
    assert kwargs["provider_id"] == "p1"
    assert kwargs["key_id"] == "k1"
    assert kwargs["status_code"] == 429
    assert kwargs["error_body"] == "usage_limit_reached"
    assert kwargs["response_headers"] == {}
    assert kwargs["config"] is parsed_pool_cfg
