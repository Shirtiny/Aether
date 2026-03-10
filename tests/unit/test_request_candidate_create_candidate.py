from __future__ import annotations

from unittest.mock import MagicMock

from src.services.request.candidate import RequestCandidateService


def test_create_candidate_persists_snapshot_fields() -> None:
    db = MagicMock()

    candidate = RequestCandidateService.create_candidate(
        db=db,
        request_id="req-1",
        candidate_index=0,
        retry_index=1,
        user_id="user-1",
        api_key_id="key-1",
        username="alice",
        api_key_name="Primary Key",
        provider_id="provider-1",
        endpoint_id="endpoint-1",
        key_id="pool-key-1",
        status="available",
    )

    assert candidate.username == "alice"
    assert candidate.api_key_name == "Primary Key"
    db.add.assert_called_once_with(candidate)
    db.flush.assert_called_once()
