from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import MagicMock

from src.services.request.candidate import RequestCandidateService


def _build_db_with_candidate(candidate: SimpleNamespace) -> MagicMock:
    query = MagicMock()
    query.filter.return_value.first.return_value = candidate

    db = MagicMock()
    db.query.return_value = query
    db.info = {"managed_by_middleware": True}
    return db


def test_mark_candidate_started_flushes_without_immediate_commit() -> None:
    candidate = SimpleNamespace(status="available", started_at=None)
    db = _build_db_with_candidate(candidate)

    RequestCandidateService.mark_candidate_started(db, "candidate-1")

    assert candidate.status == "pending"
    assert candidate.started_at is not None
    db.flush.assert_called_once()
    db.commit.assert_not_called()


def test_mark_candidate_streaming_flushes_without_immediate_commit() -> None:
    candidate = SimpleNamespace(status="pending", concurrent_requests=None)
    db = _build_db_with_candidate(candidate)

    RequestCandidateService.mark_candidate_streaming(db, "candidate-1", concurrent_requests=3)

    assert candidate.status == "streaming"
    assert candidate.concurrent_requests == 3
    db.flush.assert_called_once()
    db.commit.assert_not_called()
