from __future__ import annotations

import gzip
import json

import pytest
from fastapi import HTTPException
from starlette.requests import Request

from src.api.base.context import ApiRequestContext


def _build_request(headers: dict[str, str] | None = None) -> Request:
    return _build_request_with_body(b"", headers=headers)


def _build_request_with_body(
    body: bytes,
    headers: dict[str, str] | None = None,
) -> Request:
    header_items = [
        (str(key).encode("latin-1"), str(value).encode("latin-1"))
        for key, value in (headers or {}).items()
    ]
    scope = {
        "type": "http",
        "http_version": "1.1",
        "method": "POST",
        "scheme": "http",
        "path": "/v1/messages",
        "raw_path": b"/v1/messages",
        "query_string": b"",
        "headers": header_items,
        "client": ("127.0.0.1", 12345),
        "server": ("testserver", 80),
    }

    received = False

    async def receive() -> dict[str, object]:
        nonlocal received
        if received:
            return {"type": "http.request", "body": b"", "more_body": False}
        received = True
        return {"type": "http.request", "body": body, "more_body": False}

    request = Request(scope, receive)
    request.state.perf_metrics = {}
    return request


def _build_context(raw_body: bytes, headers: dict[str, str] | None = None) -> ApiRequestContext:
    request = _build_request(headers=headers)
    return ApiRequestContext(
        request=request,
        db=None,  # type: ignore[arg-type]
        user=None,
        api_key=None,
        request_id="req_test",
        start_time=0.0,
        client_ip="127.0.0.1",
        user_agent="pytest",
        original_headers=headers or {},
        query_params={},
        raw_body=raw_body,
    )


class TestApiRequestContextEnsureJsonBody:
    def test_decompresses_gzip_body(self) -> None:
        payload = {"message": "hello", "count": 2}
        raw_body = gzip.compress(json.dumps(payload).encode("utf-8"))
        context = _build_context(raw_body, headers={"content-encoding": "gzip"})

        result = context.ensure_json_body()

        assert result == payload

    def test_rejects_invalid_gzip_body(self) -> None:
        context = _build_context(b"not-gzip-body", headers={"content-encoding": "gzip"})

        with pytest.raises(HTTPException) as exc_info:
            context.ensure_json_body()

        assert exc_info.value.status_code == 400
        assert exc_info.value.detail == "gzip 请求体解压失败"

    def test_build_records_client_encoding_preferences(self) -> None:
        request = _build_request(
            headers={
                "content-encoding": "gzip",
                "accept-encoding": "gzip, deflate",
            }
        )
        context = ApiRequestContext.build(
            request=request,
            db=None,  # type: ignore[arg-type]
            user=None,
            api_key=None,
            raw_body=b"{}",
        )

        assert context.client_content_encoding == "gzip"
        assert context.client_accept_encoding == "gzip, deflate"

    @pytest.mark.asyncio
    async def test_ensure_json_body_async_loads_body_lazily(self) -> None:
        payload = {"message": "hello", "count": 2}
        request = _build_request_with_body(json.dumps(payload).encode("utf-8"))
        context = ApiRequestContext.build(
            request=request,
            db=None,  # type: ignore[arg-type]
            user=None,
            api_key=None,
            raw_body=None,
        )

        assert context.raw_body is None

        result = await context.ensure_json_body_async()

        assert result == payload
        assert context.raw_body == json.dumps(payload).encode("utf-8")
