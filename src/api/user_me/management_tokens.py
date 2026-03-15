"""用户 Management Token 管理端点"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel, Field, field_validator
from sqlalchemy.orm import Session

from src.api.base.authenticated_adapter import AuthenticatedApiAdapter
from src.api.base.context import ApiRequestContext
from src.api.base.pipeline import get_pipeline
from src.core.exceptions import InvalidRequestException, NotFoundException
from src.database import get_db
from src.models.database import AuditEventType
from src.services.management_token import (
    ManagementTokenService,
    parse_expires_at,
    token_to_dict,
    validate_ip_list,
)

router = APIRouter(prefix="/api/me/management-tokens", tags=["Management Tokens"])
pipeline = get_pipeline()


# ============== 安全基类 ==============


class ManagementTokenApiAdapter(AuthenticatedApiAdapter):
    """Management Token 管理 API 的基类

    安全限制：禁止使用 Management Token 调用这些接口，
    防止用户通过已有的 Token 再创建/修改/删除其他 Token。
    """

    def authorize(self, context: ApiRequestContext) -> Any:
        # 先调用父类的认证检查
        super().authorize(context)

        # 禁止使用 Management Token 调用 management-tokens 相关接口
        if context.management_token is not None:
            raise HTTPException(
                status_code=403,
                detail="不允许使用 Management Token 管理其他 Token，请使用 Web 界面或 JWT 认证",
            )


# ============== 请求/响应模型 ==============


class CreateManagementTokenRequest(BaseModel):
    """创建 Management Token 请求"""

    name: str = Field(..., min_length=1, max_length=100, description="Token 名称")
    description: str | None = Field(None, max_length=500, description="描述")
    allowed_ips: list[str] | None = Field(None, description="IP 白名单")
    expires_at: datetime | None = Field(None, description="过期时间")

    @field_validator("allowed_ips")
    @classmethod
    def validate_allowed_ips(cls, v: list[str] | None) -> list[str] | None:
        return validate_ip_list(v)

    @field_validator("expires_at", mode="before")
    @classmethod
    def parse_expires(cls, v: Any) -> Any:
        return parse_expires_at(v)


class UpdateManagementTokenRequest(BaseModel):
    """更新 Management Token 请求

    对于 allowed_ips 和 expires_at 字段：
    - 未提供（字段不在请求中）: 不修改
    - 显式设为 null: 清空该字段
    - 提供有效值: 更新为新值
    """

    model_config = {"extra": "allow"}  # 允许额外字段以便检测哪些字段被显式提供

    name: str | None = Field(None, min_length=1, max_length=100)
    description: str | None = Field(None, max_length=500)
    allowed_ips: list[str] | None = None
    expires_at: datetime | None = None

    # 用于追踪哪些字段被显式提供（包括显式设为 null 的情况）
    _provided_fields: set[str] = set()

    def __init__(self, **data: Any) -> None:
        # 记录实际传入的字段（包括值为 None 的）
        provided = set(data.keys())
        super().__init__(**data)
        object.__setattr__(self, "_provided_fields", provided)

    def is_field_provided(self, field_name: str) -> bool:
        """检查字段是否被显式提供（区分未提供和显式设为 null）"""
        return field_name in self._provided_fields

    @field_validator("allowed_ips")
    @classmethod
    def validate_allowed_ips(cls, v: list[str] | None) -> list[str] | None:
        # 如果是 None，表示要清空，直接返回
        if v is None:
            return None
        return validate_ip_list(v)

    @field_validator("expires_at", mode="before")
    @classmethod
    def parse_expires(cls, v: Any) -> Any:
        # 如果是 None 或空字符串，表示要清空
        if v is None or (isinstance(v, str) and not v.strip()):
            return None
        return parse_expires_at(v)


# ============== 路由 ==============


@router.get("")
async def list_my_management_tokens(
    request: Request,
    is_active: bool | None = Query(None, description="筛选激活状态"),
    skip: int = Query(0, ge=0),
    limit: int = Query(50, ge=1, le=100),
    db: Session = Depends(get_db),
) -> Any:
    """列出当前用户的 Management Tokens

    获取当前登录用户创建的所有 Management Tokens，支持按激活状态筛选和分页。

    **查询参数**
    - is_active (Optional[bool]): 筛选激活状态（true/false），不传则返回全部
    - skip (int): 分页偏移量，默认 0
    - limit (int): 每页数量，范围 1-100，默认 50

    **返回字段**
    - items (List[dict]): Token 列表
        - id (str): Token ID
        - user_id (str): 所属用户 ID
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
    - quota (dict): 配额信息
        - used (int): 已使用数量
        - max (int): 最大允许数量
    """
    adapter = ListMyManagementTokensAdapter(is_active=is_active, skip=skip, limit=limit)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.post("")
async def create_my_management_token(request: Request, db: Session = Depends(get_db)) -> Any:
    """创建 Management Token

    为当前用户创建一个新的 Management Token。

    **请求体字段**
    - name (str): Token 名称，必填，长度 1-100
    - description (Optional[str]): 描述，可选，最大长度 500
    - allowed_ips (Optional[List[str]]): IP 白名单，可选，支持 IPv4/IPv6 和 CIDR 格式
    - expires_at (Optional[datetime]): 过期时间，可选，支持 ISO 8601 格式字符串或 datetime 对象

    **返回字段**
    - message (str): 操作结果消息
    - token (str): 生成的 Token 明文（仅在创建时返回一次，请妥善保存）
    - data (dict): Token 信息
        - id (str): Token ID
        - user_id (str): 所属用户 ID
        - name (str): Token 名称
        - description (Optional[str]): 描述
        - token_hash (str): Token 哈希值
        - is_active (bool): 是否激活（新创建默认为 true）
        - allowed_ips (Optional[List[str]]): IP 白名单
        - expires_at (Optional[str]): 过期时间
        - last_used_at (Optional[str]): 最后使用时间
        - created_at (str): 创建时间
        - updated_at (str): 更新时间
    """
    adapter = CreateMyManagementTokenAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.get("/{token_id}")
async def get_my_management_token(
    token_id: str,
    request: Request,
    db: Session = Depends(get_db),
) -> Any:
    """获取 Management Token 详情

    获取当前用户指定 Token 的详细信息。

    **路径参数**
    - token_id (str): Token ID

    **返回字段**
    - id (str): Token ID
    - user_id (str): 所属用户 ID
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
    adapter = GetMyManagementTokenAdapter(token_id=token_id)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.put("/{token_id}")
