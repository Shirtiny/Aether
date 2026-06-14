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
    expect(getSystemConfigMock.mock.calls.map(([key]) => key)).toContain('request_capture_policy')
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

  it('loads and saves request capture policy with prompt capture and group scope', async () => {
    getSystemConfigMock.mockImplementation(async (key: string) => ({
      key,
      value: key === 'request_capture_policy'
        ? {
            request_record_level: 'basic',
            max_request_body_bytes: 65536,
            max_response_body_bytes: 0,
            scope: {
              mode: 'include_groups',
              group_ids: ['group-audit'],
            },
            prompt_capture: {
              enabled: true,
              include_system: true,
              include_developer: true,
              include_user: true,
              include_tools: false,
              preview_chars: 1000,
              max_items: 64,
            },
          }
        : undefined,
      is_set: key === 'request_capture_policy',
    }))
    updateSystemConfigMock.mockResolvedValue({})

    const state = useSystemConfig()
    await state.loadSystemConfig()

    expect(state.systemConfig.value.request_record_level).toBe('basic')
    expect(state.systemConfig.value.max_request_body_size).toBe(65536)
    expect(state.systemConfig.value.request_capture_policy.scope.group_ids).toEqual(['group-audit'])
    expect(state.systemConfig.value.request_capture_policy.prompt_capture.enabled).toBe(true)
    expect(state.hasLogConfigChanges.value).toBe(false)

    state.systemConfig.value.request_capture_policy.prompt_capture.include_tools = true
    expect(state.hasLogConfigChanges.value).toBe(true)

    await state.saveLogConfig()

    expect(updateSystemConfigMock).toHaveBeenCalledWith(
      'request_capture_policy',
      expect.objectContaining({
        request_record_level: 'basic',
        max_request_body_bytes: 65536,
        max_response_body_bytes: 0,
        scope: {
          mode: 'include_groups',
          group_ids: ['group-audit'],
        },
        prompt_capture: expect.objectContaining({
          enabled: true,
          include_tools: true,
          preview_chars: 1000,
          max_items: 64,
        }),
      }),
      '请求记录与提示词摘要捕获策略'
    )
    expect(state.hasLogConfigChanges.value).toBe(false)
  })
})
