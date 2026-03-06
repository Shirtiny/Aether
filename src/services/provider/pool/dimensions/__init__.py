"""Pool scheduling preset dimensions.

Importing this package registers all built-in preset dimensions.
"""

from __future__ import annotations

from . import cache_affinity  # noqa: F401
from . import cost_first  # noqa: F401
from . import free_first  # noqa: F401
from . import free_team_first  # noqa: F401
from . import health_first  # noqa: F401
from . import latency_first  # noqa: F401
from . import load_balance  # noqa: F401
from . import plus_first  # noqa: F401
from . import priority_first  # noqa: F401
from . import quota_balanced  # noqa: F401
from . import recent_refresh  # noqa: F401
from . import single_account  # noqa: F401
from . import team_first  # noqa: F401
from .registry import (
    PresetDimensionBase,
    PresetDimensionMeta,
    get_all_preset_dimensions,
    get_preset_dimension,
    get_preset_dimension_metas,
    get_preset_names,
    register_preset_dimension,
)

__all__ = [
    "PresetDimensionBase",
    "PresetDimensionMeta",
    "get_all_preset_dimensions",
    "get_preset_dimension",
    "get_preset_dimension_metas",
    "get_preset_names",
    "register_preset_dimension",
]
