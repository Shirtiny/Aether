"""Tests for pool scheduling trace dataclasses."""

from __future__ import annotations

from src.services.provider.pool.trace import PoolCandidateTrace, PoolSchedulingTrace

# ---------------------------------------------------------------------------
# PoolCandidateTrace.to_extra_data
# ---------------------------------------------------------------------------


class TestPoolCandidateTraceExtraData:
    def test_selected_sticky(self) -> None:
        ct = PoolCandidateTrace(key_id="k1", reason="sticky", sticky_hit=True)
        data = ct.to_extra_data()
        assert "pool_selection" in data
        assert data["pool_selection"]["reason"] == "sticky"
        assert data["pool_selection"]["sticky_hit"] is True

    def test_selected_lru_with_cost(self) -> None:
        ct = PoolCandidateTrace(
            key_id="k2",
            reason="lru",
            lru_score=1234.5,
            cost_window_usage=500,
            cost_limit=1000,
        )
        data = ct.to_extra_data()
        sel = data["pool_selection"]
        assert sel["reason"] == "lru"
        assert sel["lru_score"] == 1234.5
        assert sel["cost_window_usage"] == 500
        assert sel["cost_limit"] == 1000
        assert "sticky_hit" not in sel

    def test_selected_with_soft_threshold(self) -> None:
        ct = PoolCandidateTrace(
            key_id="k3",
            reason="lru",
            cost_window_usage=850,
            cost_limit=1000,
            cost_soft_threshold=True,
        )
        data = ct.to_extra_data()
        assert data["pool_selection"]["cost_soft_threshold"] is True

    def test_selected_random_minimal(self) -> None:
        ct = PoolCandidateTrace(key_id="k4", reason="random")
        data = ct.to_extra_data()
        sel = data["pool_selection"]
        assert sel["reason"] == "random"
        assert sel["scoring_mode"] == "lru"

    def test_selected_multi_score_fields(self) -> None:
        ct = PoolCandidateTrace(
            key_id="k4b",
            reason="multi_score",
            scoring_mode="multi_score",
            latency_avg_ms=245.7,
            health_score=0.82,
            composite_score=0.372156,
        )
        data = ct.to_extra_data()
        sel = data["pool_selection"]
        assert sel["reason"] == "multi_score"
        assert sel["scoring_mode"] == "multi_score"
        assert sel["latency_avg_ms"] == 245.7
        assert sel["health_score"] == 0.82
        assert sel["composite_score"] == 0.372156

    def test_skipped_cooldown(self) -> None:
        ct = PoolCandidateTrace(
            key_id="k5",
            skipped=True,
            skip_type="cooldown",
            cooldown_reason="rate_limited_429",
            cooldown_ttl=120,
        )
        data = ct.to_extra_data()
        assert "pool_skip" in data
        skip = data["pool_skip"]
        assert skip["type"] == "cooldown"
        assert skip["cooldown_reason"] == "rate_limited_429"
        assert skip["cooldown_ttl"] == 120

    def test_skipped_cost_exhausted(self) -> None:
        ct = PoolCandidateTrace(
            key_id="k6",
            skipped=True,
            skip_type="cost_exhausted",
            cost_window_usage=2000,
        )
        data = ct.to_extra_data()
        skip = data["pool_skip"]
        assert skip["type"] == "cost_exhausted"
        assert skip["cost_window_usage"] == 2000
        assert "cooldown_reason" not in skip

    def test_skipped_account_blocked(self) -> None:
        ct = PoolCandidateTrace(
            key_id="k6b",
            skipped=True,
            skip_type="account_blocked",
            account_block_code="account_banned",
            account_block_label="账号封禁",
            account_block_reason="account suspended",
        )
        data = ct.to_extra_data()
        skip = data["pool_skip"]
        assert skip["type"] == "account_blocked"
        assert skip["account_block_code"] == "account_banned"
        assert skip["account_block_label"] == "账号封禁"
        assert skip["account_block_reason"] == "account suspended"

    def test_skipped_minimal(self) -> None:
        ct = PoolCandidateTrace(key_id="k7", skipped=True, skip_type="upstream")
        data = ct.to_extra_data()
        skip = data["pool_skip"]
        assert skip["type"] == "upstream"
        assert skip["scoring_mode"] == "lru"


# ---------------------------------------------------------------------------
# PoolSchedulingTrace.build_summary
# ---------------------------------------------------------------------------


class TestPoolSchedulingTraceSummary:
    def test_basic_summary(self) -> None:
        trace = PoolSchedulingTrace(provider_id="prov-1", total_keys=5)
        trace.candidate_traces = {
            "k1": PoolCandidateTrace(key_id="k1", reason="sticky", sticky_hit=True),
            "k2": PoolCandidateTrace(key_id="k2", reason="lru"),
            "k3": PoolCandidateTrace(
                key_id="k3", skipped=True, skip_type="cooldown", cooldown_reason="429"
            ),
            "k4": PoolCandidateTrace(key_id="k4", skipped=True, skip_type="cost_exhausted"),
            "k5": PoolCandidateTrace(
                key_id="k5", skipped=True, skip_type="cooldown", cooldown_reason="500"
            ),
        }
        trace.sticky_session_used = True

        summary = trace.build_summary(success_key_id="k1")

        assert summary["enabled"] is True
        assert summary["total_keys"] == 5
        assert summary["attempted"] == 2
        assert summary["skipped_cooldown"] == 2
        assert summary["skipped_cost"] == 1
        assert summary["skipped_account_blocked"] == 0
        assert summary["sticky_session"] is True
        assert summary["success_key_id"] == "k1"[:8]
        assert summary["success_reason"] == "sticky"

    def test_summary_no_success_key(self) -> None:
        trace = PoolSchedulingTrace(provider_id="prov-2", total_keys=2)
        trace.candidate_traces = {
            "k1": PoolCandidateTrace(key_id="k1", reason="random"),
            "k2": PoolCandidateTrace(key_id="k2", reason="lru"),
        }

        summary = trace.build_summary(success_key_id=None)

        assert summary["attempted"] == 2
        assert summary["skipped_cooldown"] == 0
        assert summary["skipped_cost"] == 0
        assert summary["skipped_account_blocked"] == 0
        assert "success_key_id" not in summary
        assert "success_reason" not in summary

    def test_summary_all_skipped(self) -> None:
        trace = PoolSchedulingTrace(provider_id="prov-3", total_keys=2)
        trace.candidate_traces = {
            "k1": PoolCandidateTrace(key_id="k1", skipped=True, skip_type="cooldown"),
            "k2": PoolCandidateTrace(key_id="k2", skipped=True, skip_type="cost_exhausted"),
        }

        summary = trace.build_summary()
        assert summary["attempted"] == 0
        assert summary["skipped_cooldown"] == 1
        assert summary["skipped_cost"] == 1
        assert summary["skipped_account_blocked"] == 0

    def test_summary_with_account_blocked(self) -> None:
        trace = PoolSchedulingTrace(provider_id="prov-4", total_keys=2)
        trace.candidate_traces = {
            "k1": PoolCandidateTrace(key_id="k1", skipped=True, skip_type="account_blocked"),
            "k2": PoolCandidateTrace(key_id="k2", reason="lru"),
        }

        summary = trace.build_summary(success_key_id="k2")
        assert summary["attempted"] == 1
        assert summary["skipped_account_blocked"] == 1
