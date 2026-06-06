import { beforeEach, describe, expect, it, vi } from 'vitest'

const { getSystemConfigMock, updateSystemConfigMock } = vi.hoisted(() => ({
  getSystemConfigMock: vi.fn(),
  updateSystemConfigMock: vi.fn(),
}))

vi.mock('@/api/admin', () => ({
  adminApi: {
    getSystemConfig: getSystemConfigMock,
    updateSystemConfig: updateSystemConfigMock,
    getSystemVersion: vi.fn(),
  },
}))

vi.mock('@/composables/useToast', () => ({
  useToast: () => ({
    success: vi.fn(),
    error: vi.fn(),
  }),
}))

vi.mock('@/composables/useSiteInfo', () => ({
  useSiteInfo: () => ({
    refreshSiteInfo: vi.fn(),
  }),
}))

vi.mock('@/utils/logger', () => ({
  log: {
    error: vi.fn(),
  },
}))

import { useSystemConfig } from '../composables/useSystemConfig'

interface DeferredConfigResponse {
  resolve: (value: { key: string, value: unknown, is_set?: boolean }) => void
}

describe('useSystemConfig', () => {
  beforeEach(() => {
    getSystemConfigMock.mockReset()
    updateSystemConfigMock.mockReset()
  })

  it('loads config keys in parallel and keeps change detection disabled until the baseline is ready', async () => {
    const pending = new Map<string, DeferredConfigResponse>()
    getSystemConfigMock.mockImplementation((key: string) => new Promise((resolve) => {
      pending.set(key, { resolve })
    }))

    const state = useSystemConfig()
    const loadPromise = state.loadSystemConfig()

    expect(getSystemConfigMock.mock.calls.map(([key]) => key)).toContain('request_record_level')
    expect(getSystemConfigMock.mock.calls.map(([key]) => key)).toContain('proxy_node_metrics_cleanup_batch_size')
    expect(getSystemConfigMock.mock.calls.map(([key]) => key)).toContain('enable_standard_text_sync_heartbeat')

    state.systemConfig.value.request_record_level = 'headers'
    expect(state.systemConfigLoading.value).toBe(true)
    expect(state.hasLogConfigChanges.value).toBe(false)

    for (const [key, deferred] of pending) {
      deferred.resolve({
        key,
        value: key === 'request_record_level' ? 'basic' : undefined,
        is_set: false,
      })
    }
    await loadPromise

    expect(state.systemConfigLoading.value).toBe(false)
    expect(state.systemConfig.value.request_record_level).toBe('basic')
    expect(state.hasLogConfigChanges.value).toBe(false)

    state.systemConfig.value.request_record_level = 'full'
    expect(state.hasLogConfigChanges.value).toBe(true)
  })

  it('loads and saves the standard text sync heartbeat flag as a basic config item', async () => {
    getSystemConfigMock.mockImplementation(async (key: string) => ({
      key,
      value: key === 'enable_standard_text_sync_heartbeat' ? false : undefined,
      is_set: key === 'enable_standard_text_sync_heartbeat',
    }))
    updateSystemConfigMock.mockResolvedValue({})

    const state = useSystemConfig()
    await state.loadSystemConfig()

    expect(state.systemConfig.value.enable_standard_text_sync_heartbeat).toBe(false)
    state.systemConfig.value.enable_standard_text_sync_heartbeat = true
    expect(state.hasBasicConfigChanges.value).toBe(true)

    await state.saveBasicConfig()

    expect(updateSystemConfigMock).toHaveBeenCalledWith(
      'enable_standard_text_sync_heartbeat',
      true,
      '标准文本非流式心跳开关：开启后外层 HTTP 状态固定为 200，上游失败写入响应体'
    )
    expect(state.hasBasicConfigChanges.value).toBe(false)
  })
})
