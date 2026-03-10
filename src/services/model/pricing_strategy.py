"""
计费相关数据类

定义计费计算所需的数据结构。
实际的计费能力已收敛到 core.api_format 注册表，避免依赖 API Adapter。

数据类：
- UsageTokens: 请求的 token 使用量
- PricingConfig: 价格配置
- CostResult: 计费结果
"""

from dataclasses import dataclass


@dataclass
class UsageTokens:
    """请求的 token 使用量"""

    input_tokens: int = 0
    output_tokens: int = 0
    cache_creation_input_tokens: int = 0
    cache_read_input_tokens: int = 0


@dataclass
class PricingConfig:
    """价格配置"""

    input_price_per_1m: float = 0.0
    output_price_per_1m: float = 0.0
    cache_creation_price_per_1m: float | None = None
    cache_read_price_per_1m: float | None = None
    price_per_request: float | None = None
    tiered_pricing: dict | None = None
    cache_ttl_minutes: int | None = None


@dataclass
class CostResult:
    """计费结果"""

    input_cost: float = 0.0
    output_cost: float = 0.0
    cache_creation_cost: float = 0.0
    cache_read_cost: float = 0.0
    cache_cost: float = 0.0
    request_cost: float = 0.0
    total_cost: float = 0.0
    tier_index: int | None = None  # 命中的阶梯索引
