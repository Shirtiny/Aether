"""
Provider Keys 领域服务模块。
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from sqlalchemy.orm import Session

    from src.models.endpoint_models import (
        EndpointAPIKeyCreate,
        EndpointAPIKeyResponse,
        EndpointAPIKeyUpdate,
    )

__all__ = [
    "batch_delete_endpoint_keys_response",
    "clear_oauth_invalid_response",
    "create_provider_key_response",
    "delete_endpoint_key_response",
    "update_endpoint_key_response",
    "reveal_endpoint_key_payload",
    "export_oauth_key_data",
    "get_keys_grouped_by_format",
    "list_provider_keys_responses",
    "refresh_provider_quota_for_provider",
]


def clear_oauth_invalid_response(db: Session, key_id: str) -> dict[str, str]:
    """清除 OAuth 失效标记并返回统一响应（惰性导入实现）。"""
    from src.services.provider_keys.key_command_service import clear_oauth_invalid_response as _impl

    return _impl(db=db, key_id=key_id)


async def create_provider_key_response(
    db: Session,
    provider_id: str,
    key_data: EndpointAPIKeyCreate,
) -> EndpointAPIKeyResponse:
    """创建 Provider Key 并返回响应对象（惰性导入实现）。"""
    from src.services.provider_keys.key_command_service import create_provider_key_response as _impl

    return await _impl(db=db, provider_id=provider_id, key_data=key_data)


async def batch_delete_endpoint_keys_response(db: Session, key_ids: list[str]) -> dict:
    """批量删除 Keys 并返回统一响应（惰性导入实现）。"""
    from src.services.provider_keys.key_command_service import (
        batch_delete_endpoint_keys_response as _impl,
    )

    return await _impl(db=db, key_ids=key_ids)


async def delete_endpoint_key_response(db: Session, key_id: str) -> dict[str, str]:
    """删除 Key 并返回统一响应（惰性导入实现）。"""
    from src.services.provider_keys.key_command_service import delete_endpoint_key_response as _impl

    return await _impl(db=db, key_id=key_id)


async def update_endpoint_key_response(
    db: Session,
    key_id: str,
    key_data: EndpointAPIKeyUpdate,
) -> EndpointAPIKeyResponse:
    """更新 Key 并返回响应对象（惰性导入实现）。"""
    from src.services.provider_keys.key_command_service import update_endpoint_key_response as _impl

    return await _impl(db=db, key_id=key_id, key_data=key_data)


def reveal_endpoint_key_payload(db: Session, key_id: str) -> dict[str, Any]:
    """获取完整的 API Key 或 Auth Config（惰性导入实现）。"""
    from src.services.provider_keys.key_query_service import reveal_endpoint_key_payload as _impl

    return _impl(db=db, key_id=key_id)


def export_oauth_key_data(db: Session, key_id: str) -> dict[str, Any]:
    """导出 OAuth Key 凭据（惰性导入实现）。"""
    from src.services.provider_keys.key_query_service import export_oauth_key_data as _impl

    return _impl(db=db, key_id=key_id)


def get_keys_grouped_by_format(db: Session) -> dict:
    """按 API 格式分组查询所有 Key（惰性导入实现）。"""
    from src.services.provider_keys.key_query_service import get_keys_grouped_by_format as _impl

    return _impl(db=db)


def list_provider_keys_responses(
    db: Session,
    provider_id: str,
    skip: int,
    limit: int,
) -> list[EndpointAPIKeyResponse]:
    """查询 Provider 下的 Key 列表（惰性导入实现）。"""
    from src.services.provider_keys.key_query_service import list_provider_keys_responses as _impl

    return _impl(db=db, provider_id=provider_id, skip=skip, limit=limit)


async def refresh_provider_quota_for_provider(
    db: Session,
    provider_id: str,
    codex_wham_usage_url: str,
    key_ids: list[str] | None = None,
) -> dict:
    """刷新 Provider 限额信息（惰性导入实现）。"""
    from src.services.provider_keys.key_quota_service import (
        refresh_provider_quota_for_provider as _impl,
    )

    return await _impl(
        db=db,
        provider_id=provider_id,
        codex_wham_usage_url=codex_wham_usage_url,
        key_ids=key_ids,
    )
