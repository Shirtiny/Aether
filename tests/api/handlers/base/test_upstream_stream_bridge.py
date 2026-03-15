from __future__ import annotations

import json
from collections.abc import AsyncIterator

import pytest

from src.api.handlers.base.upstream_stream_bridge import (
    aggregate_upstream_stream_to_internal_response,
)
from src.config.constants import StreamDefaults
from src.core.api_format.conversion import register_default_normalizers
from src.core.api_format.conversion.internal import TextBlock
from src.core.exceptions import ProviderNotAvailableException


async def _iter_stream_lines(lines: list[str]) -> AsyncIterator[bytes]:
    for line in lines:
        yield line.encode("utf-8")


@pytest.mark.asyncio
async def test_aggregate_claude_stream_uses_message_start_usage_when_message_delta_absent() -> None:
    register_default_normalizers()

    lines = [
        "data: "
        + json.dumps(
            {
                "type": "message_start",
                "message": {
                    "id": "msg_bridge_usage",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-4-5",
                    "content": [],
                    "usage": {
                        "input_tokens": 120,
                        "output_tokens": 0,
                        "cache_read_input_tokens": 11,
                    },
                },
            },
            ensure_ascii=False,
        )
        + "\n",
        "data: "
        + json.dumps(
            {
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""},
            },
            ensure_ascii=False,
        )
        + "\n",
        "data: "
        + json.dumps(
            {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "hello"},
            },
            ensure_ascii=False,
        )
        + "\n",
        "data: "
        + json.dumps({"type": "content_block_stop", "index": 0}, ensure_ascii=False)
        + "\n",
    ]

    internal = await aggregate_upstream_stream_to_internal_response(
        _iter_stream_lines(lines),
        provider_api_format="claude:cli",
        provider_name="claude_code",
        model="claude-sonnet-4-5",
        request_id="req_bridge_usage",
    )

    assert internal.usage is not None
    assert internal.usage.input_tokens == 120
    assert internal.usage.output_tokens == 0
    assert internal.usage.cache_read_tokens == 11
    assert len(internal.content) == 1
    assert isinstance(internal.content[0], TextBlock)
    assert internal.content[0].text == "hello"


@pytest.mark.asyncio
async def test_aggregate_stream_raises_when_buffer_exceeds_limit() -> None:
    register_default_normalizers()

    async def _iter_overflow_bytes() -> AsyncIterator[bytes]:
        yield b"x" * (StreamDefaults.MAX_STREAM_BUFFER_BYTES + 1)

    with pytest.raises(ProviderNotAvailableException):
        await aggregate_upstream_stream_to_internal_response(
            _iter_overflow_bytes(),
            provider_api_format="claude:cli",
            provider_name="claude_code",
            model="claude-sonnet-4-5",
            request_id="req_bridge_overflow",
        )


@pytest.mark.asyncio
async def test_aggregate_stream_raises_when_total_buffer_exceeds_hard_limit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    register_default_normalizers()
    monkeypatch.setattr(StreamDefaults, "MAX_STREAM_BUFFER_BYTES", 64)
    monkeypatch.setattr(StreamDefaults, "MAX_STREAM_BUFFER_TOTAL_BYTES", 80)

    async def _iter_total_overflow_bytes() -> AsyncIterator[bytes]:
        yield b":" + (b"a" * 30) + b"\n" + b":" + (b"b" * 30) + b"\n" + b":" + (b"c" * 30) + b"\n"

    with pytest.raises(ProviderNotAvailableException):
        await aggregate_upstream_stream_to_internal_response(
            _iter_total_overflow_bytes(),
            provider_api_format="claude:cli",
            provider_name="claude_code",
            model="claude-sonnet-4-5",
            request_id="req_bridge_total_overflow",
        )


@pytest.mark.asyncio
async def test_aggregate_stream_allows_large_chunk_with_multiple_complete_lines(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    register_default_normalizers()
    monkeypatch.setattr(StreamDefaults, "MAX_STREAM_BUFFER_BYTES", 64)

    async def _iter_multiline_bytes() -> AsyncIterator[bytes]:
        yield b":" + (b"a" * 30) + b"\n" + b":" + (b"b" * 30) + b"\n" + b":" + (b"c" * 30) + b"\n"

    internal = await aggregate_upstream_stream_to_internal_response(
        _iter_multiline_bytes(),
        provider_api_format="claude:cli",
        provider_name="claude_code",
        model="claude-sonnet-4-5",
        request_id="req_bridge_multiline",
    )

    assert internal is not None
