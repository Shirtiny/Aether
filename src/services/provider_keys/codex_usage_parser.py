"""
Codex 配额响应解析器。
"""

from __future__ import annotations

import time
from collections.abc import Mapping
from typing import Any


class CodexUsageParseError(ValueError):
    """Codex 配额响应结构异常。"""


def _raise_type_error(field: str, expected: str, value: Any) -> None:
    raise CodexUsageParseError(
        f"{field} 类型错误，期望 {expected}，实际 {type(value).__name__}: {value!r}"
    )


def _as_dict(value: Any, field: str) -> dict[str, Any]:
    if value is None:
        return {}
    if not isinstance(value, dict):
        _raise_type_error(field, "object", value)
    return value


def _coerce_float(value: Any, field: str) -> float:
    if isinstance(value, bool):
        _raise_type_error(field, "number", value)
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        raw = value.strip()
        if not raw:
            _raise_type_error(field, "number", value)
        try:
            return float(raw)
        except ValueError as exc:  # pragma: no cover - 仅防御
            raise CodexUsageParseError(f"{field} 不是合法数字: {value!r}") from exc
    _raise_type_error(field, "number", value)


def _coerce_int(value: Any, field: str) -> int:
    if isinstance(value, bool):
        _raise_type_error(field, "integer", value)
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        if not value.is_integer():
            raise CodexUsageParseError(f"{field} 必须是整数，实际为小数: {value!r}")
        return int(value)
    if isinstance(value, str):
        raw = value.strip()
        if not raw:
            _raise_type_error(field, "integer", value)
        try:
            if "." in raw or "e" in raw.lower():
                parsed = float(raw)
                if not parsed.is_integer():
                    raise CodexUsageParseError(f"{field} 必须是整数，实际为小数: {value!r}")
                return int(parsed)
            return int(raw)
        except ValueError as exc:  # pragma: no cover - 仅防御
            raise CodexUsageParseError(f"{field} 不是合法整数: {value!r}") from exc
    _raise_type_error(field, "integer", value)


def _coerce_bool(value: Any, field: str) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, int):
        if value in (0, 1):
            return bool(value)
        raise CodexUsageParseError(f"{field} 仅支持 0/1 整数，实际为: {value!r}")
    if isinstance(value, str):
        normalized = value.strip().lower()
        if normalized in {"true", "1", "yes"}:
            return True
        if normalized in {"false", "0", "no"}:
            return False
        raise CodexUsageParseError(f"{field} 不是合法布尔值: {value!r}")
    _raise_type_error(field, "boolean", value)


def _write_window(
    result: dict[str, Any],
    *,
    source: dict[str, Any],
    source_field: str,
    target_prefix: str,
) -> None:
    if not source:
        return

    used_percent = source.get("used_percent")
    if used_percent is not None:
        result[f"{target_prefix}_used_percent"] = _coerce_float(
            used_percent, f"{source_field}.used_percent"
        )

    reset_seconds = source.get("reset_after_seconds")
    if reset_seconds is not None:
        result[f"{target_prefix}_reset_seconds"] = _coerce_int(
            reset_seconds, f"{source_field}.reset_after_seconds"
        )

    reset_at = source.get("reset_at")
    if reset_at is not None:
        result[f"{target_prefix}_reset_at"] = _coerce_int(reset_at, f"{source_field}.reset_at")

    limit_window_seconds = source.get("limit_window_seconds")
    if limit_window_seconds is not None:
        result[f"{target_prefix}_window_minutes"] = (
            _coerce_int(limit_window_seconds, f"{source_field}.limit_window_seconds") // 60
        )


