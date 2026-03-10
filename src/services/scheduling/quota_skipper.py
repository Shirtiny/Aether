from __future__ import annotations

from src.models.database import ProviderAPIKey
from src.services.provider_keys.quota_reader import get_quota_reader


def is_key_quota_exhausted(
    provider_type: str | None,
    key: ProviderAPIKey,
    *,
    model_name: str,
) -> tuple[bool, str | None]:
    """Check ProviderAPIKey.upstream_metadata quota and decide whether to skip."""

    reader = get_quota_reader(provider_type, getattr(key, "upstream_metadata", None))
    result = reader.is_exhausted(model_name)
    return result.exhausted, result.reason
