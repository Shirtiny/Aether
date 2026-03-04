from __future__ import annotations

import jwt
import pytest

from src.services.provider.adapters.codex.request_patching import (
    maybe_patch_request_for_codex,
    patch_openai_cli_request_for_codex,
)


def test_patch_openai_cli_request_for_codex_is_passthrough_except_internal_sentinel() -> None:
    req = {
        "model": "gpt-test",
        "input": [
            {
                "type": "message",
                "role": "system",
                "content": [{"type": "input_text", "text": "Hello"}],
            }
        ],
        "store": True,
        "stream": False,
        "instructions": "keep",
        "include": ["foo"],
        "parallel_tool_calls": False,
        "temperature": 0.7,
        "context_management": {"compaction": {"type": "summary"}},
        "user": "u_123",
        "_aether_compact": True,
    }
    out = patch_openai_cli_request_for_codex(req)

    assert out is not req
    assert "_aether_compact" not in out
    assert out["store"] is True
    assert out["stream"] is False
    assert out["instructions"] == "keep"
    assert out["include"] == ["foo"]
    assert out["parallel_tool_calls"] is False
    assert out["temperature"] == 0.7
    assert out["context_management"] == {"compaction": {"type": "summary"}}
    assert out["user"] == "u_123"
    assert out["input"][0]["role"] == "system"


def test_maybe_patch_request_for_codex_is_noop_for_non_codex() -> None:
    req = {"model": "gpt-test", "input": []}
    out = maybe_patch_request_for_codex(
        provider_type="custom",
        provider_api_format="openai:cli",
        request_body=req,
    )
    assert out is req


def test_maybe_patch_request_for_codex_is_noop_for_non_openai_cli() -> None:
    req = {"model": "gpt-test", "input": []}
    out = maybe_patch_request_for_codex(
        provider_type="codex",
        provider_api_format="openai:chat",
        request_body=req,
    )
    assert out is req


def test_maybe_patch_request_for_codex_patches_for_codex_openai_cli() -> None:
    req = {"model": "gpt-test", "input": [], "_aether_compact": True, "store": True}
    out = maybe_patch_request_for_codex(
        provider_type="codex",
        provider_api_format="openai:cli",
        request_body=req,
    )

    assert out is not req
    assert out["store"] is True
    assert "_aether_compact" not in out


def test_maybe_patch_request_for_codex_patches_for_codex_openai_compact() -> None:
    req = {"model": "gpt-test", "input": [], "_aether_compact": True, "store": True}
    out = maybe_patch_request_for_codex(
        provider_type="codex",
        provider_api_format="openai:compact",
        request_body=req,
    )

    assert out is not req
    assert out["store"] is True
    assert "_aether_compact" not in out


def test_openai_cli_normalizer_request_from_internal_codex_variant_preserves_store() -> None:
    from src.core.api_format.conversion.normalizers.openai_cli import OpenAICliNormalizer

    normalizer = OpenAICliNormalizer()
    internal = normalizer.request_to_internal({"model": "gpt-test", "input": [], "store": True})
    out = normalizer.request_from_internal(internal, target_variant="codex")

    assert out["store"] is True


def test_openai_cli_normalizer_request_from_internal_codex_variant_defaults_store_false() -> None:
    from src.core.api_format.conversion.normalizers.openai_cli import OpenAICliNormalizer

    normalizer = OpenAICliNormalizer()
    internal = normalizer.request_to_internal({"model": "gpt-test", "input": []})
    out = normalizer.request_from_internal(internal, target_variant="codex")

    assert out["store"] is False


def test_codex_envelope_extra_headers_includes_sse_accept_and_session() -> None:
    from src.services.provider.adapters.codex.envelope import codex_oauth_envelope

    headers = codex_oauth_envelope.extra_headers() or {}
    assert headers.get("Accept") == "text/event-stream"
    assert headers.get("Originator") == "codex_cli_rs"
    assert headers.get("Version") == "0.101.0"
    assert headers.get("Connection") == "Keep-Alive"
    assert isinstance(headers.get("Session_id"), str)
    assert headers.get("Session_id")


def test_codex_envelope_extra_headers_compact_uses_json_accept() -> None:
    from src.services.provider.adapters.codex.context import (
        CodexRequestContext,
        set_codex_request_context,
    )
    from src.services.provider.adapters.codex.envelope import codex_oauth_envelope

    set_codex_request_context(CodexRequestContext(is_compact=True))
    headers = codex_oauth_envelope.extra_headers() or {}
    assert headers.get("Accept") == "application/json"
    set_codex_request_context(None)


def test_codex_envelope_extra_headers_uses_account_id_header() -> None:
    from src.services.provider.adapters.codex.context import (
        CodexRequestContext,
        set_codex_request_context,
    )
    from src.services.provider.adapters.codex.envelope import codex_oauth_envelope

    set_codex_request_context(CodexRequestContext(account_id="acc_123"))
    headers = codex_oauth_envelope.extra_headers() or {}
    assert headers.get("Chatgpt-Account-Id") == "acc_123"
    set_codex_request_context(None)


def _encode_unsigned_jwt(payload: dict[str, object]) -> str:
    token = jwt.encode(payload, key="", algorithm="none")
    return token.decode("utf-8") if isinstance(token, bytes) else token


@pytest.mark.asyncio
async def test_enrich_codex_uses_access_token_when_id_token_missing() -> None:
    from src.services.provider.adapters.codex.plugin import enrich_codex

    access_token = _encode_unsigned_jwt(
        {
            "email": "u@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acc-access",
                "chatgpt_plan_type": "team",
                "chatgpt_user_id": "user-access",
            },
        }
    )

    auth_config: dict[str, object] = {}
    out = await enrich_codex(
        auth_config=auth_config,
        token_response={"access_token": access_token},
        access_token=access_token,
        proxy_config=None,
    )

    assert out["email"] == "u@example.com"
    assert out["account_id"] == "acc-access"
    assert out["plan_type"] == "team"
    assert out["user_id"] == "user-access"
