"""Pool scheduling hooks -- provider-type-specific pool behaviour.

Some provider types need custom logic during pool scheduling (e.g. extracting
a session UUID for sticky binding).  This module provides a small Protocol +
registry so the pool layer stays generic while provider-specific behaviour
lives alongside each adapter.
"""

from __future__ import annotations

import threading
from typing import Any, Protocol, runtime_checkable

# ---------------------------------------------------------------------------
# Protocol
# ---------------------------------------------------------------------------


@runtime_checkable
class PoolSchedulingHook(Protocol):
    """Provider-type-specific pool scheduling behaviour.

    Each provider type can optionally register a hook to customize:
    - Session UUID extraction (for sticky sessions)
    - Post-success / post-error callbacks

    Optional methods (checked via ``hasattr`` by callers):
    - ``on_pool_success``
    - ``on_pool_error``
    """

    name: str

    def extract_session_uuid(self, request_body: dict[str, Any]) -> str | None:
        """Extract a session UUID for sticky binding from the request body."""
        ...

    # -- Optional lifecycle callbacks -----------------------------------------
    # These are checked via ``hasattr`` so existing implementations that
    # don't define them will continue to work.

    def on_pool_success(
        self,
        *,
        key_id: str,
        session_uuid: str | None,
        context: dict[str, Any],
    ) -> None:
        """Called after a successful pool request (provider-specific logic)."""
        ...

    def on_pool_error(
        self,
        *,
        key_id: str,
        status_code: int,
        context: dict[str, Any],
    ) -> None:
        """Called after a failed pool request (provider-specific logic)."""
        ...


# ---------------------------------------------------------------------------
# Registry
# ---------------------------------------------------------------------------

_hook_registry: dict[str, PoolSchedulingHook] = {}
_registry_lock = threading.Lock()


def register_pool_hook(provider_type: str, hook: PoolSchedulingHook) -> None:
    """Register a pool scheduling hook for a provider type."""
    from src.core.provider_types import normalize_provider_type

    pt = normalize_provider_type(provider_type)
    with _registry_lock:
        _hook_registry[pt] = hook


def get_pool_hook(provider_type: str | None) -> PoolSchedulingHook | None:
    """Return the pool scheduling hook for a provider type, or ``None``."""
    if not provider_type:
        return None
    from src.services.provider.envelope import ensure_providers_bootstrapped

    ensure_providers_bootstrapped(provider_types=[provider_type])

    from src.core.provider_types import normalize_provider_type

    pt = normalize_provider_type(provider_type)
    return _hook_registry.get(pt)
