"""Shared helpers for pool preset dimensions."""

from __future__ import annotations

import math
import time
from typing import Any


def safe_float(value: Any) -> float | None:
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return None
    if math.isnan(parsed) or math.isinf(parsed):
        return None
    return parsed


def safe_metadata(key_obj: Any) -> dict[str, Any]:
    raw = getattr(key_obj, "upstream_metadata", None)
    return raw if isinstance(raw, dict) else {}


def normalize_plan(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower()
    return normalized or None


def rank_ascending(key_id: str, scores: dict[str, float], all_ids: list[str]) -> float:
    """Rank score within all IDs; lower value means better rank."""

    if not all_ids:
        return 0.0

    valid_count = sum(1 for kid in all_ids if safe_float(scores.get(kid)) is not None)
    if valid_count <= 0:
        return 0.5

    decorated: list[tuple[int, float, int, str]] = []
    for idx, kid in enumerate(all_ids):
        score_raw = safe_float(scores.get(kid))
        if score_raw is None:
            decorated.append((1, float("inf"), idx, kid))
        else:
            decorated.append((0, score_raw, idx, kid))

    decorated.sort(key=lambda item: (item[0], item[1], item[2]))
    rank_idx = 0
    for idx, (_missing, _value, _order, kid) in enumerate(decorated):
        if kid == key_id:
            rank_idx = idx
            break

    n = len(all_ids)
    if n <= 1:
        return 0.0
    return rank_idx / float(n - 1)


def rank_descending(key_id: str, scores: dict[str, float], all_ids: list[str]) -> float:
    """Rank score within all IDs; higher value means better rank."""

    if not all_ids:
        return 0.0

    valid_count = sum(1 for kid in all_ids if safe_float(scores.get(kid)) is not None)
    if valid_count <= 0:
        return 0.5

    decorated: list[tuple[int, float, int, str]] = []
    for idx, kid in enumerate(all_ids):
        score_raw = safe_float(scores.get(kid))
        if score_raw is None:
            decorated.append((1, float("inf"), idx, kid))
        else:
            # 排序时取负值，使分值越大排名越靠前（rank 越小）
            decorated.append((0, -score_raw, idx, kid))

    decorated.sort(key=lambda item: (item[0], item[1], item[2]))
    rank_idx = 0
    for idx, (_missing, _value, _order, kid) in enumerate(decorated):
        if kid == key_id:
            rank_idx = idx
            break

    n = len(all_ids)
    if n <= 1:
        return 0.0
    return rank_idx / float(n - 1)


def extract_plan_type(key_obj: Any) -> str | None:
    direct = normalize_plan(getattr(key_obj, "oauth_plan_type", None))
    if direct:
        return direct

    metadata = safe_metadata(key_obj)
    codex = metadata.get("codex")
    if isinstance(codex, dict):
        codex_plan = normalize_plan(codex.get("plan_type"))
        if codex_plan:
            return codex_plan

    kiro = metadata.get("kiro")
    if isinstance(kiro, dict):
        subscription_title = normalize_plan(kiro.get("subscription_title"))
        if subscription_title:
            # Normalize common Kiro labels into free/team buckets used by free_team_first.
            if "team" in subscription_title:
                return "team"
            if "free" in subscription_title:
                return "free"
            if "pro" in subscription_title:
                return "pro"
            if "plus" in subscription_title:
                return "plus"
            return subscription_title

    return None


def extract_reset_seconds(key_obj: Any) -> float | None:
    metadata = safe_metadata(key_obj)
    candidates: list[float] = []

    codex = metadata.get("codex")
    if isinstance(codex, dict):
        for field in ("secondary_reset_seconds", "primary_reset_seconds"):
            parsed = safe_float(codex.get(field))
            if parsed is None or parsed < 0:
                continue
            candidates.append(parsed)

    kiro = metadata.get("kiro")
    if isinstance(kiro, dict):
        next_reset_at = safe_float(kiro.get("next_reset_at"))
        if next_reset_at is not None and next_reset_at > 0:
            candidates.append(max(0.0, next_reset_at - time.time()))

    if not candidates:
        return None
    return min(candidates)


def extract_usage_ratio(key_obj: Any) -> float | None:
    metadata = safe_metadata(key_obj)

    codex = metadata.get("codex")
    if isinstance(codex, dict):
        codex_values: list[float] = []
        for field in ("primary_used_percent", "secondary_used_percent"):
            parsed = safe_float(codex.get(field))
            if parsed is None:
                continue
            codex_values.append(max(0.0, min(parsed, 100.0)) / 100.0)
        if codex_values:
            return sum(codex_values) / len(codex_values)

    kiro = metadata.get("kiro")
    if isinstance(kiro, dict):
        parsed = safe_float(kiro.get("usage_percentage"))
        if parsed is not None:
            return max(0.0, min(parsed, 100.0)) / 100.0

    antigravity = metadata.get("antigravity")
    if isinstance(antigravity, dict):
        quota_by_model = antigravity.get("quota_by_model")
        if isinstance(quota_by_model, dict):
            usage_values: list[float] = []
            for model_info in quota_by_model.values():
                if not isinstance(model_info, dict):
                    continue
                used_percent = safe_float(model_info.get("used_percent"))
                if used_percent is None:
                    remaining_fraction = safe_float(model_info.get("remaining_fraction"))
                    if remaining_fraction is not None:
                        used_percent = (1.0 - remaining_fraction) * 100.0
                if used_percent is None:
                    continue
                usage_values.append(max(0.0, min(used_percent, 100.0)) / 100.0)
            if usage_values:
                return sum(usage_values) / len(usage_values)

    return None


def extract_internal_priority(key_obj: Any) -> int:
    raw = getattr(key_obj, "internal_priority", None)
    parsed = safe_float(raw)
    if parsed is None:
        return 999999
    return max(0, int(parsed))


def extract_health_score(key_obj: Any) -> float | None:
    direct = safe_float(getattr(key_obj, "health_score", None))
    if direct is not None:
        return max(0.0, min(direct, 1.0))

    health_by_format = getattr(key_obj, "health_by_format", None)
    if not isinstance(health_by_format, dict) or not health_by_format:
        return None

    scores: list[float] = []
    for payload in health_by_format.values():
        if not isinstance(payload, dict):
            continue
        score = safe_float(payload.get("health_score"))
        if score is None:
            continue
        scores.append(max(0.0, min(score, 1.0)))

    if not scores:
        return None
    return min(scores)


def plan_priority_score(plan_type: str | None, mode: str | None = None) -> float:
    """Score a key based on plan type and scheduling mode.

    Lower score = higher priority.
    """

    effective_mode = (mode or "both").strip().lower()
    if effective_mode == "free_only":
        if plan_type == "free":
            return 0.0
        if plan_type == "team":
            return 0.5
    elif effective_mode == "team_only":
        if plan_type == "team":
            return 0.0
        if plan_type == "free":
            return 0.5
    elif effective_mode == "plus_only":
        if plan_type in {"plus", "pro"}:
            return 0.0
        if plan_type in {"enterprise", "business"}:
            return 0.3
    else:
        # "both" or unrecognized -> original behavior
        if plan_type in {"free", "team"}:
            return 0.0
    if plan_type in {"enterprise", "business"}:
        return 0.2
    if plan_type in {"plus", "pro"}:
        return 0.6
    if plan_type:
        return 0.7
    return 0.8


__all__ = [
    "extract_health_score",
    "extract_internal_priority",
    "extract_plan_type",
    "extract_reset_seconds",
    "extract_usage_ratio",
    "normalize_plan",
    "plan_priority_score",
    "rank_ascending",
    "rank_descending",
    "safe_float",
    "safe_metadata",
]
