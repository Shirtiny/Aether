"""Tests for pool health cache helpers."""

from __future__ import annotations

from types import SimpleNamespace

from src.services.provider.pool import health_cache


def setup_function() -> None:
    health_cache._clear_cache_for_tests()


def teardown_function() -> None:
    health_cache._clear_cache_for_tests()


def test_aggregate_health_score_uses_lowest_format_score() -> None:
    score = health_cache.aggregate_health_score(
        {
            "openai:chat": {"health_score": 0.92},
            "openai:responses": {"health_score": 0.61},
        }
    )
    assert score == 0.61


def test_get_health_scores_uses_cache_for_same_provider() -> None:
    key = SimpleNamespace(id="k1", health_by_format={"f1": {"health_score": 0.7}})
    first = health_cache.get_health_scores("p1", [key])
    assert first["k1"] == 0.7

    key.health_by_format = {"f1": {"health_score": 0.2}}
    second = health_cache.get_health_scores("p1", [key])
    assert second["k1"] == 0.7


def test_get_health_scores_merges_missing_keys_into_cache() -> None:
    k1 = SimpleNamespace(id="k1", health_by_format={"f1": {"health_score": 0.7}})
    first = health_cache.get_health_scores("p1", [k1])
    assert first == {"k1": 0.7}

    # Request with a new key k2 -- k1 should come from cache, k2 freshly computed
    k1_stale = SimpleNamespace(id="k1", health_by_format={"f1": {"health_score": 0.1}})
    k2 = SimpleNamespace(id="k2", health_by_format={"f1": {"health_score": 0.5}})
    second = health_cache.get_health_scores("p1", [k1_stale, k2])
    assert second["k1"] == 0.7  # cached, not recomputed
    assert second["k2"] == 0.5  # freshly computed


def test_invalidate_provider_health_scores_clears_cache_entry() -> None:
    key = SimpleNamespace(id="k1", health_by_format={"f1": {"health_score": 0.8}})
    _ = health_cache.get_health_scores("p1", [key])
    health_cache.invalidate_provider_health_scores("p1")

    key.health_by_format = {"f1": {"health_score": 0.3}}
    refreshed = health_cache.get_health_scores("p1", [key])
    assert refreshed["k1"] == 0.3
