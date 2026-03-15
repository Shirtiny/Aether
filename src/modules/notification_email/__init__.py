"""
通知邮件模块

提供错误通知邮件发送开关，并复用 SMTP 配置页。
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from src.core.modules.base import ModuleCategory, ModuleDefinition, ModuleMetadata

if TYPE_CHECKING:
    from sqlalchemy.orm import Session


def _validate_config(db: Session) -> tuple[bool, str]:
    """
    验证通知邮件模块配置。

    启用要求：SMTP 基础配置有效（至少 host + from_email）。
    """
    from src.services.email.email_sender import EmailSenderService

    if not EmailSenderService.is_smtp_configured(db):
        return False, "请先完成邮件配置（SMTP）"
    return True, ""


notification_email_module = ModuleDefinition(
    metadata=ModuleMetadata(
        name="notification_email",
        display_name="异常通知",
        description="为 5xx 异常发送邮件通知，可在模块管理中启用或禁用",
        category=ModuleCategory.INTEGRATION,
        env_key="NOTIFICATION_EMAIL_AVAILABLE",
        default_available=True,
        required_packages=[],
        # 通知邮件与 SMTP 配置复用同一页面，不在模块卡片展示独立“配置”入口。
        admin_route=None,
        admin_menu_icon="Mail",
        admin_menu_group="system",
        admin_menu_order=58,
    ),
    validate_config=_validate_config,
)
