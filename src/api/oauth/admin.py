"""OAuth 管理端点（管理员）。"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Depends, Request
from pydantic import BaseModel, Field, ValidationError
from sqlalchemy.orm import Session

from src.api.base.admin_adapter import AdminApiAdapter
from src.api.base.context import ApiRequestContext
from src.api.base.pipeline import get_pipeline
from src.core.exceptions import InvalidRequestException, translate_pydantic_error
from src.database import get_db
from src.models.database import OAuthProvider
from src.services.auth.oauth.registry import get_oauth_provider_registry
from src.services.auth.oauth.service import OAuthService

router = APIRouter(prefix="/api/admin/oauth", tags=["Admin - OAuth"])
pipeline = get_pipeline()


class SupportedOAuthType(BaseModel):
    provider_type: str
    display_name: str
    default_authorization_url: str
    default_token_url: str
    default_userinfo_url: str
    default_scopes: list[str]


class OAuthProviderUpsertRequest(BaseModel):
    display_name: str = Field(..., min_length=1, max_length=100)
    client_id: str = Field(..., min_length=1, max_length=255)
    client_secret: str | None = Field(None, max_length=2048)

    authorization_url_override: str | None = Field(None, max_length=500)
    token_url_override: str | None = Field(None, max_length=500)
    userinfo_url_override: str | None = Field(None, max_length=500)
    scopes: list[str] | None = None

    redirect_uri: str = Field(..., min_length=1, max_length=500)
    frontend_callback_url: str = Field(..., min_length=1, max_length=500)

    attribute_mapping: dict[str, Any] | None = None
    extra_config: dict[str, Any] | None = None

    is_enabled: bool = False
    force: bool = False


class OAuthProviderAdminResponse(BaseModel):
    provider_type: str
    display_name: str
    client_id: str
    has_secret: bool
    authorization_url_override: str | None = None
    token_url_override: str | None = None
    userinfo_url_override: str | None = None
    scopes: list[str] | None = None
    redirect_uri: str
    frontend_callback_url: str
    attribute_mapping: dict[str, Any] | None = None
    extra_config: dict[str, Any] | None = None
    is_enabled: bool


class OAuthProviderTestResponse(BaseModel):
    authorization_url_reachable: bool
    token_url_reachable: bool
    secret_status: str
    details: str = ""


class OAuthProviderTestRequest(BaseModel):
    """测试请求，使用表单数据而非数据库配置"""

    client_id: str = Field(..., min_length=1)
    client_secret: str | None = None
    authorization_url_override: str | None = None
    token_url_override: str | None = None
    redirect_uri: str = Field(..., min_length=1)


@router.get("/supported-types", response_model=list[SupportedOAuthType])
async def get_supported_types(request: Request, db: Session = Depends(get_db)) -> Any:
    adapter = GetSupportedTypesAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.get("/providers", response_model=list[OAuthProviderAdminResponse])
async def list_provider_configs(request: Request, db: Session = Depends(get_db)) -> Any:
    adapter = ListOAuthProviderConfigsAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.get("/providers/{provider_type}", response_model=OAuthProviderAdminResponse)
async def get_provider_config(
    provider_type: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    adapter = GetOAuthProviderConfigAdapter(provider_type=provider_type)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.put("/providers/{provider_type}", response_model=OAuthProviderAdminResponse)
async def upsert_provider_config(
    provider_type: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    adapter = UpsertOAuthProviderConfigAdapter(provider_type=provider_type)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.delete("/providers/{provider_type}")
async def delete_provider_config(
    provider_type: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    adapter = DeleteOAuthProviderConfigAdapter(provider_type=provider_type)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.post("/providers/{provider_type}/test", response_model=OAuthProviderTestResponse)
async def test_provider_config(
    provider_type: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    adapter = TestOAuthProviderConfigAdapter(provider_type=provider_type)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


class GetSupportedTypesAdapter(AdminApiAdapter):
    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        registry = get_oauth_provider_registry()
        types = registry.get_supported_types()
        return [
            SupportedOAuthType(
                provider_type=t.provider_type,
                display_name=t.display_name,
                default_authorization_url=t.default_authorization_url,
                default_token_url=t.default_token_url,
                default_userinfo_url=t.default_userinfo_url,
                default_scopes=list(t.default_scopes),
            ).model_dump()
            for t in types
        ]


class ListOAuthProviderConfigsAdapter(AdminApiAdapter):
    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        rows = context.db.query(OAuthProvider).order_by(OAuthProvider.provider_type.asc()).all()
        return [
            OAuthProviderAdminResponse(
                provider_type=str(row.provider_type or ""),
                display_name=str(row.display_name or ""),
                client_id=str(row.client_id or ""),
                has_secret=bool(row.client_secret_encrypted),
                authorization_url_override=row.authorization_url_override,
                token_url_override=row.token_url_override,
                userinfo_url_override=row.userinfo_url_override,
                scopes=row.scopes,
                redirect_uri=str(row.redirect_uri or ""),
                frontend_callback_url=str(row.frontend_callback_url or ""),
                attribute_mapping=row.attribute_mapping,
                extra_config=row.extra_config,
                is_enabled=bool(row.is_enabled),
            ).model_dump()
            for row in rows
        ]


class GetOAuthProviderConfigAdapter(AdminApiAdapter):
    def __init__(self, provider_type: str):
        self.provider_type = provider_type

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        row = (
            context.db.query(OAuthProvider)
            .filter(OAuthProvider.provider_type == self.provider_type)
            .first()
        )
        if not row:
            raise InvalidRequestException("Provider 配置不存在")
        return OAuthProviderAdminResponse(
            provider_type=str(row.provider_type or ""),
            display_name=str(row.display_name or ""),
            client_id=str(row.client_id or ""),
            has_secret=bool(row.client_secret_encrypted),
            authorization_url_override=row.authorization_url_override,
            token_url_override=row.token_url_override,
            userinfo_url_override=row.userinfo_url_override,
            scopes=row.scopes,
            redirect_uri=str(row.redirect_uri or ""),
            frontend_callback_url=str(row.frontend_callback_url or ""),
            attribute_mapping=row.attribute_mapping,
            extra_config=row.extra_config,
            is_enabled=bool(row.is_enabled),
        ).model_dump()


class UpsertOAuthProviderConfigAdapter(AdminApiAdapter):
    def __init__(self, provider_type: str):
        self.provider_type = provider_type

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        payload = context.ensure_json_body()
        try:
            req = OAuthProviderUpsertRequest.model_validate(payload)
        except ValidationError as exc:
            errors = exc.errors()
            if errors:
                raise InvalidRequestException(translate_pydantic_error(errors[0]))
            raise InvalidRequestException("请求数据验证失败")

        row = await OAuthService.upsert_provider_config(
            db=context.db,
            provider_type=self.provider_type,
            data=req,
        )

        return OAuthProviderAdminResponse(
            provider_type=str(row.provider_type or ""),
            display_name=str(row.display_name or ""),
            client_id=str(row.client_id or ""),
            has_secret=bool(row.client_secret_encrypted),
            authorization_url_override=row.authorization_url_override,
            token_url_override=row.token_url_override,
            userinfo_url_override=row.userinfo_url_override,
            scopes=row.scopes,
            redirect_uri=str(row.redirect_uri or ""),
            frontend_callback_url=str(row.frontend_callback_url or ""),
            attribute_mapping=row.attribute_mapping,
            extra_config=row.extra_config,
            is_enabled=bool(row.is_enabled),
        ).model_dump()


class DeleteOAuthProviderConfigAdapter(AdminApiAdapter):
    def __init__(self, provider_type: str):
        self.provider_type = provider_type

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        await OAuthService.delete_provider_config(context.db, self.provider_type)
        return {"message": "删除成功"}


class TestOAuthProviderConfigAdapter(AdminApiAdapter):
    def __init__(self, provider_type: str):
        self.provider_type = provider_type

    async def handle(self, context: ApiRequestContext) -> Any:  # type: ignore[override]
        payload = context.ensure_json_body()
        try:
            req = OAuthProviderTestRequest.model_validate(payload)
        except ValidationError as exc:
            errors = exc.errors()
            if errors:
                raise InvalidRequestException(translate_pydantic_error(errors[0]))
            raise InvalidRequestException("请求数据验证失败")

        # 如果没有提供 client_secret，尝试从数据库获取已保存的
        client_secret = req.client_secret
        if not client_secret:
            existing = (
                context.db.query(OAuthProvider)
                .filter(OAuthProvider.provider_type == self.provider_type)
                .first()
            )
            if existing and existing.client_secret_encrypted:
                client_secret = existing.get_client_secret()

        result = await OAuthService.test_provider_config_with_data(
            provider_type=self.provider_type,
            client_id=req.client_id,
            client_secret=client_secret,
            authorization_url_override=req.authorization_url_override,
            token_url_override=req.token_url_override,
            redirect_uri=req.redirect_uri,
        )
        return OAuthProviderTestResponse(**result).model_dump()
