"""Multi-dimension pool scoring strategy."""

from __future__ import annotations

from typing import Any

import src.services.provider.pool.dimensions  # noqa: F401
from src.services.provider.pool.dimensions import get_preset_dimension, get_preset_names
from src.services.provider.pool.dimensions._helpers import rank_ascending, safe_float
from src.services.provider.pool.strategy import register_pool_strategy


def _normalize_mutex_group(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower()
    return normalized or None


def _get_preset_mutex_group(preset_name: str) -> str | None:
    # LRU is a built-in preset (not in registry) but shares the distribution mutex group.
    if preset_name == "lru":
        return "distribution_mode"
    dim = get_preset_dimension(preset_name)
    if dim is None:
        return None
    return _normalize_mutex_group(getattr(dim, "mutex_group", None))


def _normalize_presets_from_config(
    config: Any,
    *,
    provider_type: str | None = None,
) -> tuple[tuple[str, str | None], ...]:
    """Extract enabled (preset_name, mode) tuples from config.scheduling_presets.

    Supports both new SchedulingPreset objects and legacy string lists.
    Excludes ``lru`` from output (LRU is a final tie-breaker only).
    For mutex groups, enabled members inherit the group's first appearance index
    so the selected member keeps the group's visible priority slot.
    """

    raw = getattr(config, "scheduling_presets", ())
    if not isinstance(raw, (list, tuple)):
        return ()

    normalized_provider_type = str(provider_type or "").strip().lower()
    allowed = get_preset_names() | {"lru"}
    entries: list[tuple[int, str, bool, str | None]] = []
    seen: set[str] = set()
    for idx, item in enumerate(raw):
        preset_name: str | None = None
        enabled = True
        mode: str | None = None

        if hasattr(item, "preset"):
            preset_name = str(getattr(item, "preset", "")).strip().lower()
            enabled = bool(getattr(item, "enabled", True))
            raw_mode = getattr(item, "mode", None)
            if isinstance(raw_mode, str):
                mode = raw_mode.strip().lower() or None
        elif isinstance(item, str):
            preset_name = item.strip().lower()
        else:
            continue

        if not preset_name or preset_name not in allowed or preset_name in seen:
            continue
        seen.add(preset_name)
        entries.append((idx, preset_name, enabled, mode))

    # Codex 默认启用额度刷新优先维度（除非显式配置了 recent_refresh）。
    if (
        normalized_provider_type == "codex"
        and entries
        and "recent_refresh" not in {name for _idx, name, _enabled, _mode in entries}
        and "recent_refresh" in allowed
    ):
        entries.append((len(entries), "recent_refresh", True, None))

    if not entries:
        return ()

    group_anchor_index: dict[str, int] = {}
    for idx, preset_name, _enabled, _mode in entries:
        mutex_group = _get_preset_mutex_group(preset_name)
        if mutex_group and mutex_group not in group_anchor_index:
            group_anchor_index[mutex_group] = idx

    ordered_enabled: list[tuple[int, int, str, str | None]] = []
    group_enabled: dict[str, tuple[int, int, str, str | None]] = {}
    for idx, preset_name, enabled, mode in entries:
        if not enabled or preset_name == "lru":
            continue
        mutex_group = _get_preset_mutex_group(preset_name)
        if not mutex_group:
            ordered_enabled.append((idx, idx, preset_name, mode))
            continue

        anchor = group_anchor_index.get(mutex_group, idx)
        existing = group_enabled.get(mutex_group)
        if existing is None or idx < existing[1]:
            group_enabled[mutex_group] = (anchor, idx, preset_name, mode)

    ordered_enabled.extend(group_enabled.values())
    ordered_enabled.sort(key=lambda item: (item[0], item[1]))
    return tuple((preset_name, mode) for _anchor, _idx, preset_name, mode in ordered_enabled)


class MultiScoreStrategy:
    name = "multi_score"

    def compute_score(
        self,
        *,
        key_id: str,
        config: Any,
        context: dict[str, Any],
    ) -> float | None:
        mode = str(getattr(config, "scheduling_mode", "lru") or "lru").strip().lower()
        if mode != "multi_score":
            return None

        all_key_ids = [str(k) for k in (context.get("all_key_ids") or []) if str(k)]
        if not all_key_ids:
            return None

        lru_scores = context.get("lru_scores", {})
        if not isinstance(lru_scores, dict):
            lru_scores = {}
        latency_avgs = context.get("latency_avgs", {})
        if not isinstance(latency_avgs, dict):
            latency_avgs = {}
        health_scores = context.get("health_scores", {})
        if not isinstance(health_scores, dict):
            health_scores = {}
        cost_totals = context.get("cost_totals", {})
        if not isinstance(cost_totals, dict):
            cost_totals = {}
        keys_by_id = context.get("keys_by_id", {})
        if not isinstance(keys_by_id, dict):
            keys_by_id = {}

        presets = _normalize_presets_from_config(
            config,
            provider_type=context.get("provider_type"),
        )
        lru_enabled = bool(getattr(config, "lru_enabled", True))
        if presets:
            return self._compute_preset_score(
                key_id=key_id,
                all_key_ids=all_key_ids,
                presets=presets,
                lru_enabled=lru_enabled,
                lru_scores=lru_scores,
                keys_by_id=keys_by_id,
                context=context,
            )

        weights = getattr(config, "scoring_weights", None)
        w_lru = safe_float(getattr(weights, "lru", 0.3)) or 0.0
        w_latency = safe_float(getattr(weights, "latency", 0.25)) or 0.0
        w_health = safe_float(getattr(weights, "health", 0.2)) or 0.0
        w_cost = safe_float(getattr(weights, "cost_remaining", 0.25)) or 0.0

        lru_rank = rank_ascending(key_id, lru_scores, all_key_ids)
        latency_rank = rank_ascending(key_id, latency_avgs, all_key_ids)

        health_raw = safe_float(health_scores.get(key_id))
        if health_raw is None:
            health_raw = 1.0
        health_norm = 1.0 - max(0.0, min(health_raw, 1.0))

        cost_limit = getattr(config, "cost_limit_per_key_tokens", None)
        used = safe_float(cost_totals.get(key_id)) or 0.0
        if cost_limit is None or int(cost_limit) <= 0:
            cost_norm = 0.0
        else:
            cost_norm = max(0.0, min(used / float(cost_limit), 1.0))

        return (
            w_lru * lru_rank
            + w_latency * latency_rank
            + w_health * health_norm
            + w_cost * cost_norm
        )

    def _compute_preset_score(
        self,
        *,
        key_id: str,
        all_key_ids: list[str],
        presets: tuple[tuple[str, str | None], ...],
        lru_enabled: bool,
        lru_scores: dict[str, Any],
        keys_by_id: dict[str, Any],
        context: dict[str, Any],
    ) -> float:
        cache_signature = (tuple(all_key_ids), presets, bool(lru_enabled))
        cache = context.get("_preset_hard_order_cache")
        if (
            isinstance(cache, dict)
            and cache.get("signature") == cache_signature
            and isinstance(cache.get("ranks"), dict)
        ):
            cached_rank = safe_float(cache["ranks"].get(key_id))
            if cached_rank is not None:
                return max(0.0, min(cached_rank, 1.0))

        # Hard-priority semantics:
        # 1) Compare by preset[0] metric first;
        # 2) only if tied, compare preset[1], preset[2], ...
        # 3) if all preset metrics tie and LRU is enabled, use LRU as final tiebreak.
        metric_vectors: dict[str, tuple[float, ...]] = {}
        for kid in all_key_ids:
            vector_parts: list[float] = []
            for preset_name, mode in presets:
                metric = 0.5
                dim = get_preset_dimension(preset_name)
                if dim is not None:
                    metric = dim.compute_metric(
                        key_id=kid,
                        all_key_ids=all_key_ids,
                        keys_by_id=keys_by_id,
                        lru_scores=lru_scores,
                        context=context,
                        mode=mode,
                    )
                metric_value = safe_float(metric)
                vector_parts.append(
                    max(0.0, min(metric_value, 1.0)) if metric_value is not None else 0.5
                )

            if lru_enabled:
                vector_parts.append(rank_ascending(kid, lru_scores, all_key_ids))

            metric_vectors[kid] = tuple(vector_parts)

        decorated = [
            (metric_vectors.get(kid, (0.5,)), idx, kid) for idx, kid in enumerate(all_key_ids)
        ]
        decorated.sort(key=lambda item: (item[0], item[1]))

        total = len(decorated)
        ranks: dict[str, float] = {}
        for rank_idx, (_vec, _idx, kid) in enumerate(decorated):
            ranks[kid] = 0.0 if total <= 1 else rank_idx / float(total - 1)

        context["_preset_hard_order_cache"] = {
            "signature": cache_signature,
            "ranks": ranks,
        }
        rank = safe_float(ranks.get(key_id))
        if rank is None:
            return 0.5
        return max(0.0, min(rank, 1.0))


register_pool_strategy("multi_score", MultiScoreStrategy())


__all__ = [
    "MultiScoreStrategy",
]
