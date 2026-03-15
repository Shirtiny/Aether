"""LDAP配置管理API端点。"""

from __future__ import annotations

import re
from typing import Any

from fastapi import APIRouter, Depends, Request
from pydantic import BaseModel, Field, ValidationError, field_validator
from sqlalchemy.orm import Session

from src.api.base.admin_adapter import AdminApiAdapter
from src.api.base.context import ApiRequestContext
from src.api.base.pipeline import get_pipeline
from src.core.crypto import crypto_service
from src.core.enums import AuthSource
from src.core.exceptions import InvalidRequestException, translate_pydantic_error
from src.core.logger import logger
from src.database import get_db
from src.models.database import AuditEventType, LDAPConfig, User, UserRole
from src.services.system.audit import AuditService

router = APIRouter(prefix="/api/admin/ldap", tags=["Admin - LDAP"])
pipeline = get_pipeline()

# bcrypt 哈希格式正则：$2a$, $2b$, $2y$ + 2位cost + $ + 53字符(22位salt + 31位hash)
BCRYPT_HASH_PATTERN = re.compile(r"^\$2[aby]\$\d{2}\$.{53}$")


# ========== Request/Response Models ==========


class LDAPConfigResponse(BaseModel):
    """LDAP配置响应（不返回密码）"""

    server_url: str | None = None
    bind_dn: str | None = None
    base_dn: str | None = None
    has_bind_password: bool = False
    user_search_filter: str
    username_attr: str
    email_attr: str
    display_name_attr: str
    is_enabled: bool
    is_exclusive: bool
    use_starttls: bool
    connect_timeout: int


class LDAPConfigUpdate(BaseModel):
    """LDAP配置更新请求"""

    server_url: str = Field(..., min_length=1, max_length=255)
    bind_dn: str = Field(..., min_length=1, max_length=255)
    # 允许空字符串表示"清除密码"；非空时自动 strip 并校验不能为空
    bind_password: str | None = Field(None, max_length=1024)
    base_dn: str = Field(..., min_length=1, max_length=255)
    user_search_filter: str = Field(default="(uid={username})", max_length=500)
    username_attr: str = Field(default="uid", max_length=50)
    email_attr: str = Field(default="mail", max_length=50)
    display_name_attr: str = Field(default="cn", max_length=50)
    is_enabled: bool = False
    is_exclusive: bool = False
    use_starttls: bool = False
    connect_timeout: int = Field(default=10, ge=1, le=60)  # 单次操作超时，跨国网络建议 15-30 秒

    @field_validator("bind_password")
    @classmethod
    def validate_bind_password(cls, v: str | None) -> str | None:
        if v is None or v == "":
            return v
        v = v.strip()
        if not v:
            raise ValueError("绑定密码不能为空")
        return v

    @field_validator("user_search_filter")
    @classmethod
    def validate_search_filter(cls, v: str) -> str:
        if "{username}" not in v:
            raise ValueError("搜索过滤器必须包含 {username} 占位符")
        # 验证括号匹配和嵌套正确性
        depth = 0
        for char in v:
            if char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
                if depth < 0:
                    raise ValueError("搜索过滤器括号不匹配")
        if depth != 0:
            raise ValueError("搜索过滤器括号不匹配")
        # 限制过滤器复杂度，防止构造复杂查询
        # 检查嵌套层数而非括号总数
        depth = 0
        max_depth = 0
        for char in v:
            if char == "(":
                depth += 1
                max_depth = max(max_depth, depth)
            elif char == ")":
                depth -= 1
        if max_depth > 5:
            raise ValueError("搜索过滤器嵌套层数过深（最多5层）")
        if len(v) > 200:
            raise ValueError("搜索过滤器过长（最多200字符）")
        return v


class LDAPTestResponse(BaseModel):
    """LDAP连接测试响应"""

    success: bool
    message: str


class LDAPConfigTest(BaseModel):
    """LDAP配置测试请求（全部可选，用于临时覆盖）"""

    server_url: str | None = Field(None, min_length=1, max_length=255)
    bind_dn: str | None = Field(None, min_length=1, max_length=255)
    bind_password: str | None = Field(None, min_length=1)
    base_dn: str | None = Field(None, min_length=1, max_length=255)
    user_search_filter: str | None = Field(None, max_length=500)
    username_attr: str | None = Field(None, max_length=50)
    email_attr: str | None = Field(None, max_length=50)
    display_name_attr: str | None = Field(None, max_length=50)
    is_enabled: bool | None = None
    is_exclusive: bool | None = None
    use_starttls: bool | None = None
    connect_timeout: int | None = Field(None, ge=1, le=60)

    @field_validator("user_search_filter")
    @classmethod
    def validate_search_filter(cls, v: str | None) -> str | None:
        if v is None:
            return v
        if "{username}" not in v:
            raise ValueError("搜索过滤器必须包含 {username} 占位符")
        # 验证括号匹配和嵌套正确性
        depth = 0
        for char in v:
            if char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
                if depth < 0:
                    raise ValueError("搜索过滤器括号不匹配")
        if depth != 0:
            raise ValueError("搜索过滤器括号不匹配")
        # 限制过滤器复杂度（检查嵌套层数而非括号总数）
        depth = 0
        max_depth = 0
        for char in v:
            if char == "(":
                depth += 1
                max_depth = max(max_depth, depth)
            elif char == ")":
                depth -= 1
        if max_depth > 5:
            raise ValueError("搜索过滤器嵌套层数过深（最多5层）")
        if len(v) > 200:
            raise ValueError("搜索过滤器过长（最多200字符）")
        return v


