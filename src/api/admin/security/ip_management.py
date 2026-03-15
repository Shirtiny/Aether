"""
IP 安全管理接口

提供 IP 黑白名单管理和速率限制统计
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Request, status
from pydantic import BaseModel, Field, ValidationError
from sqlalchemy.orm import Session

from src.api.base.adapter import ApiMode
from src.api.base.authenticated_adapter import AuthenticatedApiAdapter
from src.api.base.context import ApiRequestContext
from src.api.base.pipeline import get_pipeline
from src.core.exceptions import InvalidRequestException, translate_pydantic_error
from src.database import get_db
from src.services.rate_limit.ip_limiter import IPRateLimiter

router = APIRouter(prefix="/api/admin/security/ip", tags=["Admin - Security"])
pipeline = get_pipeline()


# ========== Pydantic 模型 ==========


class AddIPToBlacklistRequest(BaseModel):
    """添加 IP 到黑名单请求"""

    ip_address: str = Field(..., description="IP 地址")
    reason: str = Field(..., min_length=1, max_length=200, description="加入黑名单的原因")
    ttl: int | None = Field(None, gt=0, description="过期时间（秒），None 表示永久")


class RemoveIPFromBlacklistRequest(BaseModel):
    """从黑名单移除 IP 请求"""

    ip_address: str = Field(..., description="IP 地址")


class AddIPToWhitelistRequest(BaseModel):
    """添加 IP 到白名单请求"""

    ip_address: str = Field(..., description="IP 地址或 CIDR 格式（如 192.168.1.0/24）")


class RemoveIPFromWhitelistRequest(BaseModel):
    """从白名单移除 IP 请求"""

    ip_address: str = Field(..., description="IP 地址")


# ========== API 端点 ==========


@router.post("/blacklist")
async def add_to_blacklist(request: Request, db: Session = Depends(get_db)) -> None:
    """
    添加 IP 到黑名单

    将指定 IP 地址添加到黑名单，被加入黑名单的 IP 将无法访问系统。需要管理员权限。

    **请求体字段**:
    - `ip_address`: IP 地址
    - `reason`: 加入黑名单的原因
    - `ttl`: 可选，过期时间（秒），不指定表示永久

    **返回字段**:
    - `success`: 是否成功
    - `message`: 操作结果信息
    - `reason`: 加入黑名单的原因
    - `ttl`: 过期时间（秒或"永久"）
    """
    adapter = AddToBlacklistAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=ApiMode.ADMIN)


@router.delete("/blacklist/{ip_address}")
async def remove_from_blacklist(
    ip_address: str, request: Request, db: Session = Depends(get_db)
) -> None:
    """
    从黑名单移除 IP

    将指定 IP 地址从黑名单中移除。需要管理员权限。

    **路径参数**:
    - `ip_address`: IP 地址

    **返回字段**:
    - `success`: 是否成功
    - `message`: 操作结果信息
    """
    adapter = RemoveFromBlacklistAdapter(ip_address=ip_address)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=ApiMode.ADMIN)


@router.get("/blacklist/stats")
async def get_blacklist_stats(request: Request, db: Session = Depends(get_db)) -> Any:
    """
    获取黑名单统计信息

    获取黑名单的统计信息和列表。需要管理员权限。

    **返回字段**:
    - `total`: 黑名单总数
    - `items`: 黑名单列表，每个项包含：
      - `ip`: IP 地址
      - `reason`: 加入原因
      - `added_at`: 添加时间
      - `ttl`: 剩余有效时间（秒）
    """
    adapter = GetBlacklistStatsAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=ApiMode.ADMIN)


@router.post("/whitelist")
async def add_to_whitelist(request: Request, db: Session = Depends(get_db)) -> None:
    """
    添加 IP 到白名单

    将指定 IP 地址或 CIDR 网段添加到白名单，白名单中的 IP 将跳过速率限制检查。需要管理员权限。

    **请求体字段**:
    - `ip_address`: IP 地址或 CIDR 格式（如 192.168.1.0/24）

    **返回字段**:
    - `success`: 是否成功
    - `message`: 操作结果信息
    """
    adapter = AddToWhitelistAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=ApiMode.ADMIN)


@router.delete("/whitelist/{ip_address}")
async def remove_from_whitelist(
    ip_address: str, request: Request, db: Session = Depends(get_db)
) -> None:
    """
    从白名单移除 IP

    将指定 IP 地址从白名单中移除。需要管理员权限。

    **路径参数**:
    - `ip_address`: IP 地址

    **返回字段**:
    - `success`: 是否成功
    - `message`: 操作结果信息
    """
    adapter = RemoveFromWhitelistAdapter(ip_address=ip_address)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=ApiMode.ADMIN)


@router.get("/whitelist")
async def get_whitelist(request: Request, db: Session = Depends(get_db)) -> Any:
    """
    获取白名单

    获取当前的 IP 白名单列表。需要管理员权限。

    **返回字段**:
    - `whitelist`: 白名单 IP 地址列表
    - `total`: 白名单总数
    """
    adapter = GetWhitelistAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=ApiMode.ADMIN)


# ========== 适配器实现 ==========


class AddToBlacklistAdapter(AuthenticatedApiAdapter):
    """添加 IP 到黑名单适配器"""

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        payload = context.ensure_json_body()
        try:
            req = AddIPToBlacklistRequest.model_validate(payload)
        except ValidationError as e:
            errors = e.errors()
            if errors:
                raise InvalidRequestException(translate_pydantic_error(errors[0]))
            raise InvalidRequestException("请求数据验证失败")

        success = await IPRateLimiter.add_to_blacklist(req.ip_address, req.reason, req.ttl)

        if not success:
            raise HTTPException(
                status_code=status.HTTP_500_INTERNAL_SERVER_ERROR,
                detail="添加 IP 到黑名单失败（Redis 不可用）",
            )

        return {
            "success": True,
            "message": f"IP {req.ip_address} 已加入黑名单",
            "reason": req.reason,
            "ttl": req.ttl or "永久",
        }


class RemoveFromBlacklistAdapter(AuthenticatedApiAdapter):
    """从黑名单移除 IP 适配器"""

    def __init__(self, ip_address: str):
        self.ip_address = ip_address

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        success = await IPRateLimiter.remove_from_blacklist(self.ip_address)

        if not success:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND, detail=f"IP {self.ip_address} 不在黑名单中"
            )

        return {"success": True, "message": f"IP {self.ip_address} 已从黑名单移除"}


class GetBlacklistStatsAdapter(AuthenticatedApiAdapter):
    """获取黑名单统计适配器"""

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        stats = await IPRateLimiter.get_blacklist_stats()
        return stats


class AddToWhitelistAdapter(AuthenticatedApiAdapter):
    """添加 IP 到白名单适配器"""

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        payload = context.ensure_json_body()
        try:
            req = AddIPToWhitelistRequest.model_validate(payload)
        except ValidationError as e:
            errors = e.errors()
            if errors:
                raise InvalidRequestException(translate_pydantic_error(errors[0]))
            raise InvalidRequestException("请求数据验证失败")

        success = await IPRateLimiter.add_to_whitelist(req.ip_address)

        if not success:
            raise HTTPException(
                status_code=status.HTTP_400_BAD_REQUEST,
                detail=f"添加 IP 到白名单失败（无效的 IP 格式或 Redis 不可用）",
            )

        return {"success": True, "message": f"IP {req.ip_address} 已加入白名单"}


class RemoveFromWhitelistAdapter(AuthenticatedApiAdapter):
    """从白名单移除 IP 适配器"""

    def __init__(self, ip_address: str):
        self.ip_address = ip_address

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        success = await IPRateLimiter.remove_from_whitelist(self.ip_address)

        if not success:
            raise HTTPException(
                status_code=status.HTTP_404_NOT_FOUND, detail=f"IP {self.ip_address} 不在白名单中"
            )

        return {"success": True, "message": f"IP {self.ip_address} 已从白名单移除"}


class GetWhitelistAdapter(AuthenticatedApiAdapter):
    """获取白名单适配器"""

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        whitelist = await IPRateLimiter.get_whitelist()
        return {"whitelist": list(whitelist), "total": len(whitelist)}
