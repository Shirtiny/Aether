import { describe, expect, it } from 'vitest'
import type { PoolKeyDetail } from '@/api/endpoints/pool'
import { supportsCodexWs, describeCodexWsAccount } from '../codexWsAccount'

function key(overrides: Partial<PoolKeyDetail> = {}): PoolKeyDetail {
  return {
    key_id: 'key-1',
    key_name: 'Codex',
    is_active: true,
    auth_type: 'oauth',
    account_quota: null,
    cooldown_reason: null,
    cooldown_ttl_seconds: null,
    cost_window_usage: 0,
    cost_limit: null,
    request_count: 0,
    total_tokens: 0,
    total_cost_usd: '0',
    sticky_sessions: 0,
    lru_score: null,
    created_at: null,
    last_used_at: null,
    ...overrides,
  }
}

describe('Codex WS account support', () => {
  it('only applies to Codex OAuth accounts', () => {
    expect(supportsCodexWs('codex', key())).toBe(true)
    expect(supportsCodexWs(' CODEX ', key({ auth_type: ' OAuth ' }))).toBe(true)
    expect(supportsCodexWs('openai', key())).toBe(false)
    expect(supportsCodexWs('codex', key({ auth_type: 'api_key' }))).toBe(false)
  })

  it('defaults to support without account metadata and ignores the old switch/profile', () => {
    for (const account of [
      key(),
      key({ capabilities: { codex_official_ws: false } }),
      key({ capabilities: { codex_official_ws: true } }),
      key({ fingerprint: { websocket_transport_profile: { profile_id: 'outdated' } } }),
    ]) {
      const presentation = describeCodexWsAccount(account)
      expect(presentation.label).toBe('WS 默认支持')
      expect(presentation.title).toContain('每次调度时判定')
      expect(presentation.tone).toBe('warning')
    }
  })

  it('does not claim disabled accounts can be scheduled', () => {
    expect(describeCodexWsAccount(key({ is_active: false }))).toMatchObject({
      label: 'WS 不可调度',
      tone: 'danger',
    })
  })
})
