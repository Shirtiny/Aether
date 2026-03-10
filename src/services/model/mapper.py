"""
模型映射中间件
根据数据库中的配置，将用户请求的模型映射到提供商的实际模型
"""

from __future__ import annotations

from sqlalchemy.orm import Session, joinedload

from src.core.cache_utils import SyncLRUCache
from src.core.logger import logger
from src.models.claude import ClaudeMessagesRequest
from src.models.database import GlobalModel, Model, Provider, ProviderEndpoint
from src.services.cache.model_cache import ModelCacheService

# 模块级共享缓存，所有 ModelMapperMiddleware 实例共用
_shared_cache = SyncLRUCache(max_size=1000, ttl=300)


class ModelMapperMiddleware:
    """
    模型映射中间件
    负责将用户请求的模型名映射到提供商的实际模型名
    """

    def __init__(self, db: Session):
        """
        初始化模型映射中间件

        Args:
            db: 数据库会话
        """
        self.db = db

    async def apply_mapping(
        self, request: ClaudeMessagesRequest, provider: Provider
    ) -> ClaudeMessagesRequest:
        """
        应用模型映射到请求

        Args:
            request: 原始请求
            provider: 目标提供商

        Returns:
            应用映射后的请求
        """
        # 获取请求的模型名
        source_model = request.model

        # 查找映射
        mapping = await self.get_mapping(source_model, provider.id)

        if mapping:
            # 应用映射
            original_model = request.model
            request.model = mapping.model.select_provider_model_name()

            logger.debug(
                f"Applied model mapping for provider {provider.name}: "
                f"{original_model} -> {request.model}"
            )
        else:
            # 没有找到映射，使用原始模型名
            logger.debug(
                f"No model mapping found for {source_model} with provider {provider.name}, "
                f"forwarding with original model name"
            )

        return request

    async def get_mapping(self, source_model: str, provider_id: str) -> object | None:
        """
        获取模型映射

        简化后的逻辑:
        1. 通过 GlobalModel.name 解析 GlobalModel
        2. 找到 GlobalModel 后，查找该 Provider 的 Model 实现

        Args:
            source_model: 用户请求的模型名（必须是 GlobalModel.name）
            provider_id: 提供商ID (UUID)

        Returns:
            模型映射对象（包含 model 字段），如果没有找到返回None
        """
        # 步骤 1: 规范化模型名称
        normalized_name = source_model.strip() if isinstance(source_model, str) else ""
        if not normalized_name:
            logger.debug("GlobalModel not found: <empty model name>")
            return None

        # 检查缓存（使用规范化后的名称）
        cache_key = f"{provider_id}:{normalized_name}"
        if cache_key in _shared_cache:
            return _shared_cache[cache_key]

        mapping = None

        global_model = await ModelCacheService.get_global_model_by_name(self.db, normalized_name)

        if not global_model or not global_model.is_active:
            logger.debug(f"GlobalModel not found or inactive: {normalized_name}")
            _shared_cache[cache_key] = None
            return None

        # 步骤 2: 查找该 Provider 是否有实现这个 GlobalModel 的 Model（使用缓存）
        model = await ModelCacheService.get_model_by_provider_and_global_model(
            self.db, provider_id, global_model.id
        )

        if model:
            # 将 ORM Model 转为无 Session 绑定的实例，避免跨请求缓存导致 DetachedInstanceError
            from sqlalchemy.orm.session import object_session

            if object_session(model) is not None:
                model_dict = ModelCacheService._model_to_dict(model)
                model = ModelCacheService._dict_to_model(model_dict)

            # 创建映射对象
            mapping = type(
                "obj",
                (object,),
                {
                    "source_model": source_model,
                    "model": model,
                    "is_active": True,
                    "provider_id": provider_id,
                },
            )()

            logger.debug(
                f"Found model mapping: {normalized_name} -> {model.provider_model_name} "
                f"(provider={provider_id[:8]}...)"
            )

        # 缓存结果
        _shared_cache[cache_key] = mapping

        return mapping

    def get_all_mappings(self, provider_id: str) -> list[object]:
        """
        获取提供商的所有可用模型(通过 GlobalModel)

        Args:
            provider_id: 提供商ID (UUID)

        Returns:
            模型映射列表
        """
        # 查询该 Provider 的所有活跃 Model（使用 joinedload 避免 N+1）
        models = (
            self.db.query(Model)
            .join(GlobalModel)
            .options(joinedload(Model.global_model))
            .filter(
                Model.provider_id == provider_id,
                Model.is_active == True,
                GlobalModel.is_active == True,
            )
            .all()
        )

        # 构造兼容的映射对象列表
        mappings = []
        for model in models:
            mapping = type(
                "obj",
                (object,),
                {
                    "source_model": model.global_model.name,
                    "model": model,
                    "is_active": True,
                    "provider_id": provider_id,
                },
            )()
            mappings.append(mapping)

        return mappings

    def get_supported_models(self, provider_id: str) -> list[str]:
        """
        获取提供商支持的所有源模型名

        Args:
            provider_id: 提供商ID (UUID)

        Returns:
            支持的模型名列表
        """
        mappings = self.get_all_mappings(provider_id)
        return [mapping.source_model for mapping in mappings]

    async def validate_request(
        self, request: ClaudeMessagesRequest, provider: Provider
    ) -> tuple[bool, str | None]:
        """
        验证请求是否符合映射的限制

        Args:
            request: 请求对象
            provider: 提供商对象

        Returns:
            (是否有效, 错误信息)
        """
        mapping = await self.get_mapping(request.model, provider.id)

        if not mapping:
            # 没有映射，可能是默认支持的模型
            return True, None

        if not mapping.is_active:
            return False, f"Model mapping for {request.model} is disabled"

        return True, None

    @staticmethod
    def clear_cache() -> None:
        """清空共享缓存"""
        _shared_cache.clear()
        logger.debug("Model mapping cache cleared")

    @staticmethod
    def refresh_cache(provider_id: str | None = None) -> None:
        """
        刷新缓存

        Args:
            provider_id: 如果指定，只刷新该提供商的缓存 (UUID)
        """
        if provider_id:
            keys_to_remove = [
                key for key in _shared_cache.keys() if key.startswith(f"{provider_id}:")
            ]
            for key in keys_to_remove:
                del _shared_cache[key]
            logger.debug(f"Refreshed cache for provider {provider_id}")
        else:
            ModelMapperMiddleware.clear_cache()


