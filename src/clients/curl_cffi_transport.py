"""curl_cffi-based httpx AsyncTransport for TLS fingerprint impersonation.

When ``curl_cffi`` is installed, this transport can replace the default httpx
transport to send upstream requests with a browser-grade TLS fingerprint
(JA3/JA4), making the traffic indistinguishable from a real browser or
Node.js client.

The transport is used exclusively when ``tls_profile == "claude_code_nodejs"``
and ``curl_cffi`` is available. Otherwise, the system falls back to the
default httpx SSL context (best-effort cipher ordering only).

Design notes:
- curl_cffi AsyncSession instances are **reused** per (impersonate, proxy) pair
  to avoid rebuilding the TLS session on every request.
- Streaming is supported via ``aiter_content()`` on the curl_cffi response.
- The transport implements ``httpx.AsyncBaseTransport`` so it plugs into
  the existing ``HTTPClientPool`` without changing callers.
"""

from __future__ import annotations

import asyncio
import os
from collections import OrderedDict
from typing import Any

import httpx

from src.core.logger import logger

# ---------------------------------------------------------------------------
# Availability check
# ---------------------------------------------------------------------------

try:
    from curl_cffi.requests import AsyncSession  # type: ignore[import-untyped]

    CURL_CFFI_AVAILABLE = True
except ImportError:
    CURL_CFFI_AVAILABLE = False

# ---------------------------------------------------------------------------
# Default impersonate profile
# ---------------------------------------------------------------------------
# "chrome120" closely matches the TLS fingerprint of Node.js 20.x on Linux
# (which Claude Code CLI uses). If the upstream introduces fingerprint
# rotation, this can be made configurable per-profile.
DEFAULT_IMPERSONATE = "chrome120"


def _get_max_sessions() -> int:
    raw = os.getenv("CURL_CFFI_MAX_SESSIONS", "20")
    try:
        value = int(raw)
    except ValueError:
        logger.warning("环境变量 CURL_CFFI_MAX_SESSIONS 非法: {}, 使用默认值 20", raw)
        return 20
    return max(1, value)


# ---------------------------------------------------------------------------
# Session pool (module-level, async-safe)
# ---------------------------------------------------------------------------
_MAX_SESSIONS = _get_max_sessions()
_session_pool: OrderedDict[str, AsyncSession] = OrderedDict()
_pool_lock = asyncio.Lock()


def _session_key(impersonate: str, proxy: str | None) -> str:
    return f"{impersonate}::{proxy or '__direct__'}"


async def _get_or_create_session(
    impersonate: str = DEFAULT_IMPERSONATE,
    proxy: str | None = None,
) -> AsyncSession:
    """Get or create a cached curl_cffi AsyncSession."""
    key = _session_key(impersonate, proxy)
    evicted_sessions: list[tuple[str, AsyncSession]] = []
    async with _pool_lock:
        session = _session_pool.get(key)
        if session is not None:
            _session_pool.move_to_end(key)
            return session

        kwargs: dict[str, Any] = {
            "impersonate": impersonate,
            "verify": True,
        }
        if proxy:
            kwargs["proxy"] = proxy

        session = AsyncSession(**kwargs)
        _session_pool[key] = session
        _session_pool.move_to_end(key)
        while len(_session_pool) > _MAX_SESSIONS:
            old_key, old_session = _session_pool.popitem(last=False)
            evicted_sessions.append((old_key, old_session))
        logger.info(
            "curl_cffi session created: impersonate={}, proxy={}",
            impersonate,
            proxy or "direct",
        )
    for old_key, old_session in evicted_sessions:
        try:
            await old_session.close()
            logger.debug("curl_cffi session evicted: {}", old_key)
        except Exception as exc:
            logger.warning("curl_cffi session close failed during eviction ({}): {}", old_key, exc)
    return session


async def close_all_sessions() -> None:
    """Close all cached curl_cffi sessions (called at shutdown)."""
    async with _pool_lock:
        sessions = list(_session_pool.values())
        _session_pool.clear()
    for s in sessions:
        try:
            await s.close()
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Exception mapping (curl_cffi -> httpx)
# ---------------------------------------------------------------------------


