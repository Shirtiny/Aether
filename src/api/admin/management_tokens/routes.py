"""管理员 Management Token 管理端点"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from fastapi.responses import JSONResponse
from sqlalchemy import func
from sqlalchemy.orm import Session

from src.api.base.admin_adapter import AdminApiAdapter
from src.api.base.context import ApiRequestContext
from src.api.base.pipeline import get_pipeline
from src.core.exceptions import NotFoundException
from src.database import get_db
from src.models.database import AuditEventType, ManagementToken, User
from src.services.management_token import ManagementTokenService, token_to_dict

router = APIRouter(prefix="/api/admin/management-tokens", tags=["Admin - Management Tokens"])
pipeline = get_pipeline()


# ============== 安全基类 ==============


class AdminManagementTokenApiAdapter(AdminApiAdapter):
    """管理员 Management Token 管理 API 的基类

    安全限制：禁止使用 Management Token 调用这些接口。
    """

    def authorize(self, context: ApiRequestContext) -> None:
        # 先调用父类的认证和权限检查
        super().authorize(context)

        # 禁止使用 Management Token 调用 management-tokens 相关接口
        if context.management_token is not None:
            raise HTTPException(
                status_code=403,
                detail="不允许使用 Management Token 管理其他 Token，请使用 Web 界面或 JWT 认证",
            )


# ============== 路由 ==============


@router.get("")
async def list_all_management_tokens(
    request: Request,
    user_id: str | None = Query(None, description="筛选用户 ID"),
    is_active: bool | None = Query(None, description="筛选激活状态"),
    skip: int = Query(0, ge=0),
    limit: int = Query(50, ge=1, le=100),
    db: Session = Depends(get_db),
) -> Any:
    """列出所有 Management Tokens（管理员）

    管理员查看所有用户的 Management Tokens，支持筛选和分页。

    **查询参数**
    - user_id (Optional[str]): 筛选指定用户 ID 的 tokens
    - is_active (Optional[bool]): 筛选激活状态（true/false）
    - skip (int): 分页偏移量，默认 0
    - limit (int): 每页数量，范围 1-100，默认 50

    **返回字段**
    - items (List[dict]): Token 列表
        - id (str): Token ID
        - user_id (str): 所属用户 ID
        - user (dict): 用户信息（包含 id, username, email 等）
        - name (str): Token 名称
        - description (Optional[str]): 描述
        - token_hash (str): Token 哈希值（不返回明文）
        - is_active (bool): 是否激活
        - allowed_ips (Optional[List[str]]): IP 白名单
        - expires_at (Optional[str]): 过期时间（ISO 8601 格式）
        - last_used_at (Optional[str]): 最后使用时间
        - created_at (str): 创建时间
        - updated_at (str): 更新时间
    - total (int): 总数量
    - skip (int): 当前偏移量
    - limit (int): 当前每页数量
    """
    adapter = AdminListManagementTokensAdapter(
        user_id=user_id, is_active=is_active, skip=skip, limit=limit
    )
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.get("/{token_id}")
async def get_management_token(
    token_id: str,
    request: Request,
    db: Session = Depends(get_db),
) -> Any:
    """获取 Management Token 详情（管理员）

    管理员查看任意 Management Token 的详细信息。

    **路径参数**
    - token_id (str): Token ID

    **返回字段**
    - id (str): Token ID
    - user_id (str): 所属用户 ID
    - user (dict): 用户信息（包含 id, username, email 等）
    - name (str): Token 名称
    - description (Optional[str]): 描述
    - token_hash (str): Token 哈希值（不返回明文）
    - is_active (bool): 是否激活
    - allowed_ips (Optional[List[str]]): IP 白名单
    - expires_at (Optional[str]): 过期时间（ISO 8601 格式）
    - last_used_at (Optional[str]): 最后使用时间
    - created_at (str): 创建时间
    - updated_at (str): 更新时间
    """
    adapter = AdminGetManagementTokenAdapter(token_id=token_id)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.delete("/{token_id}")
async def delete_management_token(
    token_id: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    """删除任意 Management Token（管理员）

    管理员可以删除任意用户的 Management Token。

    **路径参数**
    - token_id (str): 要删除的 Token ID

    **返回字段**
    - message (str): 操作结果消息
    """
    adapter = AdminDeleteManagementTokenAdapter(token_id=token_id)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.patch("/{token_id}/status")
async def toggle_management_token(
    token_id: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    """切换任意 Management Token 状态（管理员）

    管理员可以启用/禁用任意用户的 Management Token。

    **路径参数**
    - token_id (str): Token ID

    **返回字段**
    - message (str): 操作结果消息（"Token 已启用" 或 "Token 已禁用"）
    - data (dict): 更新后的 Token 信息
        - id (str): Token ID
        - user_id (str): 所属用户 ID
        - user (dict): 用户信息
        - name (str): Token 名称
        - description (Optional[str]): 描述
        - token_hash (str): Token 哈希值
        - is_active (bool): 是否激活（已切换后的状态）
        - allowed_ips (Optional[List[str]]): IP 白名单
        - expires_at (Optional[str]): 过期时间
        - last_used_at (Optional[str]): 最后使用时间
        - created_at (str): 创建时间
        - updated_at (str): 更新时间
    """
    adapter = AdminToggleManagementTokenAdapter(token_id=token_id)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


# ============== 适配器 ==============


@dataclass
class AdminListManagementTokensAdapter(AdminManagementTokenApiAdapter):
    """列出所有 Management Tokens"""

    name: str = "admin_list_management_tokens"
    user_id: str | None = None
    is_active: bool | None = None
    skip: int = 0
    limit: int = 50

    async def handle(self, context: ApiRequestContext) -> Any:
        # 构建查询
        query = context.db.query(ManagementToken)

        if self.user_id:
            query = query.filter(ManagementToken.user_id == self.user_id)
        if self.is_active is not None:
            query = query.filter(ManagementToken.is_active == self.is_active)

        total = int(query.with_entities(func.count(ManagementToken.id)).scalar() or 0)
        tokens = (
            query.order_by(ManagementToken.created_at.desc())
            .offset(self.skip)
            .limit(self.limit)
            .all()
        )

        # 预加载用户信息
        user_ids = list({t.user_id for t in tokens})
        users = {u.id: u for u in context.db.query(User).filter(User.id.in_(user_ids)).all()}
        for token in tokens:
            token.user = users.get(token.user_id)

        return JSONResponse(
            content={
                "items": [token_to_dict(t, include_user=True) for t in tokens],
                "total": total,
                "skip": self.skip,
                "limit": self.limit,
            }
        )


@dataclass
class AdminGetManagementTokenAdapter(AdminManagementTokenApiAdapter):
    """获取 Management Token 详情"""

    name: str = "admin_get_management_token"
    token_id: str = ""

    async def handle(self, context: ApiRequestContext) -> Any:
        token = ManagementTokenService.get_token_by_id(db=context.db, token_id=self.token_id)

        if not token:
            raise NotFoundException("Management Token 不存在")

        # 加载用户信息
        token.user = context.db.query(User).filter(User.id == token.user_id).first()

        return JSONResponse(content=token_to_dict(token, include_user=True))


@dataclass
class AdminDeleteManagementTokenAdapter(AdminManagementTokenApiAdapter):
    """删除 Management Token"""

    name: str = "admin_delete_management_token"
    token_id: str = ""
    audit_success_event = AuditEventType.MANAGEMENT_TOKEN_DELETED

    async def handle(self, context: ApiRequestContext) -> Any:
        # 先获取 token 信息用于审计
        token = ManagementTokenService.get_token_by_id(db=context.db, token_id=self.token_id)

        if not token:
            raise NotFoundException("Management Token 不存在")

        context.add_audit_metadata(
            token_id=token.id,
            token_name=token.name,
            owner_user_id=token.user_id,
        )

        success = ManagementTokenService.delete_token(db=context.db, token_id=self.token_id)

        if not success:
            raise NotFoundException("Management Token 不存在")

        return JSONResponse(content={"message": "删除成功"})


@dataclass
class AdminToggleManagementTokenAdapter(AdminManagementTokenApiAdapter):
    """切换 Management Token 状态"""

    name: str = "admin_toggle_management_token"
    token_id: str = ""
    audit_success_event = AuditEventType.MANAGEMENT_TOKEN_UPDATED

    async def handle(self, context: ApiRequestContext) -> Any:
        token = ManagementTokenService.toggle_status(db=context.db, token_id=self.token_id)

        if not token:
            raise NotFoundException("Management Token 不存在")

        # 加载用户信息
        token.user = context.db.query(User).filter(User.id == token.user_id).first()

        context.add_audit_metadata(
            token_id=token.id,
            token_name=token.name,
            owner_user_id=token.user_id,
            is_active=token.is_active,
        )

        return JSONResponse(
            content={
                "message": f"Token 已{'启用' if token.is_active else '禁用'}",
                "data": token_to_dict(token, include_user=True),
            }
        )
