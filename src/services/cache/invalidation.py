"""
缓存失效服务

统一管理各种缓存的失效逻辑
"""

from __future__ import annotations

from typing import Any

from src.core.logger import logger


class CacheInvalidationService:
    """缓存失效服务"""

    def __init__(self) -> None:
        pass

    def register_model_mapper(self, model_mapper: Any) -> None:
        """注册 ModelMapper 实例（已弃用，保留兼容性，不执行任何操作）"""
        pass

    async def on_global_model_changed(
        self, model_name: str, global_model_id: str | None = None
    ) -> None:
        """
        GlobalModel 变更时的缓存失效

        Args:
            model_name: 变更的 GlobalModel.name
            global_model_id: GlobalModel ID（可选）
        """
        logger.info(f"[CacheInvalidation] GlobalModel 变更: {model_name}")

        # 1. 清空正则缓存
        from src.core.model_permissions import clear_regex_cache

        clear_regex_cache()

        # 2. 清空 ModelMapper 共享缓存
        from src.services.model.mapper import ModelMapperMiddleware

        ModelMapperMiddleware.clear_cache()

        # 3. 清空 ModelCacheService 缓存
        from src.services.cache.model_cache import ModelCacheService

        try:
            await ModelCacheService.invalidate_global_model_cache(
                global_model_id=global_model_id or "", name=model_name
            )
        except Exception as e:
            logger.error(f"[CacheInvalidation] 失效 ModelCacheService 缓存失败: {e}")

        # 4. 清除 /v1/models 列表缓存
        from src.services.cache.model_list_cache import invalidate_models_list_cache

        try:
            await invalidate_models_list_cache()
        except Exception as e:
            logger.error(f"[CacheInvalidation] 失效 models list 缓存失败: {e}")

    def on_model_changed(self, provider_id: str, global_model_id: str) -> Any:
        """Model 变更时的缓存失效"""
        self._refresh_provider_cache(provider_id)

    async def on_key_allowed_models_changed(self, provider_id: str) -> None:
        """
        Key 的 allowed_models 变更时的缓存失效

        当 Key 的模型白名单变化时（如自动获取更新），需要刷新相关缓存，
        以便正则映射规则能够重新匹配到新的白名单模型。

        Args:
            provider_id: 变更的 Key 所属的 Provider ID
        """
        logger.info(f"[CacheInvalidation] Key allowed_models 变更: provider_id={provider_id}")
        self._refresh_provider_cache(provider_id)

        # 清除 /v1/models 列表缓存（allowed_models 变更会影响模型可用性）
        from src.services.cache.model_list_cache import invalidate_models_list_cache

        try:
            await invalidate_models_list_cache()
        except Exception as e:
            logger.error(f"[CacheInvalidation] 失效 models list 缓存失败: {e}")

    def _refresh_provider_cache(self, provider_id: str) -> None:
        """刷新指定 Provider 的 ModelMapper 缓存"""
        from src.services.model.mapper import ModelMapperMiddleware

        ModelMapperMiddleware.refresh_cache(provider_id)

    def clear_all_caches(self) -> None:
        """清空所有缓存"""
        from src.services.model.mapper import ModelMapperMiddleware

        ModelMapperMiddleware.clear_cache()


# 全局单例
_cache_invalidation_service: CacheInvalidationService | None = None


def get_cache_invalidation_service() -> CacheInvalidationService:
    """获取全局缓存失效服务实例"""
    global _cache_invalidation_service

    if _cache_invalidation_service is None:
        _cache_invalidation_service = CacheInvalidationService()

    return _cache_invalidation_service
