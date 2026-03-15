"""模块管理 API 端点"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from fastapi import APIRouter, Depends, Request
from pydantic import BaseModel
from sqlalchemy.orm import Session

from src.api.base.admin_adapter import AdminApiAdapter
from src.api.base.context import ApiRequestContext
from src.api.base.pipeline import get_pipeline
from src.core.exceptions import InvalidRequestException, NotFoundException
from src.core.modules import ModuleStatus, get_module_registry
from src.database import get_db

router = APIRouter(prefix="/api/admin/modules", tags=["Admin - Modules"])
pipeline = get_pipeline()


# ========== Response Models ==========


class ModuleStatusResponse(BaseModel):
    """模块状态响应"""

    name: str
    available: bool
    enabled: bool
    active: bool
    config_validated: bool
    config_error: str | None
    display_name: str
    description: str
    category: str
    admin_route: str | None
    admin_menu_icon: str | None
    admin_menu_group: str | None
    admin_menu_order: int
    health: str

    @classmethod
    def from_status(cls, status: ModuleStatus) -> ModuleStatusResponse:
        return cls(
            name=status.name,
            available=status.available,
            enabled=status.enabled,
            active=status.active,
            config_validated=status.config_validated,
            config_error=status.config_error,
            display_name=status.display_name,
            description=status.description,
            category=status.category.value,
            admin_route=status.admin_route,
            admin_menu_icon=status.admin_menu_icon,
            admin_menu_group=status.admin_menu_group,
            admin_menu_order=status.admin_menu_order,
            health=status.health.value,
        )


class SetModuleEnabledRequest(BaseModel):
    """设置模块启用状态请求"""

    enabled: bool


# ========== API Endpoints ==========


@router.get("/status")
async def get_all_modules_status(request: Request, db: Session = Depends(get_db)) -> Any:
    """
    获取所有模块状态

    返回系统中所有已注册模块的状态信息，包括可用性、启用状态等。
    需要管理员权限。

    **返回字段**:
    - 模块名称到状态的映射字典
    """
    adapter = AdminGetAllModulesStatusAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.get("/status/{module_name}")
async def get_module_status(
    module_name: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    """
    获取单个模块状态

    获取指定模块的详细状态信息。需要管理员权限。

    **路径参数**:
    - `module_name`: 模块名称

    **返回字段**:
    - 模块状态详情
    """
    adapter = AdminGetModuleStatusAdapter(module_name=module_name)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.put("/status/{module_name}/enabled")
async def set_module_enabled(
    module_name: str, request: Request, db: Session = Depends(get_db)
) -> Any:
    """
    设置模块启用状态

    启用或禁用指定模块。需要管理员权限。
    注意：只有 available=true 的模块才能被启用。

    **路径参数**:
    - `module_name`: 模块名称

    **请求体**:
    - `enabled`: 是否启用

    **返回字段**:
    - 更新后的模块状态
    """
    adapter = AdminSetModuleEnabledAdapter(module_name=module_name)
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


# ========== Adapters ==========


class AdminGetAllModulesStatusAdapter(AdminApiAdapter):
    """获取所有模块状态"""

    async def handle(self, context: ApiRequestContext) -> dict[str, Any]:
        registry = get_module_registry()
        all_status = await registry.get_all_status_async(context.db)

        return {
            name: ModuleStatusResponse.from_status(status).model_dump()
            for name, status in all_status.items()
        }


@dataclass
class AdminGetModuleStatusAdapter(AdminApiAdapter):
    """获取单个模块状态"""

    module_name: str

    async def handle(self, context: ApiRequestContext) -> dict[str, Any]:
        registry = get_module_registry()
        status = await registry.get_module_status_async(self.module_name, context.db)

        if status is None:
            raise NotFoundException(f"模块 '{self.module_name}' 不存在")

        return ModuleStatusResponse.from_status(status).model_dump()


@dataclass
class AdminSetModuleEnabledAdapter(AdminApiAdapter):
    """设置模块启用状态"""

    module_name: str

    async def handle(self, context: ApiRequestContext) -> dict[str, Any]:
        registry = get_module_registry()

        # 检查模块是否存在
        module = registry.get_module(self.module_name)
        if module is None:
            raise NotFoundException(f"模块 '{self.module_name}' 不存在")

        # 检查模块是否可用
        if not registry.is_available(self.module_name):
            raise InvalidRequestException(
                f"模块 '{self.module_name}' 不可用，无法启用。"
                f"请检查环境变量 {module.metadata.env_key} 和依赖库。"
            )

        # 解析请求体
        payload = context.ensure_json_body()
        try:
            req = SetModuleEnabledRequest.model_validate(payload)
        except Exception:
            raise InvalidRequestException("请求体格式错误，需要 enabled 字段")

        # 如果是启用模块，必须先通过配置验证
        if req.enabled:
            config_validated, config_error = registry.validate_config(self.module_name, context.db)
            if not config_validated:
                raise InvalidRequestException(f"模块配置未验证通过: {config_error}")

        # 设置启用状态
        registry.set_enabled(self.module_name, req.enabled, context.db)

        # 返回更新后的状态（模块已在上面检查存在，此处必定返回非 None）
        status = registry.get_module_status(self.module_name, context.db)
        assert status is not None
        return ModuleStatusResponse.from_status(status).model_dump()
