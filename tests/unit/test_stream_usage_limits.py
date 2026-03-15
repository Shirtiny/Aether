from __future__ import annotations

import pytest

from src.services.usage.stream import _get_response_chunks_max_size


def test_response_chunks_max_size_default(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("RESPONSE_CHUNKS_MAX_SIZE_MB", raising=False)
    assert _get_response_chunks_max_size() == 2 * 1024 * 1024


def test_response_chunks_max_size_respects_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("RESPONSE_CHUNKS_MAX_SIZE_MB", "2")
    assert _get_response_chunks_max_size() == 2 * 1024 * 1024


def test_response_chunks_max_size_invalid_env_fallback(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("RESPONSE_CHUNKS_MAX_SIZE_MB", "bad")
    assert _get_response_chunks_max_size() == 2 * 1024 * 1024
