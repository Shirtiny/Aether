from __future__ import annotations

import asyncio
from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import MagicMock

from src.api.admin.monitoring.trace import AdminGetRequestTraceAdapter
from src.services.request.candidate import RequestCandidateService

_STARTED_STATUSES = {"pending", "streaming", "success", "failed", "cancelled"}


def _candidate(
    *, status: str, latency_ms: int | None = None, started_at: datetime | None = ...  # type: ignore[assignment]
) -> SimpleNamespace:
    now = datetime.now(timezone.utc)
    resolved_started_at = (
        (now if status in _STARTED_STATUSES else None) if started_at is ... else started_at
    )
    return SimpleNamespace(
        id=f"cand-{status}",
        request_id="req-1",
        candidate_index=0,
        retry_index=0,
        provider_id=None,
        endpoint_id=None,
        key_id=None,
        required_capabilities=None,
        status=status,
        skip_reason=None,
        is_cached=False,
        status_code=None,
        error_type=None,
        error_message=None,
        latency_ms=latency_ms,
        concurrent_requests=None,
        extra_data=None,
        created_at=now,
        started_at=resolved_started_at,
        finished_at=None,
    )


def _context() -> SimpleNamespace:
    return SimpleNamespace(
        db=MagicMock(),
        add_audit_metadata=lambda **_: None,
    )


def test_trace_returns_all_candidates_including_unused(monkeypatch: object) -> None:
    candidates = [
        _candidate(status="available"),
        _candidate(status="unused"),
        _candidate(status="failed", latency_ms=123),
    ]
    monkeypatch.setattr(
        RequestCandidateService,
        "get_candidates_by_request_id",
        lambda _db, _request_id: candidates,
    )

    adapter = AdminGetRequestTraceAdapter(request_id="req-1")
    response = asyncio.run(adapter.handle(_context()))

    assert response.total_candidates == 3
    assert len(response.candidates) == 3
    assert {c.status for c in response.candidates} == {"available", "unused", "failed"}
    assert response.total_latency_ms == 123


def test_trace_returns_unattempted_candidates(monkeypatch: object) -> None:
    candidates = [
        _candidate(status="available"),
        _candidate(status="unused"),
    ]
    monkeypatch.setattr(
        RequestCandidateService,
        "get_candidates_by_request_id",
        lambda _db, _request_id: candidates,
    )

    adapter = AdminGetRequestTraceAdapter(request_id="req-1")
    response = asyncio.run(adapter.handle(_context()))

    assert response.total_candidates == 2
    assert len(response.candidates) == 2
    assert {c.status for c in response.candidates} == {"available", "unused"}


def test_trace_attempted_only_filters_unattempted_candidates(monkeypatch: object) -> None:
    candidates = [
        _candidate(status="available"),
        _candidate(status="unused"),
        _candidate(status="skipped"),
        _candidate(status="failed", latency_ms=123),
        _candidate(status="success", latency_ms=456),
    ]
    monkeypatch.setattr(
        RequestCandidateService,
        "get_candidates_by_request_id",
        lambda _db, _request_id: candidates,
    )

    adapter = AdminGetRequestTraceAdapter(request_id="req-1", attempted_only=True)
    response = asyncio.run(adapter.handle(_context()))

    assert response.total_candidates == 2
    assert len(response.candidates) == 2
    assert {c.status for c in response.candidates} == {"failed", "success"}
    assert response.total_latency_ms == 579


def test_trace_attempted_only_excludes_pending_without_started_at(monkeypatch: object) -> None:
    candidates = [
        _candidate(status="available"),
        _candidate(status="pending", started_at=None),
        _candidate(status="pending"),
    ]
    monkeypatch.setattr(
        RequestCandidateService,
        "get_candidates_by_request_id",
        lambda _db, _request_id: candidates,
    )

    adapter = AdminGetRequestTraceAdapter(request_id="req-1", attempted_only=True)
    response = asyncio.run(adapter.handle(_context()))

    assert response.total_candidates == 1
    assert len(response.candidates) == 1
    assert response.candidates[0].status == "pending"
