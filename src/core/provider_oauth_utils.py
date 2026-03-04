from __future__ import annotations

import asyncio
import json
from typing import Any, Awaitable, Callable
from urllib.parse import urlsplit, urlunsplit

import httpx
import jwt

from src.clients.http_client import HTTPClientPool
from src.core.logger import logger
from src.core.provider_types import ProviderType

_ANTHROPIC_TOKEN_URL = "https://console.anthropic.com/v1/oauth/token"
_GOOGLE_USERINFO_URL = "https://www.googleapis.com/oauth2/v1/userinfo?alt=json"


def _coerce_proxy_url(proxy_config: dict[str, Any] | None) -> str | None:
    if not proxy_config:
        return None
    try:
        if not proxy_config.get("enabled", True):
            return None
        from src.services.proxy_node.resolver import build_proxy_url  # lazy: core→services

        return build_proxy_url(proxy_config)
    except Exception:
        return None


def _redact_url(url: str) -> str:
    """Remove query and fragment to avoid leaking secrets in logs."""
    try:
        parts = urlsplit(url)
        return urlunsplit((parts.scheme, parts.netloc, parts.path, "", ""))
    except Exception:
        return "<invalid_url>"


def _format_exc_chain(e: BaseException) -> str:
    parts: list[str] = []
    cur: BaseException | None = e
    while cur is not None:
        parts.append(f"{type(cur).__name__}: {cur}")
        nxt = getattr(cur, "__cause__", None) or getattr(cur, "__context__", None)
        cur = nxt if isinstance(nxt, BaseException) else None
    return " <- ".join(parts)


async def _httpx_post(
    url: str,
    *,
    headers: dict[str, str] | None,
    data: Any,
    json_body: Any,
    proxy_config: dict[str, Any] | None,
    timeout_seconds: float,
) -> httpx.Response:
    client = await HTTPClientPool.get_proxy_client(proxy_config)
    proxy_url = _coerce_proxy_url(proxy_config)
    safe_url = _redact_url(url)

    last_exc: Exception | None = None
    # Only retry connection-level transient failures.
    for attempt in range(2):
        if attempt:
            await asyncio.sleep(0.25 * attempt)
        try:
            return await client.post(
                url,
                headers=headers,
                data=data,
                json=json_body,
                timeout=timeout_seconds,
            )
        except httpx.ConnectError as e:
            last_exc = e
            logger.warning(
                "OAuth token POST connect error (attempt={}/2) url={} host={} proxy={} err_chain={} err={!r}",
                attempt + 1,
                safe_url,
                urlsplit(url).netloc,
                proxy_url,
                _format_exc_chain(e),
                e,
            )
            if attempt == 0:
                continue
            raise
        except httpx.TimeoutException as e:
            last_exc = e
            logger.warning(
                "OAuth token POST timeout (attempt={}/2) url={} host={} proxy={} err_chain={} err={!r}",
                attempt + 1,
                safe_url,
                urlsplit(url).netloc,
                proxy_url,
                _format_exc_chain(e),
                e,
            )
            # Retry only the first time for timeouts.
            if attempt == 0:
                continue
            raise
        except httpx.RequestError as e:
            # Other request-level errors (proxy, TLS, etc). Log and re-raise.
            last_exc = e
            logger.error(
                "OAuth token POST request error url={} host={} proxy={} err_chain={} err={!r}",
                safe_url,
                urlsplit(url).netloc,
                proxy_url,
                _format_exc_chain(e),
                e,
            )
            raise

    # Should not be reachable.
    assert last_exc is not None
    raise last_exc


def _tls_client_post_sync(
    url: str,
    *,
    headers: dict[str, str] | None,
    data: Any,
    json_body: Any,
    proxy_url: str | None,
    timeout_seconds: float,
) -> tuple[int, dict[str, str], str]:
    # tls-client is optional at runtime; import only when needed.
    import tls_client  # type: ignore

    session = tls_client.Session(
        client_identifier="firefox_120",
        random_tls_extension_order=True,
    )

    if proxy_url:
        session.proxies = {"http": proxy_url, "https": proxy_url}

    # tls-client uses a requests-like API.
    resp = session.post(
        url,
        headers=headers or {},
        data=data,
        json=json_body,
        timeout_seconds=timeout_seconds,
    )

    # Normalize output
    status_code = int(getattr(resp, "status_code", 0))
    text = str(getattr(resp, "text", ""))
    resp_headers = dict(getattr(resp, "headers", {}) or {})
    return status_code, resp_headers, text


