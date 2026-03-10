from __future__ import annotations

import sys
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

ROOT = Path(__file__).resolve().parents[3]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from src.core.api_format.capabilities import (
    compute_total_input_context_for_api_format,
    fetch_models_for_api_format,
    get_provider_default_body_rules,
    register_provider_default_body_rules,
    register_provider_format_behavior,
    resolve_billing_template_for_api_format,
)
from src.core.api_format.metadata import get_default_body_rules_for_endpoint
from src.services.provider.behavior import get_provider_behavior


class _DummyResp:
    def __init__(self, status_code: int, payload: object, text: str = "") -> None:
        self.status_code = status_code
        self.payload = payload
        self.text = text

    def json(self) -> object:
        return self.payload


def test_api_format_capability_billing_mapping() -> None:
    assert resolve_billing_template_for_api_format("openai:cli") == "openai"
    assert resolve_billing_template_for_api_format("claude:chat") == "claude"
    assert resolve_billing_template_for_api_format("gemini:cli") == "gemini"


def test_provider_default_body_rules_use_unified_registry() -> None:
    provider_type = "unit_test_provider_rules"
    expected = [{"action": "drop", "path": "foo"}]
    register_provider_default_body_rules(provider_type, "openai:cli", expected)

    rules = get_provider_default_body_rules(provider_type, "openai:cli")
    assert rules == expected
    assert (
        get_default_body_rules_for_endpoint("openai:cli", provider_type=provider_type) == expected
    )


def test_provider_behavior_variants_use_unified_registry() -> None:
    provider_type = "unit_test_variant_provider"
    register_provider_format_behavior(
        provider_type,
        same_format_variant="same-unit",
        cross_format_variant="cross-unit",
    )

    behavior = get_provider_behavior(provider_type=provider_type, endpoint_sig="openai:cli")
    assert behavior.same_format_variant == "same-unit"
    assert behavior.cross_format_variant == "cross-unit"


def test_api_format_capability_total_input_context() -> None:
    assert compute_total_input_context_for_api_format("openai:chat", 100, 20, 30) == 120
    assert compute_total_input_context_for_api_format("claude:chat", 100, 20, 30) == 150


@pytest.mark.asyncio
async def test_fetch_models_uses_registered_claude_strategy() -> None:
    client = SimpleNamespace(
        get=AsyncMock(
            side_effect=[
                _DummyResp(
                    status_code=200,
                    payload={
                        "data": [{"id": "m1"}, {"id": "m2"}],
                        "has_more": True,
                        "last_id": "m2",
                    },
                ),
                _DummyResp(
                    status_code=200,
                    payload={
                        "data": [{"id": "m3"}],
                        "has_more": False,
                        "last_id": "m3",
                    },
                ),
            ]
        )
    )

    models, err = await fetch_models_for_api_format(
        client,  # type: ignore[arg-type]
        api_format="claude:chat",
        base_url="https://api.anthropic.com",
        api_key="k",
    )

    assert err is None
    assert [m.get("id") for m in models] == ["m1", "m2", "m3"]
    assert all(m.get("api_format") == "claude:chat" for m in models)
    assert client.get.call_count == 2
    _, kwargs2 = client.get.call_args_list[1]
    assert kwargs2.get("params", {}).get("after_id") == "m2"
