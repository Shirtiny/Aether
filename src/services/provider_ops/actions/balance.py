"""
余额查询操作抽象基类
"""

import time
from abc import abstractmethod
from typing import Any

import httpx

from src.core.cache_service import CacheService
from src.core.logger import logger as _logger
from src.services.provider_ops.actions.base import ProviderAction
from src.services.provider_ops.types import (
    ActionResult,
    ActionStatus,
    BalanceInfo,
    ProviderActionType,
)

# 签到缓存 TTL（6 小时）
_CHECKIN_CACHE_TTL = 6 * 3600


class BalanceAction(ProviderAction):
    """
    余额查询操作抽象基类

    子类必须实现 _parse_balance() 方法来处理特定平台的余额解析逻辑。
    子类可选重写 _do_query_balance() 或 _do_checkin() 进行自定义。

    如果子类使用 Cookie 认证，设置 _cookie_auth = True 可让 401/403 错误
    显示 "Cookie 已失效" 而非 "认证失败"。
    """

    action_type = ProviderActionType.QUERY_BALANCE
    display_name = "查询余额"
    description = "查询账户余额信息"
    default_cache_ttl = 86400  # 24 小时

    # 子类设为 True 即可在 401/403 时显示 "Cookie 已失效" 消息
    _cookie_auth: bool = False

    async def execute(self, client: httpx.AsyncClient) -> ActionResult:
        """
        执行余额查询（模板方法）

        1. 先尝试签到（如果子类实现了 _do_checkin），带缓存冷却
        2. 执行余额查询

        Args:
            client: 已认证的 HTTP 客户端

        Returns:
            ActionResult，其中 data 字段为 BalanceInfo
        """
        # 先尝试签到（带缓存冷却）
        checkin_result = await self._get_or_do_checkin(client)

        # 执行余额查询
        result = await self._do_query_balance(client)

        # 将签到结果附加到 extra 字段
        if checkin_result and result.data and hasattr(result.data, "extra"):
            if result.data.extra is None:
                result.data.extra = {}
            # 处理 cookie_expired 标记
            if checkin_result.get("cookie_expired"):
                result.data.extra["cookie_expired"] = True
                result.data.extra["cookie_expired_message"] = checkin_result.get("message", "")
                result.status = ActionStatus.AUTH_EXPIRED
                _logger.warning("Cookie 已失效: {}", checkin_result)
            else:
                result.data.extra["checkin_success"] = checkin_result.get("success")
                result.data.extra["checkin_message"] = checkin_result.get("message", "")
                _logger.debug("签到结果已附加到 extra: {}", checkin_result)

        return result

    async def _do_query_balance(self, client: httpx.AsyncClient) -> ActionResult:
        """
        执行余额查询

        默认实现处理通用的请求/响应/错误处理流程。
        子类只需实现 _parse_balance() 即可。
        如果查询逻辑不同（如并发调用多个接口），子类可重写此方法。

        Args:
            client: 已认证的 HTTP 客户端

        Returns:
            ActionResult，其中 data 字段为 BalanceInfo
        """
        endpoint = self.config.get("endpoint", "/api/user/self")
        method = self.config.get("method", "GET")

        start_time = time.time()

        try:
            response = await client.request(method, endpoint)
            response_time_ms = int((time.time() - start_time) * 1000)

            try:
                data = response.json()
            except Exception:
                return self._make_error_result(
                    ActionStatus.PARSE_ERROR,
                    "响应不是有效的 JSON",
                )

            if response.status_code != 200:
                return self._handle_http_error(response, data)

            if data.get("success") is False:
                message = data.get("message", "业务状态码表示失败")
                return self._make_error_result(
                    ActionStatus.UNKNOWN_ERROR,
                    message,
                    raw_response=data,
                )

            balance = self._parse_balance(data)

            return self._make_success_result(
                data=balance,
                response_time_ms=response_time_ms,
                raw_response=data,
            )

        except httpx.TimeoutException:
            return self._make_error_result(
                ActionStatus.NETWORK_ERROR,
                "请求超时",
                retry_after_seconds=30,
            )
        except httpx.RequestError as e:
            return self._make_error_result(
                ActionStatus.NETWORK_ERROR,
                f"网络错误: {str(e)}",
                retry_after_seconds=30,
            )
        except Exception as e:
            return self._make_error_result(
                ActionStatus.UNKNOWN_ERROR,
                f"未知错误: {str(e)}",
            )

    @abstractmethod
    def _parse_balance(self, data: Any) -> BalanceInfo:
        """
        解析余额数据（子类必须实现）

        Args:
            data: API 响应 JSON 数据

        Returns:
            BalanceInfo 对象
        """
        pass

    def _handle_http_error(
        self, response: httpx.Response, raw_data: dict[str, Any] | None = None
    ) -> ActionResult:
        """
        处理 HTTP 错误响应

        Cookie 认证的子类设置 _cookie_auth = True 即可获得友好的错误提示，
        无需再逐个重写此方法。
        """
        status_code = response.status_code

        if status_code == 401:
            msg = "Cookie 已失效，请重新配置" if self._cookie_auth else "认证失败"
            return self._make_error_result(ActionStatus.AUTH_FAILED, msg, raw_response=raw_data)
        elif status_code == 403:
            msg = "Cookie 已失效或无权限" if self._cookie_auth else "无权限访问"
            return self._make_error_result(ActionStatus.AUTH_FAILED, msg, raw_response=raw_data)
        elif status_code == 404:
            return self._make_error_result(
                ActionStatus.NOT_SUPPORTED, "功能未开放", raw_response=raw_data
            )
        elif status_code == 429:
            retry_after = response.headers.get("Retry-After")
            return self._make_error_result(
                ActionStatus.RATE_LIMITED,
                "请求频率限制",
                retry_after_seconds=int(retry_after) if retry_after else 60,
                raw_response=raw_data,
            )
        else:
            return self._make_error_result(
                ActionStatus.UNKNOWN_ERROR,
                f"HTTP {status_code}: {response.reason_phrase}",
                raw_response=raw_data,
            )

    async def _get_or_do_checkin(self, client: httpx.AsyncClient) -> dict[str, Any] | None:
        """
        带缓存冷却的签到：优先返回缓存结果，未命中才实际执行签到。

        缓存 key 使用 host（同一站点多个 provider 只需签到一次），TTL 6 小时。
        签到失败或 cookie_expired 不写入缓存，允许下次重试。
        """
        host = str(client.base_url.host or client.base_url.netloc or client.base_url)
        cache_key = f"provider_ops:checkin:{host}"

        # 检查缓存
        try:
            cached = await CacheService.get(cache_key)
            if cached is not None:
                _logger.debug("[{}] 签到缓存命中，跳过签到: {}", host, cached)
                return cached
        except Exception:
            pass

        # 缓存未命中，执行签到
        result = await self._do_checkin(client)

        # 签到成功或"已签到"时写入缓存；失败/cookie_expired 不缓存
        if result is not None and not result.get("cookie_expired"):
            success = result.get("success")
            if success is True or success is None:
                try:
                    await CacheService.set(cache_key, result, _CHECKIN_CACHE_TTL)
                except Exception:
                    pass

        return result

    async def _do_checkin(self, client: httpx.AsyncClient) -> dict[str, Any] | None:
        """
        执行签到（子类可选实现）

        默认实现返回 None（不签到）。
        子类可重写此方法实现平台特定的签到逻辑。

        Args:
            client: 已认证的 HTTP 客户端

        Returns:
            签到结果字典 {"success": bool, "message": str}，或 None 表示不签到
        """
        return None

    def _create_balance_info(
        self,
        total_granted: float | None = None,
        total_used: float | None = None,
        total_available: float | None = None,
        currency: str = "USD",
        extra: dict[str, Any] | None = None,
    ) -> BalanceInfo:
        """
        创建余额信息对象

        辅助方法，用于创建统一格式的 BalanceInfo。
        如果只有部分数据，会尝试计算缺失的值。

        Args:
            total_granted: 总额度
            total_used: 已用额度
            total_available: 可用余额
            currency: 货币单位
            extra: 额外信息

        Returns:
            BalanceInfo 对象
        """
        # 如果只有部分数据，尝试计算
        if total_available is None and total_granted is not None and total_used is not None:
            total_available = total_granted - total_used
        if total_used is None and total_granted is not None and total_available is not None:
            total_used = total_granted - total_available
        if total_granted is None and total_used is not None and total_available is not None:
            total_granted = total_used + total_available

        return BalanceInfo(
            total_granted=total_granted,
            total_used=total_used,
            total_available=total_available,
            currency=currency,
            extra=extra if extra is not None else {},
        )

    def _to_float(self, value: Any) -> float | None:
        """转换为浮点数"""
        if value is None:
            return None
        try:
            return float(value)
        except (TypeError, ValueError):
            return None

    @classmethod
    def get_config_schema(cls) -> dict[str, Any]:
        """获取操作配置 schema（子类可重写）"""
        return {
            "type": "object",
            "properties": {
                "currency": {
                    "type": "string",
                    "title": "货币单位",
                    "default": "USD",
                },
            },
            "required": [],
        }