# ========== API Endpoints ==========


@router.get("/config")
async def get_ldap_config(request: Request, db: Session = Depends(get_db)) -> Any:
    """
    获取 LDAP 配置

    获取系统当前的 LDAP 认证配置信息，用于管理界面显示和编辑。
    密码字段不会返回原文，仅返回是否已设置的标志。

    **返回字段**:
    - `server_url`: LDAP 服务器地址（如：ldap://ldap.example.com:389）
    - `bind_dn`: 绑定 DN（如：cn=admin,dc=example,dc=com）
    - `base_dn`: 搜索基准 DN（如：ou=users,dc=example,dc=com）
    - `has_bind_password`: 是否已设置绑定密码（布尔值）
    - `user_search_filter`: 用户搜索过滤器（默认：(uid={username})）
    - `username_attr`: 用户名属性（默认：uid）
    - `email_attr`: 邮箱属性（默认：mail）
    - `display_name_attr`: 显示名称属性（默认：cn）
    - `is_enabled`: 是否启用 LDAP 认证
    - `is_exclusive`: 是否仅允许 LDAP 登录（独占模式）
    - `use_starttls`: 是否使用 STARTTLS 加密连接
    - `connect_timeout`: 连接超时时间（秒，1-60）
    """
    adapter = AdminGetLDAPConfigAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.put("/config")
async def update_ldap_config(request: Request, db: Session = Depends(get_db)) -> Any:
    """
    更新 LDAP 配置

    更新系统的 LDAP 认证配置。支持完整配置更新，包括连接参数、
    搜索过滤器、属性映射等。提供多重安全校验，防止误锁定管理员。

    **请求体字段**:
    - `server_url`: LDAP 服务器地址（必填，1-255字符）
    - `bind_dn`: 绑定 DN（必填，1-255字符）
    - `bind_password`: 绑定密码（可选，设为空字符串可清除密码）
    - `base_dn`: 搜索基准 DN（必填，1-255字符）
    - `user_search_filter`: 用户搜索过滤器（必须包含 {username} 占位符，默认：(uid={username})）
    - `username_attr`: 用户名属性（默认：uid）
    - `email_attr`: 邮箱属性（默认：mail）
    - `display_name_attr`: 显示名称属性（默认：cn）
    - `is_enabled`: 是否启用 LDAP 认证
    - `is_exclusive`: 是否仅允许 LDAP 登录（需先启用 LDAP）
    - `use_starttls`: 是否使用 STARTTLS 加密连接
    - `connect_timeout`: 连接超时时间（秒，1-60，默认 10）

    **安全校验**:
    - 启用 LDAP 时必须设置有效的绑定密码
    - 启用独占模式前会检查是否有至少 1 个有效的本地管理员账户
    - 独占模式要求先启用 LDAP 认证
    - 搜索过滤器必须包含 {username} 占位符且括号匹配
    - 搜索过滤器嵌套层数不超过 5 层，长度不超过 200 字符

    **返回字段**:
    - `message`: 操作结果消息
    """
    adapter = AdminUpdateLDAPConfigAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


@router.post("/test")
async def test_ldap_connection(request: Request, db: Session = Depends(get_db)) -> Any:
    """
    测试 LDAP 连接

    在保存配置前测试 LDAP 服务器连接是否正常。支持使用已保存的配置，
    也支持通过请求体覆盖任意配置项进行临时测试，而不影响已保存的配置。

    **请求体字段**（均为可选，用于临时覆盖）:
    - `server_url`: LDAP 服务器地址（覆盖已保存的配置）
    - `bind_dn`: 绑定 DN（覆盖已保存的配置）
    - `bind_password`: 绑定密码（覆盖已保存的密码）
    - `base_dn`: 搜索基准 DN（覆盖已保存的配置）
    - `user_search_filter`: 用户搜索过滤器（覆盖已保存的配置）
    - `username_attr`: 用户名属性（覆盖已保存的配置）
    - `email_attr`: 邮箱属性（覆盖已保存的配置）
    - `display_name_attr`: 显示名称属性（覆盖已保存的配置）
    - `use_starttls`: 是否使用 STARTTLS（覆盖已保存的配置）
    - `connect_timeout`: 连接超时时间（覆盖已保存的配置）

    **测试逻辑**:
    - 未提供的字段使用已保存的配置值
    - `bind_password` 优先使用请求体中的值，否则使用已保存的加密密码
    - 测试时会尝试连接 LDAP 服务器并验证绑定 DN

    **返回字段**:
    - `success`: 测试是否成功（布尔值）
    - `message`: 测试结果消息（成功或失败原因）
    """
    adapter = AdminTestLDAPConnectionAdapter()
    return await pipeline.run(adapter=adapter, http_request=request, db=db, mode=adapter.mode)


