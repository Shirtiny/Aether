from __future__ import annotations

from unittest.mock import AsyncMock, patch

import pytest

from src.core.provider_oauth_utils import enrich_auth_config


@pytest.mark.asyncio
async def test_enrich_auth_config_antigravity_adds_project_id_and_email() -> None:
    auth_config: dict[str, object] = {}
    token_response: dict[str, object] = {}

    from src.services.provider.envelope import ensure_providers_bootstrapped

    ensure_providers_bootstrapped()

    with (
        patch(
            "src.core.provider_oauth_utils.fetch_google_email",
            AsyncMock(return_value="u@example.com"),
        ),
        patch(
            "src.services.provider.adapters.antigravity.client.load_code_assist",
            AsyncMock(
                return_value={
                    "cloudaicompanionProject": "project-1",
                    "currentTier": {"tierType": "PAID"},
                }
            ),
        ),
    ):
        out = await enrich_auth_config(
            provider_type="antigravity",
            auth_config=auth_config,  # in-place
            token_response=token_response,
            access_token="tok",
            proxy_config=None,
        )

    assert out["email"] == "u@example.com"
    assert out["project_id"] == "project-1"
    assert out["tier"] == "Pro"
