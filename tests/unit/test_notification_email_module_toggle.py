from __future__ import annotations

import time
from types import SimpleNamespace
from typing import Any, cast
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException
from starlette.requests import Request

import src.database as database_module
from src.middleware.plugin_middleware import PluginMiddleware
from src.modules.notification_email import notification_email_module
from src.services.system.config import SystemConfigService


def _build_request(path: str = "/boom") -> Request:
    scope = {
        "type": "http",
        "asgi": {"version": "3.0"},
        "http_version": "1.1",
        "method": "GET",
        "scheme": "http",
        "path": path,
        "raw_path": path.encode("utf-8"),
        "query_string": b"",
        "headers": [],
        "client": ("127.0.0.1", 12345),
        "server": ("testserver", 80),
    }
    return Request(scope)


async def _dummy_app(scope, receive, send):  # type: ignore[no-untyped-def]
    return


def test_notification_email_module_validate_config_requires_smtp(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "src.services.email.email_sender.EmailSenderService.is_smtp_configured",
        lambda _db: False,
    )
    assert notification_email_module.validate_config is not None
    ok, message = notification_email_module.validate_config(object())  # type: ignore[arg-type]
    assert ok is False
    assert "SMTP" in message


@pytest.mark.asyncio
async def test_call_error_plugins_skips_when_module_disabled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    middleware = PluginMiddleware(_dummy_app)
    send_error = AsyncMock()
    middleware.plugin_manager = cast(
        Any,
        SimpleNamespace(
            get_plugin=lambda *_args, **_kwargs: SimpleNamespace(
                enabled=True, send_error=send_error
            )
        ),
    )
    monkeypatch.setattr(
        middleware,
        "_is_notification_email_module_enabled",
        AsyncMock(return_value=False),
    )

    await middleware._call_error_plugins(
        _build_request(),
        RuntimeError("boom"),
        start_time=time.time(),
    )

    send_error.assert_not_awaited()


@pytest.mark.asyncio
async def test_call_error_plugins_sends_when_module_enabled(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    middleware = PluginMiddleware(_dummy_app)
    send_error = AsyncMock()
    middleware.plugin_manager = cast(
        Any,
        SimpleNamespace(
            get_plugin=lambda *_args, **_kwargs: SimpleNamespace(
                enabled=True, send_error=send_error
            )
        ),
    )
    monkeypatch.setattr(
        middleware,
        "_is_notification_email_module_enabled",
        AsyncMock(return_value=True),
    )
    request = _build_request("/internal-error")
    request.state.request_id = "req-1"

    await middleware._call_error_plugins(
        request,
        RuntimeError("boom"),
        start_time=time.time(),
    )

    send_error.assert_awaited_once()
    assert send_error.await_args is not None
    kwargs = send_error.await_args.kwargs
    assert kwargs["context"]["endpoint"] == "GET /internal-error"
    assert kwargs["context"]["request_id"] == "req-1"


@pytest.mark.asyncio
async def test_notification_email_switch_reads_from_system_config(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    middleware = PluginMiddleware(_dummy_app)
    request = _build_request("/x")

    class _FakeDb:
        closed = False

        def close(self) -> None:
            self.closed = True

    fake_db = _FakeDb()
    monkeypatch.setattr(database_module, "create_session", lambda: fake_db)
    monkeypatch.setattr(SystemConfigService, "get_config", lambda *_args, **_kwargs: True)

    enabled = await middleware._is_notification_email_module_enabled(request)
    assert enabled is True
    assert fake_db.closed is True


@pytest.mark.asyncio
async def test_call_error_plugins_ignores_http_4xx(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    middleware = PluginMiddleware(_dummy_app)
    send_error = AsyncMock()
    middleware.plugin_manager = cast(
        Any,
        SimpleNamespace(
            get_plugin=lambda *_args, **_kwargs: SimpleNamespace(
                enabled=True, send_error=send_error
            )
        ),
    )
    monkeypatch.setattr(
        middleware,
        "_is_notification_email_module_enabled",
        AsyncMock(return_value=True),
    )

    await middleware._call_error_plugins(
        _build_request(),
        HTTPException(status_code=400, detail="bad request"),
        start_time=time.time(),
    )

    send_error.assert_not_awaited()
