"""Pool scheduling trace -- per-candidate decision records.

Collects scheduling decisions made during pool-level candidate selection
without adding any extra Redis round-trips.  Trace data is later written
to ``RequestCandidate.extra_data`` and ``Usage.request_metadata``.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(slots=True)
class PoolCandidateTrace:
    """Single candidate scheduling decision in a pool context."""

    key_id: str
    reason: str = ""  # sticky / lru / random / tiebreak
    sticky_hit: bool = False
    lru_score: float = 0.0
    cost_window_usage: int = 0
    cost_limit: int | None = None
    cost_soft_threshold: bool = False
    skipped: bool = False
    skip_type: str | None = None  # cooldown / cost_exhausted / account_blocked / upstream
    cooldown_reason: str | None = None
    cooldown_ttl: int | None = None
    account_block_code: str | None = None
    account_block_label: str | None = None
    account_block_reason: str | None = None
    latency_avg_ms: float = 0.0
    health_score: float = 1.0
    composite_score: float = 0.0
    scoring_mode: str = "lru"

    def to_extra_data(self) -> dict[str, Any]:
        """Build dict to merge into ``RequestCandidate.extra_data``."""
        if self.skipped:
            skip_info: dict[str, Any] = {"type": self.skip_type}
            if self.cooldown_reason is not None:
                skip_info["cooldown_reason"] = self.cooldown_reason
            if self.cooldown_ttl is not None:
                skip_info["cooldown_ttl"] = self.cooldown_ttl
            if self.account_block_code is not None:
                skip_info["account_block_code"] = self.account_block_code
            if self.account_block_label is not None:
                skip_info["account_block_label"] = self.account_block_label
            if self.account_block_reason is not None:
                skip_info["account_block_reason"] = self.account_block_reason
            if self.cost_window_usage:
                skip_info["cost_window_usage"] = self.cost_window_usage
            if self.scoring_mode:
                skip_info["scoring_mode"] = self.scoring_mode
            return {"pool_skip": skip_info}

        sel: dict[str, Any] = {"reason": self.reason}
        if self.sticky_hit:
            sel["sticky_hit"] = True
        if self.lru_score:
            sel["lru_score"] = self.lru_score
        if self.cost_window_usage:
            sel["cost_window_usage"] = self.cost_window_usage
        if self.cost_limit is not None:
            sel["cost_limit"] = self.cost_limit
        if self.cost_soft_threshold:
            sel["cost_soft_threshold"] = True
        if self.latency_avg_ms > 0:
            sel["latency_avg_ms"] = round(self.latency_avg_ms, 2)
        if self.health_score < 1.0:
            sel["health_score"] = round(self.health_score, 4)
        if self.reason == "multi_score":
            sel["composite_score"] = round(self.composite_score, 6)
        if self.scoring_mode:
            sel["scoring_mode"] = self.scoring_mode
        return {"pool_selection": sel}


@dataclass(slots=True)
class PoolSchedulingTrace:
    """Aggregated scheduling trace for one pool-provider dispatch."""

    provider_id: str
    total_keys: int = 0
    sticky_session_used: bool = False
    session_uuid: str | None = None
    candidate_traces: dict[str, PoolCandidateTrace] = field(default_factory=dict)

    def build_summary(
        self,
        success_key_id: str | None = None,
        *,
        attempted_key_ids: set[str] | None = None,
    ) -> dict[str, Any]:
        """Build compact dict for ``Usage.request_metadata["pool_summary"]``."""
        skipped_cooldown = 0
        skipped_cost = 0
        skipped_account_blocked = 0
        attempted = 0
        for t in self.candidate_traces.values():
            if t.skipped:
                if t.skip_type == "cooldown":
                    skipped_cooldown += 1
                elif t.skip_type == "cost_exhausted":
                    skipped_cost += 1
                elif t.skip_type == "account_blocked":
                    skipped_account_blocked += 1

        if attempted_key_ids is None:
            # Backward-compatible behavior: count all schedulable keys.
            attempted = sum(1 for t in self.candidate_traces.values() if not t.skipped)
        else:
            # Preferred behavior: count only keys that were actually executed.
            attempted = sum(
                1
                for kid in attempted_key_ids
                if kid in self.candidate_traces and not self.candidate_traces[kid].skipped
            )

        success_reason: str | None = None
        if success_key_id and success_key_id in self.candidate_traces:
            success_reason = self.candidate_traces[success_key_id].reason

        summary: dict[str, Any] = {
            "enabled": True,
            "total_keys": self.total_keys,
            "attempted": attempted,
            "skipped_cooldown": skipped_cooldown,
            "skipped_cost": skipped_cost,
            "skipped_account_blocked": skipped_account_blocked,
            "sticky_session": self.sticky_session_used,
        }
        if success_key_id:
            summary["success_key_id"] = success_key_id[:8]
        if success_reason:
            summary["success_reason"] = success_reason
        return summary
