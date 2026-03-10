"""
OpenAI CLI Adapter - 基于通用 CLI Adapter 基类的简化实现

继承 CliAdapterBase，只需配置 FORMAT_ID 和 HANDLER_CLASS。
"""

from __future__ import annotations

from typing import Any

from src.api.base.context import ApiRequestContext
from src.api.handlers.base.cli_adapter_base import CliAdapterBase, register_cli_adapter
from src.api.handlers.base.cli_handler_base import CliMessageHandlerBase
from src.config.settings import config
from src.core.api_format import ApiFamily, EndpointKind
from src.core.provider_types import ProviderType
from src.utils.url_utils import is_codex_url


@register_cli_adapter
class OpenAICliAdapter(CliAdapterBase):
    """
    OpenAI CLI API 适配器

    处理 /v1/responses 端点的请求。
    """

    FORMAT_ID = "openai:cli"
    API_FAMILY = ApiFamily.OPENAI
    name = "openai.cli"

    @property
    def HANDLER_CLASS(self) -> type[CliMessageHandlerBase]:
        """延迟导入 Handler 类避免循环依赖"""
        from src.api.handlers.openai_cli.handler import OpenAICliMessageHandler

        return OpenAICliMessageHandler

    def __init__(
        self,
        allowed_api_formats: list[str] | None = None,
        *,
        compact: bool = False,
    ):
        super().__init__(allowed_api_formats)
        self._compact = compact

    async def handle(self, context: ApiRequestContext) -> Any:
        """处理 CLI API 请求 -- compact 模式下注入标记并强制非流式"""
        if self._compact:
            body = context.ensure_json_body()
            body["_aether_compact"] = True
            # compact 端点永远非流式
            body.pop("stream", None)
            # 预设 Codex compact 上下文 -- finalize_provider_request 在 envelope
            # 之前运行，会清除 _aether_compact sentinel，所以在此处提前设置
            # context var 供 Codex envelope 和 build_codex_url 读取
            from src.services.provider.adapters.codex.context import (
                CodexRequestContext,
                set_codex_request_context,
            )

            set_codex_request_context(CodexRequestContext(is_compact=True))
        return await super().handle(context)

    @classmethod
    def build_endpoint_url(
        cls,
        base_url: str,
        request_data: dict[str, Any],
        model_name: str | None = None,
        *,
        compact: bool = False,
        provider_type: str | None = None,
    ) -> str:
        """构建OpenAI CLI API端点URL（使用 Responses API）

        对于 Codex OAuth 端点（如 chatgpt.com/backend-api/codex），直接追加 /responses；
        对于标准 OpenAI API，使用 /v1/responses。
        compact=True 时追加 /compact 后缀。

        provider_type 优先：仅当 provider_type 为 codex 时才使用 Codex 路由规则；
        未传入 provider_type 时回退到 URL 模式匹配（兼容旧调用方）。
        """
        suffix = "/responses/compact" if compact else "/responses"
        base_url = base_url.rstrip("/")
        # 判断是否按 Codex 规则构建 URL
        is_codex = (
            (provider_type or "").lower() == ProviderType.CODEX
            if provider_type
            else is_codex_url(base_url)
        )
        if is_codex:
            return f"{base_url}{suffix}"
        # 标准 OpenAI API
        if base_url.endswith("/v1"):
            return f"{base_url}{suffix}"
        else:
            return f"{base_url}/v1{suffix}"

    # build_request_body 使用基类实现
    # OpenAI CLI normalizer 会自动添加 instructions 字段

    @classmethod
    def build_request_body(
        cls,
        request_data: dict[str, Any] | None = None,
        *,
        base_url: str | None = None,
        provider_type: str | None = None,
    ) -> dict[str, Any]:
        """构建测试请求体（Codex 端点需要强制 stream=true 等特性）

        provider_type 优先：仅当 provider_type 为 codex 时才应用 Codex 变体；
        未传入 provider_type 时回退到 URL 模式匹配（兼容旧调用方）。
        """
        from src.api.handlers.base.request_builder import build_test_request_body

        is_codex = (
            (provider_type or "").lower() == ProviderType.CODEX
            if provider_type
            else (bool(base_url) and is_codex_url(base_url))
        )
        target_variant = "codex" if is_codex else None
        return build_test_request_body(
            cls.FORMAT_ID,
            request_data,
            target_variant=target_variant,
        )

    @classmethod
    def get_cli_user_agent(cls) -> str | None:
        """获取OpenAI CLI User-Agent"""
        return config.internal_user_agent_openai_cli

    @classmethod
    def get_cli_extra_headers(
        cls, *, base_url: str | None = None, provider_type: str | None = None
    ) -> dict[str, str]:
        """
        获取额外请求头

        对于 Codex OAuth 端点，添加特定头部（缺少可能导致 Cloudflare 拦截）。
        对于标准 OpenAI API 端点，仅添加 User-Agent。

        provider_type 优先：仅当 provider_type 为 codex 时才添加 Codex 头部；
        未传入 provider_type 时回退到 URL 模式匹配（兼容旧调用方）。
        """
        headers: dict[str, str] = {}

        # User-Agent
        cli_user_agent = cls.get_cli_user_agent()
        if cli_user_agent:
            headers["User-Agent"] = cli_user_agent

        # 仅 Codex 端点添加特定头部
        is_codex = (
            (provider_type or "").lower() == ProviderType.CODEX
            if provider_type
            else (bool(base_url) and is_codex_url(base_url))
        )
        if is_codex:
            # 与运行时路径保持一致：使用 Codex envelope 的 best-effort headers。
            from src.services.provider.adapters.codex.envelope import codex_oauth_envelope

            headers.update(codex_oauth_envelope.extra_headers() or {})

        return headers


__all__ = ["OpenAICliAdapter"]


@register_cli_adapter
class OpenAICompactAdapter(OpenAICliAdapter):
    """OpenAI Compact Responses adapter (/v1/responses/compact)."""

    FORMAT_ID = "openai:compact"
    ENDPOINT_KIND = EndpointKind.COMPACT
    name = "openai.compact"

    def __init__(self, allowed_api_formats: list[str] | None = None):
        super().__init__(allowed_api_formats=allowed_api_formats, compact=True)


__all__.append("OpenAICompactAdapter")