async def post_oauth_token(
    *,
    provider_type: str,
    token_url: str,
    headers: dict[str, str] | None,
    data: Any = None,
    json_body: Any = None,
    proxy_config: dict[str, Any] | None = None,
    timeout_seconds: float = 30.0,
) -> httpx.Response:
    """POST to token endpoint.

    Claude Code + Anthropic token URL will try tls-client (Firefox TLS fingerprint) first.
    If tls-client is unavailable or fails, fall back to httpx.

    IMPORTANT: Never log secrets (tokens, secrets). This function only logs generic errors.
    """

    if provider_type == ProviderType.CLAUDE_CODE and token_url == _ANTHROPIC_TOKEN_URL:
        proxy_url = _coerce_proxy_url(proxy_config)
        try:
            status_code, resp_headers, text = await asyncio.to_thread(
                _tls_client_post_sync,
                token_url,
                headers=headers,
                data=data,
                json_body=json_body,
                proxy_url=proxy_url,
                timeout_seconds=timeout_seconds,
            )
            return httpx.Response(
                status_code=status_code,
                headers=resp_headers,
                content=text.encode("utf-8", errors="replace"),
                request=httpx.Request("POST", token_url),
            )
        except Exception as e:
            logger.warning(
                "Claude OAuth token request via tls-client failed; fallback to httpx. err={!r}",
                e,
            )

    return await _httpx_post(
        token_url,
        headers=headers,
        data=data,
        json_body=json_body,
        proxy_config=proxy_config,
        timeout_seconds=timeout_seconds,
    )


