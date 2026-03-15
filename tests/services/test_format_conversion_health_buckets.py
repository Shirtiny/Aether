from collections.abc import AsyncGenerator
from contextlib import asynccontextmanager
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from src.services.orchestration.error_classifier import ErrorClassifier
from src.services.request.executor import RequestExecutor


@asynccontextmanager
async def _noop_async_cm() -> AsyncGenerator[None]:
    yield


@pytest.mark.asyncio
async def test_executor_records_health_by_provider_format() -> None:
    db = MagicMock()

    concurrency_manager = MagicMock()
    concurrency_manager.get_current_concurrency = AsyncMock(return_value=(0, 0))
    concurrency_manager.get_key_rpm_count = AsyncMock(return_value=1)
    concurrency_manager.rpm_guard = MagicMock(return_value=_noop_async_cm())

    adaptive_manager = MagicMock()

    provider = MagicMock()
    provider.id = "p1"
    provider.name = "p1"

    endpoint = MagicMock()
    endpoint.id = "e1"
    endpoint.api_format = "openai:chat"
    endpoint.api_family = "openai"
    endpoint.endpoint_kind = "chat"

    key = MagicMock()
    key.id = "k1"
    key.api_key = "encrypted"
    key.rpm_limit = 10
    key.learned_rpm_limit = None
    key.cache_ttl_minutes = 0

    candidate = MagicMock()
    candidate.provider = provider
    candidate.endpoint = endpoint
    candidate.key = key
    candidate.is_cached = False

    async def request_func(
        _provider: Any, _endpoint: Any, _key: Any, _candidate: Any
    ) -> dict[str, bool]:
        return {"ok": True}

    with (
        patch("src.services.request.executor.RequestCandidateService.mark_candidate_started"),
        patch("src.services.request.executor.RequestCandidateService.mark_candidate_success"),
        patch("src.services.request.executor.get_adaptive_reservation_manager") as mock_res_mgr,
        patch("src.services.request.executor.get_health_monitor") as mock_get_health_monitor,
    ):
        mock_res_mgr.return_value.calculate_reservation.return_value = MagicMock(
            ratio=0.0, phase="stable", confidence=1.0
        )
        record_success = mock_get_health_monitor.return_value.record_success

        executor = RequestExecutor(
            db=db, concurrency_manager=concurrency_manager, adaptive_manager=adaptive_manager
        )
        await executor.execute(
            candidate=candidate,
            candidate_id="c1",
            candidate_index=0,
            user_api_key=MagicMock(user_id="u1", id="ak1"),
            request_func=request_func,
            request_id="r1",
            api_format="claude:chat",  # client_format
            model_name="m",
            is_stream=False,
        )

        record_success.assert_called()
        assert record_success.call_args.kwargs["api_format"] == "openai:chat"


@pytest.mark.asyncio
async def test_error_classifier_records_failure_by_provider_format() -> None:
    db = MagicMock()
    classifier = ErrorClassifier(db=db, cache_scheduler=None, adaptive_manager=MagicMock())

    provider = MagicMock()
    provider.name = "p1"

    endpoint = MagicMock()
    endpoint.id = "e1"
    endpoint.api_format = "openai:chat"
    endpoint.api_family = "openai"
    endpoint.endpoint_kind = "chat"

    key = MagicMock()
    key.id = "k1"

    with patch(
        "src.services.orchestration.error_handler.get_health_monitor"
    ) as mock_get_health_monitor:
        record_failure = mock_get_health_monitor.return_value.record_failure
        await classifier.handle_retriable_error(
            error=RuntimeError("boom"),
            provider=provider,
            endpoint=endpoint,
            key=key,
            affinity_key="aff",
            api_format="claude:chat",  # client_format
            global_model_id="gm1",
            captured_key_concurrent=None,
            elapsed_ms=None,
            request_id="r1",
            attempt=1,
            max_attempts=2,
        )

        record_failure.assert_called()
        assert record_failure.call_args.kwargs["api_format"] == "openai:chat"