def _normalize_plan_type(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    normalized = value.strip().lower()
    return normalized or None


def _is_blank_string(value: Any) -> bool:
    return isinstance(value, str) and not value.strip()


def _coerce_optional_float(value: Any, field: str) -> float | None:
    if value is None or _is_blank_string(value):
        return None
    return _coerce_float(value, field)


def _coerce_optional_int(value: Any, field: str) -> int | None:
    if value is None or _is_blank_string(value):
        return None
    return _coerce_int(value, field)


def _coerce_optional_bool(value: Any, field: str) -> bool | None:
    if value is None or _is_blank_string(value):
        return None
    return _coerce_bool(value, field)


def _normalize_header_map(headers: Mapping[str, Any]) -> dict[str, Any]:
    normalized: dict[str, Any] = {}
    for raw_key, raw_value in headers.items():
        key = str(raw_key).strip().lower()
        if not key:
            continue
        normalized[key] = raw_value
    return normalized


def _read_header_window(
    *,
    headers: Mapping[str, Any],
    used_percent_key: str,
    reset_seconds_key: str,
    reset_at_key: str,
    window_minutes_key: str,
    source_field: str,
) -> dict[str, Any]:
    window: dict[str, Any] = {}

    used_percent = _coerce_optional_float(
        headers.get(used_percent_key),
        f"{source_field}.used_percent",
    )
    if used_percent is not None:
        window["used_percent"] = used_percent

    reset_seconds = _coerce_optional_int(
        headers.get(reset_seconds_key),
        f"{source_field}.reset_after_seconds",
    )
    if reset_seconds is not None:
        window["reset_after_seconds"] = reset_seconds

    reset_at = _coerce_optional_int(headers.get(reset_at_key), f"{source_field}.reset_at")
    if reset_at is not None:
        window["reset_at"] = reset_at

    window_minutes = _coerce_optional_int(
        headers.get(window_minutes_key),
        f"{source_field}.limit_window_minutes",
    )
    if window_minutes is not None:
        window["limit_window_seconds"] = window_minutes * 60

    return window


def parse_codex_wham_usage_response(data: dict[str, Any]) -> dict[str, Any] | None:
    """
    解析 Codex wham/usage API 响应，提取限额信息

    Free 账号:
    - rate_limit.primary_window: 周限额

    Team/Plus/Enterprise 账号:
    - rate_limit.primary_window: 5H 限额
    - rate_limit.secondary_window: 周限额
    """
    if data is None:
        return None
    if not isinstance(data, dict):
        _raise_type_error("root", "object", data)
    if not data:
        return None

    result: dict[str, Any] = {}

    plan_type: str | None = None
    raw_plan_type = data.get("plan_type")
    if raw_plan_type is not None:
        if not isinstance(raw_plan_type, str):
            _raise_type_error("plan_type", "string", raw_plan_type)
        normalized_plan_type = _normalize_plan_type(raw_plan_type)
        if normalized_plan_type:
            plan_type = normalized_plan_type
            result["plan_type"] = normalized_plan_type

    # 解析 rate_limit
    rate_limit = _as_dict(data.get("rate_limit"), "rate_limit")
    primary_window = _as_dict(rate_limit.get("primary_window"), "rate_limit.primary_window")
    secondary_window = _as_dict(rate_limit.get("secondary_window"), "rate_limit.secondary_window")

    # 根据账号类型解析限额
    # Free 账号: primary_window 是周限额，无 secondary_window
    # Team/Plus/Enterprise: primary_window 是 5H 限额，secondary_window 是周限额
    use_paid_windows = bool(secondary_window) and plan_type != "free"
    if use_paid_windows:
        # 周限额 (secondary_window)
        _write_window(
            result,
            source=secondary_window,
            source_field="rate_limit.secondary_window",
            target_prefix="primary",
        )
        # 5H 限额 (primary_window)
        _write_window(
            result,
            source=primary_window,
            source_field="rate_limit.primary_window",
            target_prefix="secondary",
        )
    else:
        # Free / 或 secondary_window 缺失时，primary_window 视为主窗口
        _write_window(
            result,
            source=primary_window,
            source_field="rate_limit.primary_window",
            target_prefix="primary",
        )

    # 解析 credits
    credits = _as_dict(data.get("credits"), "credits")
    has_credits = credits.get("has_credits")
    if has_credits is not None:
        result["has_credits"] = _coerce_bool(has_credits, "credits.has_credits")

    credits_balance = _coerce_optional_float(credits.get("balance"), "credits.balance")
    if credits_balance is not None:
        result["credits_balance"] = credits_balance

    credits_unlimited = _coerce_optional_bool(credits.get("unlimited"), "credits.unlimited")
    if credits_unlimited is not None:
        result["credits_unlimited"] = credits_unlimited

    # 添加更新时间戳
    if result:
        result["updated_at"] = int(time.time())

    return result if result else None


def parse_codex_usage_headers(headers: Mapping[str, Any] | None) -> dict[str, Any] | None:
    """
    解析 Codex 反代响应头中的配额信息，提取账号配额（不依赖 code review 字段）。

    Team/Plus/Enterprise:
    - x-codex-primary-*   : 5H 限额
    - x-codex-secondary-* : 周限额

    Free:
    - x-codex-primary-*   : 周限额
    """
    if headers is None:
        return None
    if not isinstance(headers, Mapping):
        _raise_type_error("headers", "object", headers)
    if not headers:
        return None

    normalized_headers = _normalize_header_map(headers)
    if not any(k.startswith("x-codex-") for k in normalized_headers):
        return None

    result: dict[str, Any] = {}

    plan_type = _normalize_plan_type(normalized_headers.get("x-codex-plan-type"))
    if plan_type:
        result["plan_type"] = plan_type

    primary_window = _read_header_window(
        headers=normalized_headers,
        used_percent_key="x-codex-primary-used-percent",
        reset_seconds_key="x-codex-primary-reset-after-seconds",
        reset_at_key="x-codex-primary-reset-at",
        window_minutes_key="x-codex-primary-window-minutes",
        source_field="headers.primary_window",
    )
    secondary_window = _read_header_window(
        headers=normalized_headers,
        used_percent_key="x-codex-secondary-used-percent",
        reset_seconds_key="x-codex-secondary-reset-after-seconds",
        reset_at_key="x-codex-secondary-reset-at",
        window_minutes_key="x-codex-secondary-window-minutes",
        source_field="headers.secondary_window",
    )
    # 与 wham/usage 解析保持一致：
    # - metadata.primary_* 统一表示周限额
    # - metadata.secondary_* 统一表示 5H 限额
    use_paid_windows = bool(secondary_window) and plan_type != "free"
    if use_paid_windows:
        _write_window(
            result,
            source=secondary_window,
            source_field="headers.secondary_window",
            target_prefix="primary",
        )
        _write_window(
            result,
            source=primary_window,
            source_field="headers.primary_window",
            target_prefix="secondary",
        )
    else:
        _write_window(
            result,
            source=primary_window,
            source_field="headers.primary_window",
            target_prefix="primary",
        )

    # 当前窗口挤占占比（有值才记录）
    primary_over_secondary_limit = _coerce_optional_float(
        normalized_headers.get("x-codex-primary-over-secondary-limit-percent"),
        "headers.primary_over_secondary_limit_percent",
    )
    if primary_over_secondary_limit is not None:
        result["primary_over_secondary_limit_percent"] = primary_over_secondary_limit

    has_credits = _coerce_optional_bool(
        normalized_headers.get("x-codex-credits-has-credits"),
        "headers.credits.has_credits",
    )
    if has_credits is not None:
        result["has_credits"] = has_credits

    credits_balance = _coerce_optional_float(
        normalized_headers.get("x-codex-credits-balance"),
        "headers.credits.balance",
    )
    if credits_balance is not None:
        result["credits_balance"] = credits_balance

    credits_unlimited = _coerce_optional_bool(
        normalized_headers.get("x-codex-credits-unlimited"),
        "headers.credits.unlimited",
    )
    if credits_unlimited is not None:
        result["credits_unlimited"] = credits_unlimited

    if result:
        result["updated_at"] = int(time.time())

    return result if result else None
