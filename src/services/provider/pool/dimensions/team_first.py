"""team_first preset dimension."""

from __future__ import annotations

from typing import Any

from ._helpers import extract_plan_type, plan_priority_score, rank_ascending
from .registry import PresetDimensionBase, register_preset_dimension


class TeamFirstDimension(PresetDimensionBase):
    @property
    def name(self) -> str:
        return "team_first"

    @property
    def label(self) -> str:
        return "Team 优先"

    @property
    def description(self) -> str:
        return "优先消耗 Team 账号（依赖 plan_type）"

    @property
    def evidence_hint(self) -> str | None:
        return "依据 plan_type（Team 账号优先调度）"

    @property
    def providers(self) -> tuple[str, ...]:
        return ("codex", "kiro")

    def compute_metric(
        self,
        *,
        key_id: str,
        all_key_ids: list[str],
        keys_by_id: dict[str, Any],
        lru_scores: dict[str, Any],
        context: dict[str, Any],
        mode: str | None,
    ) -> float:
        plan_scores = {
            kid: plan_priority_score(extract_plan_type(keys_by_id.get(kid)), "team_only")
            for kid in all_key_ids
        }
        if len(set(plan_scores.values())) <= 1:
            return rank_ascending(key_id, lru_scores, all_key_ids)
        return rank_ascending(key_id, plan_scores, all_key_ids)


register_preset_dimension(TeamFirstDimension())
