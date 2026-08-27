import { beforeEach, describe, expect, it, vi } from 'vitest'

const { getMock, putMock } = vi.hoisted(() => ({
  getMock: vi.fn(),
  putMock: vi.fn(),
}))

vi.mock('@/api/client', () => ({
  default: {
    get: getMock,
    put: putMock,
  },
}))

import {
  modulesApi,
  parseLocalProbeInterceptUsage,
  type LocalProbeInterceptConfig,
} from '@/api/modules'

const config: LocalProbeInterceptConfig = {
  enabled: true,
  rules: [{
    id: 'ping',
    name: 'Ping',
    prompt: 'ping',
    response: 'pong',
    kind: 'ping',
    enabled: true,
    system: true,
  }],
  usage: {
    input_tokens: 120,
    output_tokens: 8,
    cached_tokens: 40,
  },
  delay_min_ms: 100,
  delay_max_ms: 200,
}

describe('local probe intercept config API', () => {
  beforeEach(() => {
    getMock.mockReset()
    putMock.mockReset()
  })

  it('reads the combined config with one request', async () => {
    getMock.mockResolvedValue({ data: { value: config } })

    await expect(modulesApi.getLocalProbeInterceptConfig()).resolves.toEqual(config)
    expect(getMock).toHaveBeenCalledTimes(1)
    expect(getMock).toHaveBeenCalledWith('/api/admin/system/configs/module.local_probe_intercept.config')
  })

  it('falls back to legacy keys when the combined config is absent', async () => {
    getMock.mockImplementation((url: string) => {
      const key = url.split('/').pop()
      if (key === 'module.local_probe_intercept.config') {
        return Promise.reject({ response: { status: 404 } })
      }
      const values: Record<string, unknown> = {
        'module.local_probe_intercept.enabled': config.enabled,
        'module.local_probe_intercept.rules': config.rules,
        'module.local_probe_intercept.usage': config.usage,
        'module.local_probe_intercept.delay_min_ms': config.delay_min_ms,
        'module.local_probe_intercept.delay_max_ms': config.delay_max_ms,
      }
      return Promise.resolve({ data: { value: values[key ?? ''] } })
    })

    await expect(modulesApi.getLocalProbeInterceptConfig()).resolves.toEqual(config)
    expect(getMock).toHaveBeenCalledTimes(6)
  })

  it('saves the complete config with one request', async () => {
    putMock.mockResolvedValue({ data: { value: config } })

    await expect(modulesApi.updateLocalProbeInterceptConfig(config)).resolves.toEqual(config)
    expect(putMock).toHaveBeenCalledTimes(1)
    expect(putMock).toHaveBeenCalledWith(
      '/api/admin/system/configs/module.local_probe_intercept.config',
      { value: config, description: '测活拦截完整配置' },
    )
  })

  it('rejects invalid server usage instead of clamping it', () => {
    expect(() => parseLocalProbeInterceptUsage({
      input_tokens: 12,
      output_tokens: 3,
      cached_tokens: 13,
    })).toThrow('缓存 Token 不能大于输入 Token')
  })

  it('does not hide an invalid combined config behind legacy fallback', async () => {
    getMock.mockResolvedValue({
      data: {
        value: {
          ...config,
          usage: { ...config.usage, cached_tokens: 121 },
        },
      },
    })

    await expect(modulesApi.getLocalProbeInterceptConfig())
      .rejects.toThrow('缓存 Token 不能大于输入 Token')
    expect(getMock).toHaveBeenCalledTimes(1)
  })
})
