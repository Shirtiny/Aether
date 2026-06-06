/**
 * 上游模型获取服务
 *
 * 缓存已移至后端（Redis），前端只保留并发请求去重，避免同时发多个相同请求。
 */
import { ref } from 'vue'
import { isAxiosError } from 'axios'
import { adminApi } from '@/api/admin'
import { parseUpstreamModelError } from '@/utils/errorParser'
import type { UpstreamModel } from '@/api/endpoints/types'

export type { UpstreamModel }

type FetchResult = { models: UpstreamModel[]; error?: string; warning?: string; fromCache?: boolean }

// 进行中的请求（用于去重并发请求）
const pendingRequests = new Map<string, Promise<FetchResult>>()

// 请求状态
const loadingMap = ref<Map<string, boolean>>(new Map())

/**
 * 生成请求 key
 */
function getRequestKey(providerId: string, apiKeyId?: string): string {
  return apiKeyId ? `${providerId}:${apiKeyId}` : providerId
}

export function useUpstreamModelsCache() {
  /**
   * 获取上游模型列表
   * @param providerId 提供商ID
   * @param apiKeyId 可选的 API Key ID（用于获取特定 Key 支持的模型）
   * @param forceRefresh 是否强制刷新（跳过后端缓存）
   * @returns 模型列表或错误信息
   */
  async function fetchModels(
    providerId: string,
    apiKeyId?: string,
    forceRefresh = false
  ): Promise<FetchResult> {
    const requestKey = getRequestKey(providerId, apiKeyId)

    // 强制刷新时不复用进行中的请求
    if (!forceRefresh && pendingRequests.has(requestKey)) {
      // eslint-disable-next-line @typescript-eslint/no-non-null-assertion
      return pendingRequests.get(requestKey)!
    }

    // 创建新请求
    const requestPromise = (async (): Promise<FetchResult> => {
      try {
        loadingMap.value.set(requestKey, true)
        const response = await adminApi.queryProviderModels(providerId, apiKeyId, forceRefresh)

        if (response.success && response.data?.models) {
          const partialWarning = response.data.warning ?? response.data.error
          return {
            models: response.data.models,
            warning: partialWarning ? parseUpstreamModelError(partialWarning) : undefined,
            fromCache: response.data.from_cache
          }
        } else {
          const rawError = response.data?.error || response.data?.warning || '获取上游模型失败'
          return { models: [], error: parseUpstreamModelError(rawError) }
        }
      } catch (err: unknown) {
        const rawError = isAxiosError(err) ? (err.response?.data?.detail ?? err.message) : (err instanceof Error ? err.message : String(err))
        return { models: [], error: parseUpstreamModelError(rawError || '获取上游模型失败') }
      } finally {
        loadingMap.value.set(requestKey, false)
        pendingRequests.delete(requestKey)
      }
    })()

    pendingRequests.set(requestKey, requestPromise)
    return requestPromise
  }

  /**
   * 检查是否正在加载
   */
  function isLoading(providerId: string, apiKeyId?: string): boolean {
    const requestKey = getRequestKey(providerId, apiKeyId)
    return loadingMap.value.get(requestKey) || false
  }

  return {
    fetchModels,
    isLoading,
    loadingMap
  }
}
