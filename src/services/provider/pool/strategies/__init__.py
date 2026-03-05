"""Built-in pool strategies."""

# Import side effects: register built-in strategies.
import src.services.provider.pool.dimensions  # noqa: F401

from . import multi_score  # noqa: F401

__all__ = ["multi_score"]