async def update_my_management_token(
    token_id: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    """更新 Management Token

    更新当前用户指定 Token 的信息。支持部分字段更新。

    **路径参数**
    - token_id (str): Token ID

    **请求体字段**（所有字段均可选）
    - name (Optional[str]): Token 名称，长度 1-100
    - description (Optional[str]): 描述，最大长度 500，传空字符串或 null 可清空
    - allowed_ips (Optional[List[str]]): IP 白名单，传 null 可清空
    - expires_at (Optional[datetime]): 过期时间，传 null 可清空

    注意：未提供的字段不会被修改，显式传 null 表示清空该字段。

    **返回字段**
    - message (str): 操作结果消息
    - data (dict): 更新后的 Token 信息
        - id (str): Token ID
        - user_id (str): 所属用户 ID
        - name (str): Token 名称
        - description (Optional[str]): 描述
        - token_hash (str): Token 哈希值
        - is_active (bool): 是否激活
        - allowed_ips (Optional[List[str]]): IP 白名单
        - expires_at (Optional[str]): 过期时间
        - last_used_at (Optional[str]): 最后使用时间
        - created_at (str): 创建时间
        - updated_at (str): 更新时间
    """
    adapter = UpdateMyManagementTokenAdapter(token_id=token_id)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.delete("/{token_id}")
async def delete_my_management_token(
    token_id: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    """删除 Management Token

    删除当前用户指定的 Token。

    **路径参数**
    - token_id (str): 要删除的 Token ID

    **返回字段**
    - message (str): 操作结果消息
    """
    adapter = DeleteMyManagementTokenAdapter(token_id=token_id)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.patch("/{token_id}/status")
async def toggle_my_management_token(
    token_id: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    """切换 Management Token 状态

    启用或禁用当前用户指定的 Token。

    **路径参数**
    - token_id (str): Token ID

    **返回字段**
    - message (str): 操作结果消息（"Token 已启用" 或 "Token 已禁用"）
    - data (dict): 更新后的 Token 信息
        - id (str): Token ID
        - user_id (str): 所属用户 ID
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
    adapter = ToggleMyManagementTokenAdapter(token_id=token_id)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.post("/{token_id}/regenerate")
async def regenerate_my_management_token(
    token_id: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    """重新生成 Management Token

    重新生成当前用户指定 Token 的值，旧 Token 将立即失效。

    **路径参数**
    - token_id (str): Token ID

    **返回字段**
    - message (str): 操作结果消息
    - token (str): 新生成的 Token 明文（仅在重新生成时返回一次，请妥善保存）
    - data (dict): Token 信息
        - id (str): Token ID
        - user_id (str): 所属用户 ID
        - name (str): Token 名称
        - description (Optional[str]): 描述
        - token_hash (str): 新的 Token 哈希值
        - is_active (bool): 是否激活
        - allowed_ips (Optional[List[str]]): IP 白名单
        - expires_at (Optional[str]): 过期时间
        - last_used_at (Optional[str]): 最后使用时间（重置为 null）
        - created_at (str): 创建时间
        - updated_at (str): 更新时间
    """
    adapter = RegenerateMyManagementTokenAdapter(token_id=token_id)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


# ============== 适配器 ==============


@dataclass
class ListMyManagementTokensAdapter(ManagementTokenApiAdapter):
    """列出用户的 Management Tokens"""

    name: str = "list_my_management_tokens"
    is_active: bool | None = None
    skip: int = 0
    limit: int = 50

    async def handle(self, context: ApiRequestContext) -> Any:
        from src.config.settings import config

        tokens, total = ManagementTokenService.list_tokens(
            db=context.db,
            user_id=context.user.id,
            is_active=self.is_active,
            skip=self.skip,
            limit=self.limit,
        )

        # 获取用户 Token 总数（用于配额显示）
        max_tokens = config.management_token_max_per_user

        return JSONResponse(
            content={
                "items": [token_to_dict(t) for t in tokens],
                "total": total,
                "skip": self.skip,
                "limit": self.limit,
                "quota": {
                    "used": total,
                    "max": max_tokens,
                },
            }
        )


@dataclass
class CreateMyManagementTokenAdapter(ManagementTokenApiAdapter):
    """创建 Management Token"""

    name: str = "create_my_management_token"
    audit_success_event = AuditEventType.MANAGEMENT_TOKEN_CREATED

    async def handle(self, context: ApiRequestContext) -> Any:
        body = context.ensure_json_body()

        try:
            req = CreateManagementTokenRequest(**body)
        except Exception as e:
            raise InvalidRequestException(str(e))

        try:
            token, raw_token = ManagementTokenService.create_token(
                db=context.db,
                user_id=context.user.id,
                name=req.name,
                description=req.description,
                allowed_ips=req.allowed_ips,
                expires_at=req.expires_at,
            )
        except ValueError as e:
            raise InvalidRequestException(str(e))

        context.add_audit_metadata(token_id=token.id, token_name=token.name)

        return JSONResponse(
            status_code=201,
            content={
                "message": "Management Token 创建成功",
                "token": raw_token,  # 仅在创建时返回一次
                "data": token_to_dict(token),
            },
        )


@dataclass
class GetMyManagementTokenAdapter(ManagementTokenApiAdapter):
    """获取 Management Token 详情"""

    name: str = "get_my_management_token"
    token_id: str = ""

    async def handle(self, context: ApiRequestContext) -> Any:
        token = ManagementTokenService.get_token_by_id(
            db=context.db, token_id=self.token_id, user_id=context.user.id
        )

        if not token:
            raise NotFoundException("Management Token 不存在")

        return JSONResponse(content=token_to_dict(token))


@dataclass
class UpdateMyManagementTokenAdapter(ManagementTokenApiAdapter):
    """更新 Management Token"""

    name: str = "update_my_management_token"
    token_id: str = ""
    audit_success_event = AuditEventType.MANAGEMENT_TOKEN_UPDATED

    async def handle(self, context: ApiRequestContext) -> Any:
        body = context.ensure_json_body()

        try:
            req = UpdateManagementTokenRequest(**body)
        except Exception as e:
            raise InvalidRequestException(str(e))

        # 构建更新参数，只包含显式提供的字段
        update_kwargs: dict = {
            "db": context.db,
            "token_id": self.token_id,
            "user_id": context.user.id,
        }

        # 对于普通字段，只有提供了才更新
        if req.is_field_provided("name"):
            update_kwargs["name"] = req.name
        if req.is_field_provided("description"):
            update_kwargs["description"] = req.description
            update_kwargs["clear_description"] = req.description is None or req.description == ""

        # 对于可清空字段，需要传递特殊标记
        if req.is_field_provided("allowed_ips"):
            update_kwargs["allowed_ips"] = req.allowed_ips
            update_kwargs["clear_allowed_ips"] = req.allowed_ips is None
        if req.is_field_provided("expires_at"):
            update_kwargs["expires_at"] = req.expires_at
            update_kwargs["clear_expires_at"] = req.expires_at is None

        try:
            token = ManagementTokenService.update_token(**update_kwargs)
        except ValueError as e:
            raise InvalidRequestException(str(e))

        if not token:
            raise NotFoundException("Management Token 不存在")

        context.add_audit_metadata(token_id=token.id, token_name=token.name)

        return JSONResponse(content={"message": "更新成功", "data": token_to_dict(token)})


@dataclass
class DeleteMyManagementTokenAdapter(ManagementTokenApiAdapter):
    """删除 Management Token"""

    name: str = "delete_my_management_token"
    token_id: str = ""
    audit_success_event = AuditEventType.MANAGEMENT_TOKEN_DELETED

    async def handle(self, context: ApiRequestContext) -> Any:
        # 先获取 token 信息用于审计
        token = ManagementTokenService.get_token_by_id(
            db=context.db, token_id=self.token_id, user_id=context.user.id
        )

        if not token:
            raise NotFoundException("Management Token 不存在")

        context.add_audit_metadata(token_id=token.id, token_name=token.name)

        success = ManagementTokenService.delete_token(
            db=context.db, token_id=self.token_id, user_id=context.user.id
        )

        if not success:
            raise NotFoundException("Management Token 不存在")

        return JSONResponse(content={"message": "删除成功"})


@dataclass
class ToggleMyManagementTokenAdapter(ManagementTokenApiAdapter):
    """切换 Management Token 状态"""

    name: str = "toggle_my_management_token"
    token_id: str = ""
    audit_success_event = AuditEventType.MANAGEMENT_TOKEN_UPDATED

    async def handle(self, context: ApiRequestContext) -> Any:
        token = ManagementTokenService.toggle_status(
            db=context.db, token_id=self.token_id, user_id=context.user.id
        )

        if not token:
            raise NotFoundException("Management Token 不存在")

        context.add_audit_metadata(
            token_id=token.id, token_name=token.name, is_active=token.is_active
        )

        return JSONResponse(
            content={
                "message": f"Token 已{'启用' if token.is_active else '禁用'}",
                "data": token_to_dict(token),
            }
        )


@dataclass
class RegenerateMyManagementTokenAdapter(ManagementTokenApiAdapter):
    """重新生成 Management Token"""

    name: str = "regenerate_my_management_token"
    token_id: str = ""
    audit_success_event = AuditEventType.MANAGEMENT_TOKEN_UPDATED

    async def handle(self, context: ApiRequestContext) -> Any:
        token, raw_token, old_token_hash = ManagementTokenService.regenerate_token(
            db=context.db, token_id=self.token_id, user_id=context.user.id
        )

        if not token:
            raise NotFoundException("Management Token 不存在")

        context.add_audit_metadata(
            token_id=token.id,
            token_name=token.name,
            regenerated=True,
        )

        return JSONResponse(
            content={
                "message": "Token 已重新生成",
                "token": raw_token,  # 仅在重新生成时返回一次
                "data": token_to_dict(token),
            }
        )