# ========== Adapters ==========


class AdminGetLDAPConfigAdapter(AdminApiAdapter):
    async def handle(self, context: ApiRequestContext) -> dict[str, Any]:  # type: ignore[override]
        db = context.db
        config = db.query(LDAPConfig).first()

        if not config:
            return LDAPConfigResponse(
                server_url=None,
                bind_dn=None,
                base_dn=None,
                has_bind_password=False,
                user_search_filter="(uid={username})",
                username_attr="uid",
                email_attr="mail",
                display_name_attr="cn",
                is_enabled=False,
                is_exclusive=False,
                use_starttls=False,
                connect_timeout=10,
            ).model_dump()

        return LDAPConfigResponse(
            server_url=config.server_url,
            bind_dn=config.bind_dn,
            base_dn=config.base_dn,
            has_bind_password=bool(config.bind_password_encrypted),
            user_search_filter=config.user_search_filter,
            username_attr=config.username_attr,
            email_attr=config.email_attr,
            display_name_attr=config.display_name_attr,
            is_enabled=config.is_enabled,
            is_exclusive=config.is_exclusive,
            use_starttls=config.use_starttls,
            connect_timeout=config.connect_timeout,
        ).model_dump()


class AdminUpdateLDAPConfigAdapter(AdminApiAdapter):
    async def handle(self, context: ApiRequestContext) -> dict[str, str]:  # type: ignore[override]
        db = context.db
        payload = context.ensure_json_body()

        try:
            config_update = LDAPConfigUpdate.model_validate(payload)
        except ValidationError as e:
            errors = e.errors()
            if errors:
                raise InvalidRequestException(translate_pydantic_error(errors[0]))
            raise InvalidRequestException("请求数据验证失败")

        # 使用行级锁防止并发修改导致的竞态条件
        config = db.query(LDAPConfig).with_for_update().first()
        is_new_config = config is None

        if is_new_config:
            # 首次创建配置时必须提供密码
            if not config_update.bind_password:
                raise InvalidRequestException("首次配置 LDAP 时必须设置绑定密码")
            config = LDAPConfig()
            db.add(config)

        # 需要启用 LDAP 且未提交新密码时，验证已保存密码可解密（避免开启后不可用）
        if config_update.is_enabled and config_update.bind_password is None:
            try:
                if not config.get_bind_password():
                    raise InvalidRequestException("启用 LDAP 认证 需要先设置绑定密码")
            except InvalidRequestException:
                raise
            except Exception:
                raise InvalidRequestException("绑定密码解密失败，请重新设置绑定密码")

        # 计算更新后的密码状态（用于校验是否可启用/独占）
        if config_update.bind_password is None:
            will_have_password = bool(config.bind_password_encrypted)
        elif config_update.bind_password == "":
            will_have_password = False
        else:
            will_have_password = True

        # 独占模式必须启用 LDAP 且必须有绑定密码（防止误锁定）
        if config_update.is_exclusive and not config_update.is_enabled:
            raise InvalidRequestException("仅允许 LDAP 登录 需要先启用 LDAP 认证")
        if config_update.is_enabled and not will_have_password:
            raise InvalidRequestException("启用 LDAP 认证 需要先设置绑定密码")
        if config_update.is_exclusive and not will_have_password:
            raise InvalidRequestException("仅允许 LDAP 登录 需要先设置绑定密码")

        config.server_url = config_update.server_url
        config.bind_dn = config_update.bind_dn
        config.base_dn = config_update.base_dn
        config.user_search_filter = config_update.user_search_filter
        config.username_attr = config_update.username_attr
        config.email_attr = config_update.email_attr
        config.display_name_attr = config_update.display_name_attr
        config.is_enabled = config_update.is_enabled
        config.is_exclusive = config_update.is_exclusive
        config.use_starttls = config_update.use_starttls
        config.connect_timeout = config_update.connect_timeout

        # 启用独占模式前检查是否有足够的本地管理员（防止锁定）
        # 使用 with_for_update() 阻塞锁防止竞态条件（移除 nowait 确保并发安全）
        if config_update.is_enabled and config_update.is_exclusive:
            local_admins = (
                db.query(User)
                .filter(
                    User.role == UserRole.ADMIN,
                    User.auth_source == AuthSource.LOCAL,
                    User.is_active.is_(True),
                    User.is_deleted.is_(False),
                )
                .with_for_update()
                .all()
            )
            # 验证至少有一个管理员有有效的密码哈希（可以登录）
            # 使用严格的 bcrypt 格式校验：$2a$/$2b$/$2y$ + 2位cost + $ + 53字符
            valid_admin_count = sum(
                1
                for admin in local_admins
                if admin.password_hash
                and isinstance(admin.password_hash, str)
                and BCRYPT_HASH_PATTERN.match(admin.password_hash)
            )
            if valid_admin_count < 1:
                raise InvalidRequestException(
                    "启用 LDAP 独占模式前，必须至少保留 1 个有效的本地管理员账户（含有效密码）作为紧急恢复通道"
                )

        if config_update.bind_password is not None:
            if config_update.bind_password == "":
                # 显式清除密码（设置为 NULL）
                config.bind_password_encrypted = None
                password_changed = "cleared"
            else:
                config.bind_password_encrypted = crypto_service.encrypt(config_update.bind_password)
                password_changed = "updated"
        else:
            password_changed = None

        db.commit()

        # 记录审计日志
        AuditService.log_event(
            db=db,
            event_type=AuditEventType.CONFIG_CHANGED,
            description=f"LDAP 配置已更新 (enabled={config_update.is_enabled}, exclusive={config_update.is_exclusive})",
            user_id=str(context.user.id) if context.user else None,
            metadata={
                "server_url": config_update.server_url,
                "is_enabled": config_update.is_enabled,
                "is_exclusive": config_update.is_exclusive,
                "password_changed": password_changed,
                "is_new_config": is_new_config,
            },
        )

        return {"message": "LDAP配置更新成功"}


