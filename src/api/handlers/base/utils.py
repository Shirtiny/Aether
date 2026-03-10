"""
Handler 基础工具函数
"""

from __future__ import annotations

import gzip
import json
from typing import TYPE_CHECKING, Any

from fastapi.responses import JSONResponse, Response

from src.core.api_format import filter_response_headers
from src.core.api_format.headers import get_header_value
from src.core.exceptions import EmbeddedErrorException, ProviderNotAvailableException
from src.core.http_compression import accepts_gzip, normalize_content_encoding
from src.core.logger import logger

if TYPE_CHECKING:
    from src.core.api_format.conversion.registry import FormatConversionRegistry


def get_format_converter_registry() -> FormatConversionRegistry:
    """
    获取格式转换注册表（线程安全）

    该函数确保 normalizers 已注册后再返回全局注册表实例。
    register_default_normalizers 内部已有双重检查锁，可安全多次调用。
    """
    from src.core.api_format.conversion.registry import (
        format_conversion_registry,
        register_default_normalizers,
    )

    register_default_normalizers()
    return format_conversion_registry


def build_sse_headers(extra_headers: dict[str, str] | None = None) -> dict[str, str]:
    """
    构建 SSE（text/event-stream）推荐响应头，用于减少代理缓冲带来的卡顿/成段输出。

    说明：
    - Cache-Control: no-transform 可避免部分代理对流做压缩/改写导致缓冲
    - X-Accel-Buffering: no 可显式提示 Nginx 关闭缓冲（即使全局已关闭也无害）
    """
    headers: dict[str, str] = {
        "Cache-Control": "no-cache, no-transform",
        "X-Accel-Buffering": "no",
    }
    if extra_headers:
        headers.update(extra_headers)
    return headers


def filter_proxy_response_headers(headers: dict[str, str] | None) -> dict[str, str]:
    """
    过滤上游响应头中不应透传给客户端的字段。

    主要用于“解析/转换后再返回”的场景：
    - 非流式：我们会 `resp.json()` 后再由 `JSONResponse` 重新序列化
    - 流式：我们会解析/重组 SSE 行再输出

    如果透传上游的 `content-length/content-encoding/...`，会导致客户端解码失败或等待更多字节。
    """
    return filter_response_headers(headers)


def resolve_client_content_encoding(
    original_headers: dict[str, str],
    hinted_content_encoding: str | None = None,
) -> str | None:
    """解析客户端请求体编码（优先使用上层透传值）。"""
    if hinted_content_encoding is not None:
        return normalize_content_encoding(hinted_content_encoding)
    return normalize_content_encoding(get_header_value(original_headers, "content-encoding"))


def resolve_client_accept_encoding(
    original_headers: dict[str, str],
    hinted_accept_encoding: str | None = None,
) -> str | None:
    """解析客户端 Accept-Encoding（优先使用上层透传值）。"""
    if isinstance(hinted_accept_encoding, str):
        normalized_hint = hinted_accept_encoding.strip()
        if normalized_hint:
            return normalized_hint
    header_value = get_header_value(original_headers, "accept-encoding")
    normalized_header = header_value.strip()
    return normalized_header or None


def build_json_response_for_client(
    *,
    status_code: int,
    content: Any,
    headers: dict[str, str] | None,
    client_accept_encoding: str | None,
) -> Response:
    """根据客户端 Accept-Encoding 返回普通或 gzip 压缩 JSON 响应。"""
    response_headers = dict(headers or {})
    response_headers.setdefault("content-type", "application/json")

    if not accepts_gzip(client_accept_encoding):
        return JSONResponse(status_code=status_code, content=content, headers=response_headers)

    json_bytes = json.dumps(content, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    compressed_bytes = gzip.compress(json_bytes, compresslevel=6)

    cleaned_headers = {
        key: value
        for key, value in response_headers.items()
        if key.lower() not in {"content-length", "content-encoding", "vary"}
    }
    cleaned_headers["Content-Encoding"] = "gzip"

    existing_vary = next((v for k, v in response_headers.items() if k.lower() == "vary"), "")
    vary_values = [part.strip() for part in str(existing_vary).split(",") if part.strip()]
    if not any(part.lower() == "accept-encoding" for part in vary_values):
        vary_values.append("Accept-Encoding")
    if vary_values:
        cleaned_headers["Vary"] = ", ".join(vary_values)

    return Response(
        status_code=status_code,
        content=compressed_bytes,
        headers=cleaned_headers,
        media_type="application/json",
    )


def check_html_response(line: str) -> bool:
    """
    检查行是否为 HTML 响应（base_url 配置错误的常见症状）

    Args:
        line: 要检查的行内容

    Returns:
        True 如果检测到 HTML 响应
    """
    lower_line = line.lstrip().lower()
    return lower_line.startswith("<!doctype") or lower_line.startswith("<html")


def check_prefetched_response_error(
    prefetched_chunks: list,
    parser: Any,
    request_id: str,
    provider_name: str,
    endpoint_id: str | None,
    base_url: str | None,
) -> None:
    """
    检查预读的响应是否为非 SSE 格式的错误响应（HTML 或纯 JSON 错误）

    某些代理可能返回：
    1. HTML 页面（base_url 配置错误）
    2. 纯 JSON 错误（无换行或多行 JSON）

    Args:
        prefetched_chunks: 预读的字节块列表
        parser: 响应解析器（需要有 is_error_response 和 parse_response 方法）
        request_id: 请求 ID（用于日志）
        provider_name: Provider 名称
        endpoint_id: Endpoint ID
        base_url: Endpoint 的 base_url

    Raises:
        ProviderNotAvailableException: 如果检测到 HTML 响应
        EmbeddedErrorException: 如果检测到 JSON 错误响应
    """
    if not prefetched_chunks:
        return

    try:
        prefetched_bytes = b"".join(prefetched_chunks)
        stripped = prefetched_bytes.lstrip()

        # 去除 BOM
        if stripped.startswith(b"\xef\xbb\xbf"):
            stripped = stripped[3:]

        # HTML 响应（通常是 base_url 配置错误导致返回网页）
        lower_prefix = stripped[:32].lower()
        if lower_prefix.startswith(b"<!doctype") or lower_prefix.startswith(b"<html"):
            endpoint_short = endpoint_id[:8] + "..." if endpoint_id else "N/A"
            logger.error(
                f"  [{request_id}] 检测到 HTML 响应，可能是 base_url 配置错误: "
                f"Provider={provider_name}, Endpoint={endpoint_short}, "
                f"base_url={base_url}"
            )
            raise ProviderNotAvailableException(
                "上游服务返回了非预期的响应格式",
                provider_name=provider_name,
                upstream_status=200,
                upstream_response=stripped.decode("utf-8", errors="replace")[:500],
            )

        # 纯 JSON（可能无换行/多行 JSON）
        if stripped.startswith(b"{") or stripped.startswith(b"["):
            payload_str = stripped.decode("utf-8", errors="replace").strip()
            data = json.loads(payload_str)
            if isinstance(data, dict) and parser.is_error_response(data):
                parsed = parser.parse_response(data, 200)
                logger.warning(
                    f"  [{request_id}] 检测到 JSON 错误响应: "
                    f"Provider={provider_name}, "
                    f"error_type={parsed.error_type}, "
                    f"embedded_status={parsed.embedded_status_code}, "
                    f"message={parsed.error_message}"
                )
                raise EmbeddedErrorException(
                    provider_name=provider_name,
                    error_code=parsed.embedded_status_code,
                    error_message=parsed.error_message,
                    error_status=parsed.error_type,
                )
    except json.JSONDecodeError:
        pass
