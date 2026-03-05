"""In-process pool health score cache.

This cache avoids recomputing per-key health aggregation on every request.
It does not replace persistent health storage; source data still comes from
``ProviderAPIKey.health_by_format`` carried on key objects.
"""

from __future__ import annotations

import threading
import time
from typing import Any

_TTL_SECONDS = 30.0
_LOCK = threading.Lock()
_CACHE: dict[str, tuple[float, dict[str, float]]] = {}


def aggregate_health_score(health_by_format: Any) -> float:
    """Aggregate health score from ``health_by_format`` (lower-bound strategy)."""
    if not isinstance(health_by_format, dict) or not health_by_format:
        return 1.0
    scores: list[float] = []
    for item in health_by_format.values():
        if not isinstance(item, dict):
            continue
        try:
            score = float(item.get("health_score") or 1.0)
        except (TypeError, ValueError):
            score = 1.0
        scores.append(max(0.0, min(score, 1.0)))
    if not scores:
        return 1.0
    return min(scores)


def get_health_scores(provider_id: str, keys: list[Any]) -> dict[str, float]:
    """Return key health scores with per-provider TTL cache.

    Uses incremental merge: if the cache is still valid but missing some keys,
    only the missing keys are computed and merged into the existing cache entry.
    """
    now = time.monotonic()
    keys_by_id: dict[str, Any] = {}
    for k in keys:
        kid = str(getattr(k, "id", "") or "")
        if kid:
            keys_by_id[kid] = k
    if not keys_by_id:
        return {}

    with _LOCK:
        cached = _CACHE.get(provider_id)
        if cached is not None:
            expires_at, payload = cached
            if now < expires_at:
                missing_ids = [kid for kid in keys_by_id if kid not in payload]
                if not missing_ids:
                    return {kid: payload[kid] for kid in keys_by_id}
                # Compute only for missing keys, merge into existing cache
                for kid in missing_ids:
                    payload[kid] = aggregate_health_score(
                        getattr(keys_by_id[kid], "health_by_format", None)
                    )
                return {kid: payload[kid] for kid in keys_by_id}

    fresh: dict[str, float] = {}
    for kid, key in keys_by_id.items():
        fresh[kid] = aggregate_health_score(getattr(key, "health_by_format", None))

    with _LOCK:
        _CACHE[provider_id] = (now + _TTL_SECONDS, fresh)
    return dict(fresh)


def invalidate_provider_health_scores(provider_id: str) -> None:
    """Invalidate health-score cache for one provider."""
    with _LOCK:
        _CACHE.pop(provider_id, None)


def _clear_cache_for_tests() -> None:
    with _LOCK:
        _CACHE.clear()


__all__ = [
    "aggregate_health_score",
    "get_health_scores",
    "invalidate_provider_health_scores",
]