def _as_non_empty_str(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    text = value.strip()
    return text or None


def _first_non_empty_str(values: list[Any]) -> str | None:
    for value in values:
        text = _as_non_empty_str(value)
        if text:
            return text
    return None


def _decode_unverified_jwt_payload(token: str) -> dict[str, Any] | None:
    try:
        claims = jwt.decode(
            token,
            options={
                "verify_signature": False,
                "verify_aud": False,
            },
        )
    except Exception:
        return None
    return claims if isinstance(claims, dict) else None


def _extract_codex_fields_from_claims(claims: dict[str, Any]) -> dict[str, Any]:
    auth_info = claims.get("https://api.openai.com/auth")
    auth = auth_info if isinstance(auth_info, dict) else {}

    result: dict[str, Any] = {}

    email = _first_non_empty_str(
        [
            claims.get("email"),
            auth.get("email"),
        ]
    )
    if email:
        result["email"] = email

    account_id = _first_non_empty_str(
        [
            auth.get("chatgpt_account_id"),
            auth.get("chatgptAccountId"),
            auth.get("account_id"),
            auth.get("accountId"),
            claims.get("chatgpt_account_id"),
            claims.get("chatgptAccountId"),
            claims.get("account_id"),
            claims.get("accountId"),
        ]
    )
    if account_id:
        result["account_id"] = account_id

    plan_type = _first_non_empty_str(
        [
            auth.get("chatgpt_plan_type"),
            auth.get("chatgptPlanType"),
            auth.get("plan_type"),
            auth.get("planType"),
            claims.get("chatgpt_plan_type"),
            claims.get("chatgptPlanType"),
            claims.get("plan_type"),
            claims.get("planType"),
        ]
    )
    if plan_type:
        result["plan_type"] = plan_type

    user_id = _first_non_empty_str(
        [
            auth.get("chatgpt_user_id"),
            auth.get("chatgptUserId"),
            auth.get("user_id"),
            auth.get("userId"),
            claims.get("chatgpt_user_id"),
            claims.get("chatgptUserId"),
            claims.get("user_id"),
            claims.get("userId"),
            claims.get("sub"),
        ]
    )
    if user_id:
        result["user_id"] = user_id

    return result


def parse_codex_id_token(id_token: Any) -> dict[str, Any]:
    """Parse Codex token payload without signature verification.

    Supports:
    - JWT string (typical id_token / sometimes access_token)
    - JSON string containing claims
    - Already-decoded dict payload
    """

    claims: dict[str, Any] | None = None
    if isinstance(id_token, dict):
        claims = id_token
    else:
        token_text = _as_non_empty_str(id_token)
        if not token_text:
            return {}

        if token_text.startswith("{"):
            try:
                payload = json.loads(token_text)
            except Exception:
                payload = None
            if isinstance(payload, dict):
                claims = payload

        if claims is None:
            claims = _decode_unverified_jwt_payload(token_text)

    if not isinstance(claims, dict):
        return {}
    return _extract_codex_fields_from_claims(claims)


async def fetch_google_email(
    access_token: str,
    *,
    proxy_config: dict[str, Any] | None = None,
    timeout_seconds: float = 10.0,
) -> str | None:
    if not access_token:
        return None

    client = await HTTPClientPool.get_proxy_client(proxy_config)
    try:
        resp = await client.get(
            _GOOGLE_USERINFO_URL,
            headers={"Authorization": f"Bearer {access_token}", "Accept": "application/json"},
            timeout=timeout_seconds,
        )
        if resp.status_code < 200 or resp.status_code >= 300:
            return None
        data = resp.json()
        email = data.get("email")
        if isinstance(email, str) and email:
            return email
        return None
    except Exception:
        return None


def extract_claude_email_from_token_response(token: dict[str, Any]) -> str | None:
    # CLIProxyAPI expects: { account: { email_address: ... } }
    try:
        account = token.get("account")
        if isinstance(account, dict):
            email = account.get("email_address")
            if isinstance(email, str) and email:
                return email
    except Exception:
        pass
    return None


# ---------------------------------------------------------------------------
# Auth Enricher Registry
# ---------------------------------------------------------------------------

AuthEnricherFn = Callable[
    [dict[str, Any], dict[str, Any], str, dict[str, Any] | None],
    Awaitable[dict[str, Any]],
]
_auth_enrichers: dict[str, AuthEnricherFn] = {}


def register_auth_enricher(provider_type: str, enricher: AuthEnricherFn) -> None:
    """注册 provider 特有的 auth_config enrichment hook。"""
    from src.core.provider_types import normalize_provider_type

    _auth_enrichers[normalize_provider_type(provider_type)] = enricher


async def _enrich_claude_code(
    auth_config: dict[str, Any],
    token_response: dict[str, Any],
    access_token: str,
    proxy_config: dict[str, Any] | None,
) -> dict[str, Any]:
    email = extract_claude_email_from_token_response(token_response)
    if email:
        auth_config["email"] = email
    return auth_config


async def _enrich_gemini_cli(
    auth_config: dict[str, Any],
    token_response: dict[str, Any],
    access_token: str,
    proxy_config: dict[str, Any] | None,
) -> dict[str, Any]:
    if not auth_config.get("email"):
        email = await fetch_google_email(
            access_token,
            proxy_config=proxy_config,
            timeout_seconds=10.0,
        )
        if email:
            auth_config["email"] = email
    return auth_config


def _bootstrap_auth_enrichers() -> None:
    # 简单的内置 enrichers 直接注册
    register_auth_enricher("claude_code", _enrich_claude_code)
    register_auth_enricher("gemini_cli", _enrich_gemini_cli)
    # Provider-specific enrichers 通过 plugin.register_all() 注册
    # (called from envelope.py bootstrap)


_bootstrap_auth_enrichers()


async def enrich_auth_config(
    *,
    provider_type: str,
    auth_config: dict[str, Any],
    token_response: dict[str, Any],
    access_token: str,
    proxy_config: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Enrich auth_config with non-secret metadata (email/account_id).

    各 provider 的 enrichment 逻辑通过 register_auth_enricher 注册。
    注意: ensure_providers_bootstrapped() 在应用启动时(main.py lifespan)已显式调用。
    """
    from src.core.provider_types import normalize_provider_type

    pt = normalize_provider_type(provider_type)
    enricher = _auth_enrichers.get(pt)
    if enricher:
        return await enricher(auth_config, token_response, access_token, proxy_config)
    return auth_config