class AdminTestLDAPConnectionAdapter(AdminApiAdapter):
    async def handle(self, context: ApiRequestContext) -> dict[str, Any]:  # type: ignore[override]
        from src.services.auth.ldap import LDAPService

        db = context.db
        if context.json_body is not None:
            payload = context.json_body
        elif not context.raw_body:
            payload = {}
        else:
            payload = context.ensure_json_body()

        saved_config = db.query(LDAPConfig).first()

        try:
            overrides = LDAPConfigTest.model_validate(payload)
        except ValidationError as e:
            errors = e.errors()
            if errors:
                raise InvalidRequestException(translate_pydantic_error(errors[0]))
            raise InvalidRequestException("请求数据验证失败")

        config_data: dict[str, Any] = {}

        if saved_config:
            config_data = {
                "server_url": saved_config.server_url,
                "bind_dn": saved_config.bind_dn,
                "base_dn": saved_config.base_dn,
                "user_search_filter": saved_config.user_search_filter,
                "username_attr": saved_config.username_attr,
                "email_attr": saved_config.email_attr,
                "display_name_attr": saved_config.display_name_attr,
                "use_starttls": saved_config.use_starttls,
                "connect_timeout": saved_config.connect_timeout,
            }

        # 应用前端传入的覆盖值
        for field in [
            "server_url",
            "bind_dn",
            "base_dn",
            "user_search_filter",
            "username_attr",
            "email_attr",
            "display_name_attr",
            "use_starttls",
            "is_enabled",
            "is_exclusive",
            "connect_timeout",
        ]:
            value = getattr(overrides, field)
            if value is not None:
                config_data[field] = value

        # bind_password 优先使用 overrides；否则使用已保存的密码（允许保存密码无法解密时依然用 overrides 测试）
        if overrides.bind_password is not None:
            config_data["bind_password"] = overrides.bind_password
        elif saved_config and saved_config.bind_password_encrypted:
            try:
                config_data["bind_password"] = crypto_service.decrypt(
                    saved_config.bind_password_encrypted
                )
            except Exception as e:
                logger.error(f"绑定密码解密失败: {type(e).__name__}: {e}")
                return LDAPTestResponse(
                    success=False, message="绑定密码解密失败，请检查配置或重新设置密码"
                ).model_dump()

        # 必填字段检查
        required_fields = ["server_url", "bind_dn", "base_dn", "bind_password"]
        missing = [f for f in required_fields if not config_data.get(f)]
        if missing:
            return LDAPTestResponse(
                success=False, message=f"缺少必要字段: {', '.join(missing)}"
            ).model_dump()

        success, message = LDAPService.test_connection_with_config(config_data)
        return LDAPTestResponse(success=success, message=message).model_dump()
