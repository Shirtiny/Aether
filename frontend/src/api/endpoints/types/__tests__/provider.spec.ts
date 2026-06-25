import { describe, expect, it } from 'vitest'

import {
  normalizeChatPiiRedactionProviderConfig,
  normalizePoolAdvancedConfig,
  normalizeRiskControlSessionAvoidanceProviderConfig,
} from '@/api/endpoints/types'

describe('normalizePoolAdvancedConfig', () => {
  it('keeps object payloads, including empty objects', () => {
    expect(normalizePoolAdvancedConfig({})).toEqual({})
    expect(normalizePoolAdvancedConfig({ global_priority: 5 })).toEqual({ global_priority: 5 })
  })

  it('maps legacy boolean payloads to the current object semantics', () => {
    expect(normalizePoolAdvancedConfig(true)).toEqual({})
    expect(normalizePoolAdvancedConfig(false)).toBeNull()
  })

  it('drops unsupported payload shapes', () => {
    expect(normalizePoolAdvancedConfig(null)).toBeNull()
    expect(normalizePoolAdvancedConfig('enabled')).toBeNull()
    expect(normalizePoolAdvancedConfig(['lru'])).toBeNull()
  })
})


describe('normalizeChatPiiRedactionProviderConfig', () => {
  it('defaults unsupported payloads to disabled', () => {
    expect(normalizeChatPiiRedactionProviderConfig(null)).toEqual({ enabled: false })
    expect(normalizeChatPiiRedactionProviderConfig({})).toEqual({ enabled: false })
    expect(normalizeChatPiiRedactionProviderConfig({ enabled: 'yes' })).toEqual({ enabled: false })
  })

  it('passes through enabled state only', () => {
    expect(normalizeChatPiiRedactionProviderConfig({ enabled: true })).toEqual({ enabled: true })
    expect(normalizeChatPiiRedactionProviderConfig({ enabled: false, entities: ['email'] })).toEqual({ enabled: false })
  })
})

describe('normalizeRiskControlSessionAvoidanceProviderConfig', () => {
  it('defaults unsupported and empty payloads to candidate mode', () => {
    expect(normalizeRiskControlSessionAvoidanceProviderConfig(null)).toEqual({ mode: 'candidate' })
    expect(normalizeRiskControlSessionAvoidanceProviderConfig({})).toEqual({ mode: 'candidate' })
    expect(normalizeRiskControlSessionAvoidanceProviderConfig({ mode: 'unexpected' })).toEqual({ mode: 'candidate' })
  })

  it('supports select modes and legacy enabled payloads', () => {
    expect(normalizeRiskControlSessionAvoidanceProviderConfig({ mode: 'candidate' })).toEqual({ mode: 'candidate' })
    expect(normalizeRiskControlSessionAvoidanceProviderConfig({ mode: 'block' })).toEqual({ mode: 'block' })
    expect(normalizeRiskControlSessionAvoidanceProviderConfig({ enabled: true })).toEqual({ mode: 'candidate' })
    expect(normalizeRiskControlSessionAvoidanceProviderConfig({ enabled: false })).toEqual({ mode: 'candidate' })
  })
})
