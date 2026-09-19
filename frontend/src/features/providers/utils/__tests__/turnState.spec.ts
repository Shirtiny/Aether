import { describe, expect, it } from 'vitest'
import type { ProviderWithEndpointsSummary } from '@/api/endpoints'
import { selectedTurnStateSource, turnStateCollectionConfig, turnStateSourceConfig, turnStateSourceOptions, TURN_STATE_DISABLED } from '../turnState'

describe('account/provider Turn-State sources', () => {
  it('lists only collection-enabled providers and accounts, not inactive parent providers', () => {
    const providers = [
      { id: 'empty', is_active: true },
      { id: 'provider', name: 'Provider', is_active: true, turn_state_collection: { enabled: true, models: ['m'] } },
      { id: 'accounts', name: 'Pool', is_active: true, turn_state_collection: { enabled: false, models: ['m'] }, turn_state_account_sources: [{ key_id: 'key', name: 'Account' }] },
      { id: 'stopped', is_active: false, turn_state_collection: { enabled: true, models: ['m'] }, turn_state_account_sources: [{ key_id: 'hidden', name: 'Hidden' }] },
    ] as ProviderWithEndpointsSummary[]
    expect(turnStateSourceOptions(providers)).toEqual([
      { id: 'provider', name: '提供商 · Provider' },
      { id: 'account:accounts:key', name: '账号 · Pool / Account' },
    ])
  })
  it('round-trips provider, account and off without retaining an old key binding', () => {
    for (const value of ['provider', 'account:provider:key', TURN_STATE_DISABLED]) {
      expect(selectedTurnStateSource(turnStateSourceConfig(value))).toBe(value)
    }
    expect(turnStateSourceConfig('provider').turn_state_source_key_id).toBeNull()
    expect(turnStateSourceConfig(TURN_STATE_DISABLED)).toEqual({ turn_state_source_provider_id: null, turn_state_source_key_id: null })
    expect(selectedTurnStateSource({ turn_state_source_provider_id: null, turn_state_source_key_id: 'stale' })).toBe(TURN_STATE_DISABLED)
  })
  it('deduplicates model names and enforces server validation limits', () => {
    expect(turnStateCollectionConfig(true, ' a\nb,a， B ')).toEqual({ enabled: true, models: ['a', 'b', 'B'] })
    expect(turnStateCollectionConfig(false, '')).toEqual({ enabled: false, models: [] })
    expect(() => turnStateCollectionConfig(true, '')).toThrow()
    expect(() => turnStateCollectionConfig(true, '汉'.repeat(67))).toThrow()
    expect(() => turnStateCollectionConfig(true, 'a\u0001b')).toThrow()
    expect(() => turnStateCollectionConfig(true, Array.from({ length: 65 }, (_, n) => `model-${n}`).join('\n'))).toThrow()
  })
})
