"""
字段映射配置（集中定义）

该文件用于承载：
- role/stop_reason/usage/error 的常见映射表

注意：
- conversion 层只负责 body 结构转换，不维护 model_in_body/stream_in_body/auth_header 等元数据；
  这些应复用 `src/core/api_format/metadata.py`（API_FORMAT_DEFINITIONS）作为单一事实来源。
"""

# 角色映射（仅作为辅助；system/tool 的具体落点以 Normalizer 规则为准）
ROLE_MAPPINGS: dict[str, dict[str, str]] = {
    "OPENAI": {
        "user": "user",
        "assistant": "assistant",
        "system": "system",
        "developer": "developer",
        "tool": "tool",
    },
    "CLAUDE": {"user": "user", "assistant": "assistant"},
    "GEMINI": {"user": "user", "assistant": "model"},
}


# 停止原因映射（internal -> provider），未知值使用 UNKNOWN 并写入 extra/raw
STOP_REASON_MAPPINGS: dict[str, dict[str, str]] = {
    "CLAUDE": {
        "end_turn": "end_turn",
        "max_tokens": "max_tokens",
        "stop_sequence": "stop_sequence",
        "tool_use": "tool_use",
        "pause_turn": "end_turn",
        "refusal": "end_turn",
        # Claude 通常以错误/阻断体现，这里仅兜底
        "content_filtered": "end_turn",
        "unknown": "end_turn",
    },
    "OPENAI": {
        "end_turn": "stop",
        "max_tokens": "length",
        "stop_sequence": "stop",
        "tool_use": "tool_calls",
        "content_filtered": "content_filter",
        "refusal": "content_filter",
        "pause_turn": "stop",
        "unknown": "stop",
    },
    "GEMINI": {
        "end_turn": "STOP",
        "max_tokens": "MAX_TOKENS",
        "stop_sequence": "STOP",
        # Gemini finishReason 对工具调用并没有稳定等价枚举，这里保守兜底为 STOP
        "tool_use": "STOP",
        "content_filtered": "SAFETY",
        "refusal": "SAFETY",
        "pause_turn": "OTHER",
        "unknown": "OTHER",
    },
}


# 使用量字段映射（provider usage field -> internal UsageInfo field）
USAGE_FIELD_MAPPINGS: dict[str, dict[str, str]] = {
    "CLAUDE": {
        "input_tokens": "input_tokens",
        "output_tokens": "output_tokens",
        "cache_read_input_tokens": "cache_read_tokens",
        "cache_creation_input_tokens": "cache_write_tokens",
    },
    "OPENAI": {
        "prompt_tokens": "input_tokens",
        "completion_tokens": "output_tokens",
        "total_tokens": "total_tokens",
    },
    "GEMINI": {
        "promptTokenCount": "input_tokens",
        "candidatesTokenCount": "output_tokens",
        "totalTokenCount": "total_tokens",
        "cachedContentTokenCount": "cache_read_tokens",
    },
}


# 错误类型映射（provider -> internal ErrorType.value）
ERROR_TYPE_MAPPINGS: dict[str, dict[str, str]] = {
    "CLAUDE": {
        "invalid_request_error": "invalid_request",
        "authentication_error": "authentication",
        "permission_error": "permission_denied",
        "not_found_error": "not_found",
        "rate_limit_error": "rate_limit",
        "timeout_error": "server_error",
        "overloaded_error": "overloaded",
        "billing_error": "permission_denied",
        "api_error": "server_error",
    },
    "OPENAI": {
        "invalid_request_error": "invalid_request",
        "invalid_api_key": "authentication",
        "insufficient_quota": "rate_limit",
        "rate_limit_exceeded": "rate_limit",
        "server_error": "server_error",
        "context_length_exceeded": "context_length_exceeded",
        "content_policy_violation": "content_filtered",
    },
    "GEMINI": {
        "INVALID_ARGUMENT": "invalid_request",
        "UNAUTHENTICATED": "authentication",
        "PERMISSION_DENIED": "permission_denied",
        "NOT_FOUND": "not_found",
        "RESOURCE_EXHAUSTED": "rate_limit",
        "INTERNAL": "server_error",
        "UNAVAILABLE": "overloaded",
    },
}


# 可重试的错误类型（internal ErrorType.value）
RETRYABLE_ERROR_TYPES: set[str] = {
    "rate_limit",
    "overloaded",
    "server_error",
}


