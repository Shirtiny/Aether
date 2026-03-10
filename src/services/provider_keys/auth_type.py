"""
Provider Key 认证类型相关规则。
"""

# 数据库中所有属于 OAuth 的 auth_type 值（含历史别名）。
# 新增 OAuth 类型时只需在此追加，SQL 过滤和 Python 判断均引用此常量。
OAUTH_AUTH_TYPES: tuple[str, ...] = ("oauth", "kiro")


def normalize_auth_type(raw: str) -> str:
    """将数据库中的 auth_type 归一化为逻辑类型。

    - ``"kiro"`` -> ``"oauth"``  (Kiro 使用 OAuth 流程)
    - ``"vertex_ai"`` -> ``"service_account"``  (旧的 Vertex AI auth_type 已重命名)
    """
    t = str(raw or "api_key").strip() or "api_key"
    if t == "kiro":  # TODO: 迁移稳定后清理，同步清理各处 in ("...", "kiro") 兼容检查
        return "oauth"
    if t == "vertex_ai":  # TODO: 迁移稳定后清理，同步清理各处 in ("...", "vertex_ai") 兼容检查
        return "service_account"
    return t
