"""Preset dimension registry for pool multi-score scheduling."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from threading import RLock
from typing import Any


@dataclass(frozen=True, slots=True)
class PresetDimensionMeta:
    """Serializable metadata for one preset dimension."""

    name: str
    label: str
    description: str
    providers: tuple[str, ...]
    modes: tuple[str, ...] | None
    default_mode: str | None
    mutex_group: str | None
    evidence_hint: str | None


class PresetDimensionBase(ABC):
    """Base class of one pool scheduling preset dimension."""

    @property
    @abstractmethod
    def name(self) -> str:
        """Stable preset key, e.g. ``free_team_first``."""

    @property
    @abstractmethod
    def label(self) -> str:
        """User-facing label."""

    @property
    @abstractmethod
    def description(self) -> str:
        """User-facing description."""

    @property
    def providers(self) -> tuple[str, ...]:
        """Supported provider types.

        Empty tuple means the dimension is universal and applies to all providers.
        """

        return ()

    @property
    def modes(self) -> tuple[str, ...] | None:
        """Optional sub-modes for this dimension."""

        return None

    @property
    def default_mode(self) -> str | None:
        """Default mode when mode is omitted."""

        return None

    @property
    def mutex_group(self) -> str | None:
        """Optional mutual-exclusion group key.

        Presets in the same group are expected to be mutually exclusive in UI.
        """

        return None

    @property
    def evidence_hint(self) -> str | None:
        """Human-readable hint about which data this preset uses."""

        return None

    @property
    def hidden(self) -> bool:
        """If True, this dimension is excluded from API metadata listings.

        The dimension remains functional for backward compatibility but
        will not appear in the scheduling dialog.
        """

        return False

    @abstractmethod
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
        """Compute normalized metric in [0, 1], lower is better."""

    def is_applicable(self, provider_type: str) -> bool:
        """Return whether this dimension applies to the given provider type."""

        if not self.providers:
            return True
        normalized = _normalize_name(provider_type)
        return normalized in self.providers


def _normalize_name(value: Any) -> str:
    if not isinstance(value, str):
        return ""
    return value.strip().lower()


def _normalize_names(values: tuple[str, ...] | list[str]) -> tuple[str, ...]:
    normalized = [_normalize_name(item) for item in values]
    return tuple(item for item in normalized if item)


_registry_lock = RLock()
_registry: dict[str, PresetDimensionBase] = {}


def register_preset_dimension(dim: PresetDimensionBase) -> None:
    """Register or replace one preset dimension by name."""

    name = _normalize_name(dim.name)
    if not name:
        raise ValueError("preset dimension name must be a non-empty string")

    providers = _normalize_names(dim.providers)
    modes = _normalize_names(dim.modes or ())
    default_mode = _normalize_name(dim.default_mode)

    if modes and default_mode and default_mode not in modes:
        raise ValueError(f"default_mode must be one of modes for preset '{name}'")

    class _NormalizedDimension(PresetDimensionBase):
        # Lightweight wrapper to keep normalized metadata while preserving compute logic.
        def __init__(self, wrapped: PresetDimensionBase) -> None:
            self._wrapped = wrapped

        @property
        def name(self) -> str:
            return name

        @property
        def label(self) -> str:
            return self._wrapped.label

        @property
        def description(self) -> str:
            return self._wrapped.description

        @property
        def providers(self) -> tuple[str, ...]:
            return providers

        @property
        def modes(self) -> tuple[str, ...] | None:
            return modes or None

        @property
        def default_mode(self) -> str | None:
            if not modes:
                return None
            if default_mode:
                return default_mode
            return modes[0]

        @property
        def mutex_group(self) -> str | None:
            raw = _normalize_name(self._wrapped.mutex_group)
            return raw or None

        @property
        def evidence_hint(self) -> str | None:
            raw = str(self._wrapped.evidence_hint or "").strip()
            return raw or None

        @property
        def hidden(self) -> bool:
            return self._wrapped.hidden

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
            return self._wrapped.compute_metric(
                key_id=key_id,
                all_key_ids=all_key_ids,
                keys_by_id=keys_by_id,
                lru_scores=lru_scores,
                context=context,
                mode=mode,
            )

    normalized = _NormalizedDimension(dim)
    with _registry_lock:
        _registry[name] = normalized


def get_preset_dimension(name: str) -> PresetDimensionBase | None:
    """Get one registered preset dimension by name."""

    key = _normalize_name(name)
    if not key:
        return None
    with _registry_lock:
        return _registry.get(key)


def get_all_preset_dimensions() -> list[PresetDimensionBase]:
    """Get all registered preset dimensions in registration order."""

    with _registry_lock:
        return list(_registry.values())


def get_preset_names() -> set[str]:
    """Get all registered preset names."""

    with _registry_lock:
        return set(_registry.keys())


def get_preset_dimension_metas() -> list[PresetDimensionMeta]:
    """Get serializable metadata for all preset dimensions."""

    metas: list[PresetDimensionMeta] = []
    for dim in get_all_preset_dimensions():
        if dim.hidden:
            continue
        metas.append(
            PresetDimensionMeta(
                name=dim.name,
                label=dim.label,
                description=dim.description,
                providers=dim.providers,
                modes=dim.modes,
                default_mode=dim.default_mode,
                mutex_group=dim.mutex_group,
                evidence_hint=dim.evidence_hint,
            )
        )
    return metas


__all__ = [
    "PresetDimensionBase",
    "PresetDimensionMeta",
    "get_all_preset_dimensions",
    "get_preset_dimension",
    "get_preset_dimension_metas",
    "get_preset_names",
    "register_preset_dimension",
]
