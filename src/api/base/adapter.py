from abc import ABC, abstractmethod
from enum import Enum
from typing import Any

from fastapi import Request, Response

from .context import ApiRequestContext


class ApiMode(str, Enum):
    STANDARD = "standard"
    PROXY = "proxy"
    ADMIN = "admin"
    USER = "user"  # JWT 认证的普通用户（不要求管理员权限）
    PUBLIC = "public"
    MANAGEMENT = "management"  # Management Token 认证


class ApiAdapter(ABC):
    """所有API格式适配器的抽象基类。"""

    name: str = "base"
    mode: ApiMode = ApiMode.STANDARD
    api_format: str | None = None  # 对应 Provider API 格式提示
    audit_log_enabled: bool = True
    audit_success_event = None
    audit_failure_event = None
    eager_request_body: bool = True

    @abstractmethod
    async def handle(self, context: ApiRequestContext) -> Response:
        """处理请求并返回 FastAPI Response。"""

    def authorize(self, context: ApiRequestContext) -> None:
        """可选的授权钩子，默认允许通过。"""
        return None

    def extract_api_key(self, request: Request) -> str | None:
        """
        从请求中提取客户端 API 密钥。

        子类应覆盖此方法以支持各自的认证头格式。

        Args:
            request: FastAPI Request 对象

        Returns:
            提取的 API 密钥，如果未找到则返回 None
        """
        return None

    def get_audit_metadata(
        self,
        context: ApiRequestContext,
        *,
        success: bool,
        status_code: int | None,
        error: str | None = None,
    ) -> dict[str, Any]:
        """允许适配器在审计日志中追加自定义字段。"""
        return {}

    def detect_capability_requirements(
        self,
        headers: dict[str, str],
        request_body: dict[str, Any] | None = None,
    ) -> dict[str, bool]:
        """
        检测请求中隐含的能力需求（子类可覆盖）

        不同 API 格式有不同的能力声明方式，例如：
        - Claude: anthropic-beta: context-1m-xxx 表示需要 1M 上下文
        - 其他格式可能有不同的请求头或请求体字段

        Args:
            headers: 请求头字典
            request_body: 请求体字典（可选）

        Returns:
            检测到的能力需求，如 {"context_1m": True}
        """
        return {}