def _map_curl_exception(exc: Exception) -> httpx.HTTPError:
    """Map curl_cffi exceptions to the closest httpx equivalents.

    This lets the upstream failover / error_classifier distinguish between
    transient timeouts (retryable) and hard connection failures.
    """
    if CURL_CFFI_AVAILABLE:
        from curl_cffi.requests.exceptions import ConnectionError as CurlConnectionError
        from curl_cffi.requests.exceptions import ProxyError as CurlProxyError
        from curl_cffi.requests.exceptions import Timeout as CurlTimeout

        if isinstance(exc, CurlTimeout):
            return httpx.ReadTimeout(f"curl_cffi timeout: {exc}")
        if isinstance(exc, CurlProxyError):
            return httpx.ProxyError(f"curl_cffi proxy error: {exc}")
        if isinstance(exc, CurlConnectionError):
            return httpx.ConnectError(f"curl_cffi connection error: {exc}")
    return httpx.ConnectError(f"curl_cffi request failed: {exc}")


# ---------------------------------------------------------------------------
# httpx AsyncTransport implementation
# ---------------------------------------------------------------------------


class CurlCffiStream(httpx.AsyncByteStream):
    """Async byte stream backed by curl_cffi response content iterator."""

    def __init__(self, curl_response: Any) -> None:
        self._response = curl_response
        self._consumed = False

    async def __aiter__(self) -> Any:  # type: ignore[override]
        if self._consumed:
            return
        try:
            async for chunk in self._response.aiter_content():
                yield chunk
        finally:
            self._consumed = True

    async def aclose(self) -> None:
        self._consumed = True
        close_fn = getattr(self._response, "aclose", None)
        if close_fn and callable(close_fn):
            try:
                await close_fn()
            except Exception:
                pass


class CurlCffiTransport(httpx.AsyncBaseTransport):
    """httpx-compatible async transport using curl_cffi for TLS impersonation.

    Usage::

        transport = CurlCffiTransport(proxy="http://proxy:8080")
        client = httpx.AsyncClient(transport=transport)
        resp = await client.post(url, json=payload, headers=headers)
    """

    def __init__(
        self,
        impersonate: str = DEFAULT_IMPERSONATE,
        proxy: str | None = None,
    ) -> None:
        self._impersonate = impersonate
        self._proxy = proxy

    async def handle_async_request(self, request: httpx.Request) -> httpx.Response:
        session = await _get_or_create_session(self._impersonate, self._proxy)

        # Build headers dict (skip host header, curl_cffi handles it).
        headers: dict[str, str] = {}
        for key, value in request.headers.raw:
            k = key.decode("latin-1").lower()
            if k in ("host", "content-length", "transfer-encoding"):
                continue
            headers[key.decode("latin-1")] = value.decode("latin-1")

        body = request.content if request.content else None
        method = request.method.upper()
        url = str(request.url)

        # Determine timeout from request extensions.
        timeout = 60.0
        if hasattr(request, "extensions") and isinstance(request.extensions, dict):
            raw_timeout = request.extensions.get("timeout")
            if isinstance(raw_timeout, dict):
                # httpx timeout pool format: {"connect": ..., "read": ..., "write": ..., "pool": ...}
                read_timeout = raw_timeout.get("read")
                if isinstance(read_timeout, (int, float)) and read_timeout > 0:
                    timeout = float(read_timeout)
            elif isinstance(raw_timeout, (int, float)) and raw_timeout > 0:
                timeout = float(raw_timeout)

        try:
            # Use stream=True for all requests so we can support streaming responses.
            curl_resp = await session.request(
                method,
                url,
                headers=headers,
                data=body,
                timeout=timeout,
                stream=True,
            )
        except Exception as exc:
            raise _map_curl_exception(exc) from exc

        # Build response headers.
        resp_headers_list: list[tuple[bytes, bytes]] = []
        if hasattr(curl_resp, "headers") and curl_resp.headers:
            for k, v in curl_resp.headers.multi_items():
                resp_headers_list.append((k.encode("latin-1"), v.encode("latin-1")))

        return httpx.Response(
            status_code=curl_resp.status_code,
            headers=resp_headers_list,
            stream=CurlCffiStream(curl_resp),
            request=request,
        )


__all__ = [
    "CURL_CFFI_AVAILABLE",
    "CurlCffiTransport",
    "close_all_sessions",
    "DEFAULT_IMPERSONATE",
]
