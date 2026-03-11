"""
Video Adapter 通用基类

负责请求分发、Handler 创建、认证头提取等通用逻辑。
"""

from __future__ import annotations

from typing import Any, ClassVar

from fastapi import HTTPException, Request
from fastapi.responses import Response

from src.api.base.adapter import ApiAdapter, ApiMode
from src.api.base.context import ApiRequestContext
from src.api.handlers.base.video_handler_base import VideoHandlerBase
from src.core.api_format import (
    ApiFamily,
    EndpointKind,
    get_auth_handler,
    get_default_auth_method_for_endpoint,
)
from src.core.logger import logger


class VideoAdapterBase(ApiAdapter):
    """视频生成适配器基类"""

    FORMAT_ID: str = "UNKNOWN"
    HANDLER_CLASS: type[VideoHandlerBase]

    # 新架构：结构化标识（逐步替代直接依赖 FORMAT_ID 的语义）
    API_FAMILY: ClassVar[ApiFamily | None] = None
    ENDPOINT_KIND: ClassVar[EndpointKind] = EndpointKind.VIDEO

    name: str = "video.base"
    mode = ApiMode.STANDARD
    eager_request_body = False

    def __init__(self, allowed_api_formats: list[str] | None = None):
        self.allowed_api_formats = allowed_api_formats or [self.FORMAT_ID]

    def extract_api_key(self, request: Request) -> str | None:
        auth_method = get_default_auth_method_for_endpoint(self.FORMAT_ID)
        handler = get_auth_handler(auth_method)
        return handler.extract_credentials(request)

    async def handle(self, context: ApiRequestContext) -> Response:
        http_request = context.request
        path_params = context.path_params or {}

        if context.api_key is None or context.user is None:
            raise HTTPException(status_code=401, detail="Unauthorized")

        handler = self._create_handler(context)

        method = http_request.method.upper()
        path = http_request.url.path.lower()
        task_id = path_params.get("task_id")

        # Note: not every POST endpoint requires a body (e.g. cancel).
        original_request_body: dict[str, Any] = {}

        logger.debug(
            "[VideoAdapter] dispatch method={} path={} task_id={}",
            method,
            path,
            task_id,
        )

        # Download content
        if method == "GET" and path.endswith("/content") and task_id:
            return await handler.handle_download_content(
                task_id=task_id,
                http_request=http_request,
                original_headers=context.original_headers,
                query_params=context.query_params,
                path_params=path_params,
            )

        # Cancel task (POST /videos/{id}/cancel or explicit action=cancel)
        if (method == "POST" and path.endswith("/cancel")) or path_params.get("action") == "cancel":
            if not task_id:
                raise HTTPException(
                    status_code=400, detail="Task ID is required for cancel operation"
                )
            return await handler.handle_cancel_task(
                task_id=task_id,
                http_request=http_request,
                original_headers=context.original_headers,
                query_params=context.query_params,
                path_params=path_params,
            )

        # Delete task (DELETE /videos/{id})
        if method == "DELETE" and task_id:
            return await handler.handle_delete_task(
                task_id=task_id,
                http_request=http_request,
                original_headers=context.original_headers,
                query_params=context.query_params,
                path_params=path_params,
            )

        # Remix task
        if method == "POST" and path.endswith("/remix") and task_id:
            original_request_body = await context.ensure_json_body_async()
            return await handler.handle_remix_task(
                task_id=task_id,
                http_request=http_request,
                original_headers=context.original_headers,
                original_request_body=original_request_body,
                query_params=context.query_params,
                path_params=path_params,
            )

        # Get task
        if method == "GET" and task_id:
            return await handler.handle_get_task(
                task_id=task_id,
                http_request=http_request,
                original_headers=context.original_headers,
                query_params=context.query_params,
                path_params=path_params,
            )

        # List tasks
        if method == "GET" and not task_id:
            return await handler.handle_list_tasks(
                http_request=http_request,
                original_headers=context.original_headers,
                query_params=context.query_params,
                path_params=path_params,
            )

        # Create task (default)
        if method in {"POST", "PUT", "PATCH"}:
            original_request_body = await context.ensure_json_body_async()
        return await handler.handle_create_task(
            http_request=http_request,
            original_headers=context.original_headers,
            original_request_body=original_request_body,
            query_params=context.query_params,
            path_params=path_params,
        )

    def _create_handler(self, context: ApiRequestContext) -> VideoHandlerBase:
        return self.HANDLER_CLASS(
            db=context.db,
            user=context.user,
            api_key=context.api_key,
            request_id=context.request_id,
            client_ip=context.client_ip,
            user_agent=context.user_agent,
            start_time=context.start_time,
            allowed_api_formats=self.allowed_api_formats,
        )


__all__ = ["VideoAdapterBase"]