# OpenAI reasoning_effort -> thinking budget_tokens
# 参考 new-api relay-claude.go:178-196
REASONING_EFFORT_TO_THINKING_BUDGET: dict[str, int] = {
    "low": 1280,
    "medium": 2048,
    "high": 4096,
    "xhigh": 8192,
}

# thinking budget_tokens -> OpenAI reasoning_effort（反向映射，取最近区间）
THINKING_BUDGET_TO_REASONING_EFFORT: list[tuple[int, str]] = [
    (1664, "low"),  # <= 1664 -> low  (midpoint of 1280..2048)
    (3072, "medium"),  # <= 3072 -> medium  (midpoint of 2048..4096)
    (6144, "high"),  # <= 6144 -> high  (midpoint of 4096..8192)
    (2**31, "xhigh"),  # > 6144 -> xhigh
]

# Claude output_config.effort -> 标准化 reasoning_effort
CLAUDE_EFFORT_TO_REASONING_EFFORT: dict[str, str] = {
    "low": "low",
    "medium": "medium",
    "high": "high",
    "max": "xhigh",
    # "auto" 不映射，让目标格式使用默认行为
}

# 标准化 reasoning_effort -> Claude output_config.effort
REASONING_EFFORT_TO_CLAUDE_EFFORT: dict[str, str] = {
    "low": "low",
    "medium": "medium",
    "high": "high",
    "xhigh": "max",
}


# OpenAI web_search_options.search_context_size -> Claude web_search max_uses
WEB_SEARCH_CONTEXT_SIZE_TO_MAX_USES: dict[str, int] = {
    "low": 1,
    "medium": 5,
    "high": 10,
}


# Claude max_tokens 兜底默认值（仅在 GlobalModel.output_limit 和请求 max_tokens 均为空时使用）
# 参考 new-api setting/model_setting/claude.go 的 DefaultMaxTokens["default"]
CLAUDE_DEFAULT_MAX_TOKENS: int = 8192


def get_claude_default_max_tokens(_model: str) -> int:
    """获取 Claude 的 max_tokens 兜底默认值。

    正常情况下应优先使用 GlobalModel.config.output_limit（通过 InternalRequest.output_limit 传入），
    此函数仅在 output_limit 不可用时作为最终兜底。
    """
    return CLAUDE_DEFAULT_MAX_TOKENS


# thinking budget_tokens 占 max_tokens 的比例（参考 new-api: 0.8）
THINKING_BUDGET_TOKENS_PERCENTAGE: float = 0.8

# thinking budget_tokens 最小值（Claude API 要求 >= 1024）
THINKING_BUDGET_TOKENS_MIN: int = 1280


def get_claude_default_thinking_budget(model: str) -> int:
    """根据模型名称计算 thinking budget_tokens 默认值。

    budget = max(max_tokens * THINKING_BUDGET_TOKENS_PERCENTAGE, THINKING_BUDGET_TOKENS_MIN)
    """
    max_tokens = get_claude_default_max_tokens(model)
    return max(int(max_tokens * THINKING_BUDGET_TOKENS_PERCENTAGE), THINKING_BUDGET_TOKENS_MIN)


# 跨格式 thinking 转换时，非 Claude 模型的默认 budget_tokens 安全值
CROSS_FORMAT_THINKING_BUDGET_DEFAULT: int = 8192


__all__ = [
    "ROLE_MAPPINGS",
    "STOP_REASON_MAPPINGS",
    "USAGE_FIELD_MAPPINGS",
    "ERROR_TYPE_MAPPINGS",
    "RETRYABLE_ERROR_TYPES",
    "REASONING_EFFORT_TO_THINKING_BUDGET",
    "THINKING_BUDGET_TO_REASONING_EFFORT",
    "CLAUDE_EFFORT_TO_REASONING_EFFORT",
    "REASONING_EFFORT_TO_CLAUDE_EFFORT",
    "WEB_SEARCH_CONTEXT_SIZE_TO_MAX_USES",
    "CLAUDE_DEFAULT_MAX_TOKENS",
    "THINKING_BUDGET_TOKENS_PERCENTAGE",
    "THINKING_BUDGET_TOKENS_MIN",
    "CROSS_FORMAT_THINKING_BUDGET_DEFAULT",
    "get_claude_default_max_tokens",
    "get_claude_default_thinking_budget",
]
