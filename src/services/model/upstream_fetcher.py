"""
上游模型获取公共模块

提供从上游 API 获取模型列表的公共函数；通用 api_format 抓取能力统一来自 core.api_format.capabilities，供以下场景使用：
- 定时任务自动获取（fetch_scheduler.py）
- 管理后台手动查询（provider_query.py）
"""

import asyncio
from collections.abc import Iterable
from dataclasses import dataclass
from typing import Any, Awaitable, Callable

import httpx

from src.core.api_format import get_extra_headers_from_endpoint
from src.core.logger import logger

# 并发请求限制
MAX_CONCURRENT_REQUESTS = 5

# 模型获取格式优先级：同族内优先使用 chat 端点，若无则回退到 cli 端点
MODEL_FETCH_FORMAT_PRIORITY: list[tuple[str, ...]] = [
    ("openai:chat", "openai:cli", "openai:compact"),
    ("claude:chat", "claude:cli"),
    ("gemini:chat", "gemini:cli"),
]

# Return tuple signature:
# (models, errors, has_success, upstream_metadata)
_ModelsFetcher = Callable[
    ["UpstreamModelsFetchContext", float],
    Awaitable[tuple[list[dict], list[str], bool, dict[str, Any] | None]],
]


@dataclass(frozen=True, slots=True)
class EndpointFetchConfig:
    """端点获取配置（纯数据，不依赖 DB session）。

    从 ProviderEndpoint ORM 对象提取必要字段，确保在 DB session 关闭后
    仍可安全使用（避免 DetachedInstanceError）。
    """

    base_url: str
    extra_headers: dict[str, str] | None = None


def build_format_to_config(endpoints: Iterable[Any]) -> dict[str, EndpointFetchConfig]:
    """将活跃的 ProviderEndpoint 转换为 api_format -> EndpointFetchConfig 映射。

    应在 DB session 活跃时调用，提取 ORM 对象上的 base_url 和 header_rules，
    转换为 session 无关的纯数据结构。
    """
    result: dict[str, EndpointFetchConfig] = {}
    for ep in endpoints:
        if not getattr(ep, "is_active", False):
            continue
        result[ep.api_format] = EndpointFetchConfig(
            base_url=ep.base_url,
            extra_headers=get_extra_headers_from_endpoint(ep),
        )
    return result


@dataclass(frozen=True)
class UpstreamModelsFetchContext:
    """上游模型获取上下文（Key 级别）。"""

    provider_type: str
    api_key_value: str
    format_to_endpoint: dict[str, EndpointFetchConfig]
    proxy_config: dict[str, Any] | None = None
    auth_config: dict[str, Any] | None = None


class UpstreamModelsFetcherRegistry:
    """按 provider_type 注册上游模型获取策略，避免到处写特判。"""

    _fetchers: dict[str, _ModelsFetcher] = {}

    @classmethod
    def register(cls, *, provider_types: list[str], fetcher: _ModelsFetcher) -> None:
        for pt in provider_types:
            if not pt:
                continue
            cls._fetchers[pt.lower()] = fetcher

    @classmethod
    def get(cls, provider_type: str) -> _ModelsFetcher | None:
        if not provider_type:
            return None
        return cls._fetchers.get(provider_type.lower())


async def _fetch_models_default(
    ctx: UpstreamModelsFetchContext,
    timeout_seconds: float,
) -> tuple[list[dict], list[str], bool, dict[str, Any] | None]:
    endpoint_configs = build_all_format_configs(ctx.api_key_value, ctx.format_to_endpoint)
    models, errors, has_success = await fetch_models_from_endpoints(
        endpoint_configs, timeout=timeout_seconds, proxy_config=ctx.proxy_config
    )
    return models, errors, has_success, None


async def fetch_models_for_key(
    ctx: UpstreamModelsFetchContext,
    *,
    timeout_seconds: float = 30.0,
) -> tuple[list[dict], list[str], bool, dict[str, Any] | None]:
    """统一入口：按 provider_type 选择策略获取模型列表（可附带 upstream_metadata）。"""
    # Ensure provider plugins (including custom model fetchers) are registered.
    from src.services.provider.envelope import ensure_providers_bootstrapped

    ensure_providers_bootstrapped()

    fetcher = UpstreamModelsFetcherRegistry.get(ctx.provider_type) or _fetch_models_default
    return await fetcher(ctx, timeout_seconds)