class ModelRoutingMiddleware:
    """
    模型路由中间件
    根据模型名选择合适的提供商
    """

    def __init__(self, db: Session):
        """
        初始化模型路由中间件

        Args:
            db: 数据库会话
        """
        self.db = db
        self.mapper = ModelMapperMiddleware(db)

    def select_provider(
        self,
        model_name: str,
        preferred_provider: str | None = None,
        allowed_api_formats: list[str] | None = None,
        request_id: str | None = None,
    ) -> Provider | None:
        """
        根据模型名选择提供商

        Args:
            model_name: 请求的模型名
            preferred_provider: 首选提供商名称
            allowed_api_formats: 允许的API格式列表
            request_id: 请求ID（用于日志关联）

        Returns:
            选中的提供商，如果没有找到返回None
        """
        request_prefix = f"ID:{request_id} | " if request_id else ""
        allowed_norm: set[str] | None = None
        if allowed_api_formats:
            from src.services.provider.format import normalize_endpoint_signature

            allowed_norm = {
                normalize_endpoint_signature(str(fmt))
                for fmt in allowed_api_formats
                if isinstance(fmt, str) and fmt
            }

        # 1. 如果指定了提供商，直接使用
        if preferred_provider:
            provider = (
                self.db.query(Provider)
                .filter(Provider.name == preferred_provider, Provider.is_active == True)
                .first()
            )

            if provider:
                # 检查API格式 - 从 endpoints 中检查
                if allowed_norm:
                    has_matching_endpoint = any(
                        ep.is_active
                        and ep.api_format
                        and str(ep.api_format).strip().lower() in allowed_norm
                        for ep in provider.endpoints
                    )
                    if not has_matching_endpoint:
                        logger.warning(
                            f"Specified provider {provider.name} has no active endpoints with allowed API formats ({allowed_api_formats})"
                        )
                    else:
                        logger.debug(
                            f"  └─ {request_prefix}使用指定提供商: {provider.name} | 模型:{model_name}"
                        )
                        return provider
                else:
                    logger.debug(
                        f"  └─ {request_prefix}使用指定提供商: {provider.name} | 模型:{model_name}"
                    )
                    return provider
            else:
                logger.warning(f"Specified provider {preferred_provider} not found or inactive")

        # 2. 查找优先级最高的活动提供商
        query = self.db.query(Provider).filter(Provider.is_active == True)

        if allowed_norm:
            query = (
                query.join(ProviderEndpoint)
                .filter(
                    ProviderEndpoint.is_active == True,
                    ProviderEndpoint.api_format.in_(sorted(allowed_norm)),
                )
                .distinct()
            )

        best_provider = query.order_by(Provider.provider_priority.asc(), Provider.id.asc()).first()

        if best_provider:
            logger.debug(
                f"  └─ {request_prefix}使用优先级最高提供商: {best_provider.name} (priority:{best_provider.provider_priority}) | 模型:{model_name}"
            )
            return best_provider

        if allowed_api_formats:
            logger.error(
                f"No active providers found with allowed API formats {allowed_api_formats}."
            )
        else:
            logger.error("No active providers found.")
        return None

    def get_available_models(self) -> dict[str, list[str]]:
        """
        获取所有可用的模型及其提供商

        Returns:
            字典，键为 GlobalModel.name，值为支持该模型的提供商名列表
        """
        result = {}

        models = (
            self.db.query(GlobalModel.name, Provider.name)
            .join(Model, GlobalModel.id == Model.global_model_id)
            .join(Provider, Model.provider_id == Provider.id)
            .filter(
                GlobalModel.is_active == True, Model.is_active == True, Provider.is_active == True
            )
            .all()
        )

        for global_model_name, provider_name in models:
            if global_model_name not in result:
                result[global_model_name] = []
            if provider_name not in result[global_model_name]:
                result[global_model_name].append(provider_name)

        return result

    async def get_cheapest_provider(self, model_name: str) -> Provider | None:
        """
        获取某个模型最便宜的提供商

        Args:
            model_name: GlobalModel 名称

        Returns:
            最便宜的提供商
        """
        # 直接查找 GlobalModel
        global_model = (
            self.db.query(GlobalModel)
            .filter(GlobalModel.name == model_name, GlobalModel.is_active == True)
            .first()
        )

        if not global_model:
            return None

        # 查询所有支持该模型的 Provider 及其价格
        models_with_providers = (
            self.db.query(Provider, Model)
            .join(Model, Provider.id == Model.provider_id)
            .filter(
                Model.global_model_id == global_model.id,
                Model.is_active == True,
                Provider.is_active == True,
            )
            .all()
        )

        if not models_with_providers:
            return None

        # 按总价格排序
        cheapest = min(
            models_with_providers,
            key=lambda x: x[1].get_effective_input_price() + x[1].get_effective_output_price(),
        )

        provider = cheapest[0]
        model = cheapest[1]

        logger.debug(
            f"Selected cheapest provider {provider.name} for model {model_name} "
            f"(input: ${model.get_effective_input_price()}/M, output: ${model.get_effective_output_price()}/M)"
        )

        return provider
