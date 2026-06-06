import { describe, expect, it } from 'vitest'

import {
  base64UrlEncodeUtf8,
  buildCcSwitchProviderImportUrl,
  type CcSwitchTargetApp,
} from '@/features/api-keys/utils/ccswitchImport'

function decodeBase64UrlJson(value: string): unknown {
  const padded = `${value}${'='.repeat((4 - (value.length % 4)) % 4)}`
    .replace(/-/g, '+')
    .replace(/_/g, '/')
  const binary = atob(padded)
  const bytes = Uint8Array.from(binary, char => char.charCodeAt(0))
  return JSON.parse(new TextDecoder().decode(bytes))
}

function parseImportUrl(url: string) {
  const parsed = new URL(url)
  const params = parsed.searchParams
  const config = params.get('config')
  if (!config) throw new Error('missing config')
  return {
    parsed,
    params,
    config: decodeBase64UrlJson(config) as Record<string, unknown>,
  }
}

const baseInput = {
  baseUrl: 'https://aether.example.com',
  apiKey: 'sk-user-live-1',
  apiKeyName: 'primary',
  siteName: 'Aether Local',
  modelId: 'gpt-5',
}

describe('ccswitchImport', () => {
  it('encodes UTF-8 text as base64url without padding', () => {
    const encoded = base64UrlEncodeUtf8('Aether 中文+/=')

    expect(encoded).not.toContain('+')
    expect(encoded).not.toContain('/')
    expect(encoded).not.toContain('=')
    expect(new TextDecoder().decode(Uint8Array.from(atob(`${encoded}${'='.repeat((4 - (encoded.length % 4)) % 4)}`.replace(/-/g, '+').replace(/_/g, '/')), char => char.charCodeAt(0)))).toBe('Aether 中文+/=')
  })

  it('builds a Claude Code import URL with separate Claude model env fields', () => {
    const { params, config } = parseImportUrl(buildCcSwitchProviderImportUrl({
      ...baseInput,
      targetApp: 'claude',
      modelIds: {
        haiku: 'gpt-5.4-mini',
        sonnet: 'gpt-5',
        opus: 'gpt-5.1-pro',
      },
    }))

    expect(params.get('resource')).toBe('provider')
    expect(params.get('app')).toBe('claude')
    expect(params.get('name')).toBe('Aether Local')
    expect(params.get('icon')).toBe('claude')
    expect(params.get('enabled')).toBe('true')
    expect(params.get('configFormat')).toBe('json')
    expect(params.get('usageEnabled')).toBe('true')
    expect(params.get('usageBaseUrl')).toBe('https://aether.example.com')
    expect(params.get('usageApiKey')).toBe('sk-user-live-1')
    expect(params.get('usageAutoInterval')).toBe('30')
    expect(params.get('endpoint')).toBe('https://aether.example.com')
    expect(params.get('apiKey')).toBe('sk-user-live-1')
    expect(params.get('model')).toBe('gpt-5')
    expect(params.get('haikuModel')).toBe('gpt-5.4-mini')
    expect(params.get('sonnetModel')).toBe('gpt-5')
    expect(params.get('opusModel')).toBe('gpt-5.1-pro')
    const usageScript = params.get('usageScript') || ''
    expect(usageScript).not.toBe('')
    expect(usageScript).not.toContain('(')
    expect(usageScript).not.toContain('+')
    expect(usageScript).not.toContain('/')
    const paddedUsageScript = `${usageScript}${'='.repeat((4 - (usageScript.length % 4)) % 4)}`
      .replace(/-/g, '+')
      .replace(/_/g, '/')
    const decodedUsageScript = new TextDecoder().decode(
      Uint8Array.from(atob(paddedUsageScript), char => char.charCodeAt(0)),
    )
    expect(decodedUsageScript).toContain('/api/ccswitch/usage')
    expect(decodedUsageScript).toContain('Authorization')
    expect(config).toEqual({
      env: {
        ANTHROPIC_AUTH_TOKEN: 'sk-user-live-1',
        ANTHROPIC_BASE_URL: 'https://aether.example.com',
        ANTHROPIC_MODEL: 'gpt-5',
        ANTHROPIC_DEFAULT_HAIKU_MODEL: 'gpt-5.4-mini',
        ANTHROPIC_DEFAULT_SONNET_MODEL: 'gpt-5',
        ANTHROPIC_DEFAULT_OPUS_MODEL: 'gpt-5.1-pro',
      },
    })
  })

  it('builds a Codex import URL with auth and TOML config', () => {
    const { params, config } = parseImportUrl(buildCcSwitchProviderImportUrl({
      ...baseInput,
      targetApp: 'codex',
    }))

    expect(params.get('app')).toBe('codex')
    expect(params.get('icon')).toBe('openai')
    expect(params.get('endpoint')).toBe('https://aether.example.com/v1')
    expect(params.get('apiKey')).toBe('sk-user-live-1')
    expect(params.get('model')).toBe('gpt-5')
    expect(config.auth).toEqual({ OPENAI_API_KEY: 'sk-user-live-1' })
    expect(config.config).toContain('model_provider = "aether"')
    expect(config.config).toContain('model = "gpt-5"')
    expect(config.config).toContain('base_url = "https://aether.example.com/v1"')
    expect(config.config).toContain('wire_api = "responses"')
  })

  it.each([
    ['gemini', {
      GEMINI_API_KEY: 'sk-user-live-1',
      GOOGLE_GEMINI_BASE_URL: 'https://aether.example.com',
      GEMINI_MODEL: 'gpt-5',
    }],
    ['opencode', {
      npm: '@ai-sdk/openai-compatible',
      options: {
        baseURL: 'https://aether.example.com/v1',
        apiKey: 'sk-user-live-1',
      },
      models: {
        'gpt-5': { name: 'gpt-5' },
      },
    }],
    ['openclaw', {
      baseUrl: 'https://aether.example.com/v1',
      apiKey: 'sk-user-live-1',
      api: 'openai-completions',
      models: [{ id: 'gpt-5', name: 'gpt-5' }],
    }],
    ['hermes', {
      name: 'Aether Local',
      base_url: 'https://aether.example.com/v1',
      api_key: 'sk-user-live-1',
      api_mode: 'chat_completions',
      models: [{ id: 'gpt-5', name: 'gpt-5' }],
    }],
  ] satisfies Array<[CcSwitchTargetApp, Record<string, unknown>]>)('builds %s import config', (targetApp, expectedConfig) => {
    const { params, config } = parseImportUrl(buildCcSwitchProviderImportUrl({
      ...baseInput,
      targetApp,
    }))

    expect(params.get('app')).toBe(targetApp)
    expect(params.get('endpoint')).toBe(
      targetApp === 'gemini'
        ? 'https://aether.example.com'
        : 'https://aether.example.com/v1',
    )
    expect(params.get('apiKey')).toBe('sk-user-live-1')
    expect(params.get('model')).toBe('gpt-5')
    expect(config).toEqual(expectedConfig)
  })
})