def merge_upstream_metadata(
    current: dict[str, Any] | None,
    incoming: dict[str, Any],
) -> dict[str, Any]:
    """合并上游元数据，对 quota_by_model 做模型级深度合并。

    以上游返回为准（全量替换），不再保留上游已下架的模型。
    仅对上游返回的模型补充旧数据中的 reset_time（当新数据缺少时）。
    """
    merged: dict[str, Any] = dict(current) if isinstance(current, dict) else {}
    for ns_key, ns_val in incoming.items():
        old_ns = merged.get(ns_key)
        if (
            isinstance(ns_val, dict)
            and isinstance(old_ns, dict)
            and "quota_by_model" in ns_val
            and "quota_by_model" in old_ns
        ):
            old_qbm = old_ns["quota_by_model"]
            new_qbm = ns_val["quota_by_model"]
            if isinstance(old_qbm, dict) and isinstance(new_qbm, dict):
                # 保留新数据中已有模型的旧 reset_time
                for model_id, new_info in new_qbm.items():
                    if not isinstance(new_info, dict):
                        continue
                    old_info = old_qbm.get(model_id)
                    if (
                        isinstance(old_info, dict)
                        and "reset_time" in old_info
                        and "reset_time" not in new_info
                    ):
                        new_info["reset_time"] = old_info["reset_time"]
        merged[ns_key] = ns_val
    return merged


def build_all_format_configs(
    api_key_value: str,
    format_to_endpoint: dict[str, EndpointFetchConfig],
) -> list[dict]:
    """
    构建所有 API 格式的端点配置

    只对实际配置了端点的格式构建请求配置，不同端点的 base_url 可能不同，
    不应使用某个端点的 base_url 去尝试其他格式。

    Args:
        api_key_value: 解密后的 API Key
        format_to_endpoint: API 格式到 EndpointFetchConfig 的映射

    Returns:
        端点配置列表，每个配置包含 api_key, base_url, api_format, extra_headers
    """
    if not format_to_endpoint:
        return []

    # 同族内优先使用 chat 端点，若无则回退到 cli 端点
    configs: list[dict] = []
    for candidates in MODEL_FETCH_FORMAT_PRIORITY:
        fmt = next((f for f in candidates if f in format_to_endpoint), None)
        if fmt is not None:
            cfg = format_to_endpoint[fmt]

            base_url = str(getattr(cfg, "base_url", "") or "")
            extra_headers: dict[str, str] | None

            if isinstance(cfg, EndpointFetchConfig):
                extra_headers = cfg.extra_headers
            else:
                # 允许直接传递类似 ProviderEndpoint 的对象（测试/独立使用场景）。
                candidate_extra = getattr(cfg, "extra_headers", None)
                if isinstance(candidate_extra, dict):
                    extra_headers = {str(k): str(v) for k, v in candidate_extra.items() if k}
                else:
                    extra_headers = get_extra_headers_from_endpoint(cfg)

            configs.append(
                {
                    "api_key": api_key_value,
                    "base_url": base_url,
                    "api_format": fmt,
                    "extra_headers": extra_headers,
                }
            )
    return configs


async def fetch_models_from_endpoints(
    endpoint_configs: list[dict],
    timeout: float = 30.0,
    proxy_config: dict[str, Any] | None = None,
) -> tuple[list[dict], list[str], bool]:
    """
    从多个端点并发获取模型

    Args:
        endpoint_configs: 端点配置列表，每个配置包含 api_key, base_url, api_format, extra_headers
        timeout: 请求超时时间（秒）
        proxy_config: 代理配置（可选），支持系统默认回退

    Returns:
        (模型列表, 错误列表, 是否有成功)
    """
    from src.services.proxy_node.resolver import build_proxy_client_kwargs

    all_models: list[dict] = []
    errors: list[str] = []
    has_success = False
    semaphore = asyncio.Semaphore(MAX_CONCURRENT_REQUESTS)

    async def fetch_one(client: httpx.AsyncClient, config: dict) -> tuple[list, str | None, bool]:
        base_url = config["base_url"]
        if not base_url:
            return [], None, False
        base_url = base_url.rstrip("/")
        api_format = config["api_format"]
        api_key_value = config["api_key"]
        extra_headers = config.get("extra_headers")

        try:
            async with semaphore:
                from src.core.api_format.capabilities import fetch_models_for_api_format

                models, error = await fetch_models_for_api_format(
                    client,
                    api_format=api_format,
                    base_url=base_url,
                    api_key=api_key_value,
                    extra_headers=extra_headers,
                )

            for m in models:
                if "api_format" not in m:
                    m["api_format"] = api_format

            # 即使返回空列表，只要没有错误也算成功
            success = error is None
            return models, error, success
        except httpx.TimeoutException:
            logger.warning("获取 {} 模型超时", api_format)
            return [], f"{api_format}: timeout", False
        except Exception:
            logger.exception("获取 {} 模型出错", api_format)
            return [], f"{api_format}: error", False

    async with httpx.AsyncClient(
        **build_proxy_client_kwargs(proxy_config, timeout=timeout)
    ) as client:
        results = await asyncio.gather(*[fetch_one(client, c) for c in endpoint_configs])
        for models, error, success in results:
            all_models.extend(models)
            if error:
                errors.append(error)
            if success:
                has_success = True

    return all_models, errors, has_success
