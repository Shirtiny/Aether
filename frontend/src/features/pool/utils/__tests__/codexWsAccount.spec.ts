import { describe, expect, it } from 'vitest'
import type { PoolKeyDetail } from '@/api/endpoints/pool'
import {
  canConfigureCodexWs,
  describeCodexWsAccount,
  hasValidCodexWsManifest,
  isCodexWsEnabled,
} from '../codexWsAccount'
import type { CodexWsAccountStatus } from '@/api/endpoints/keys'

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
    last_used_at: null,
    ...overrides,
  }
}

function status(overrides: Partial<CodexWsAccountStatus> = {}): CodexWsAccountStatus {
  return {
    key_id: 'key-1',
    configured: true,
    profile_effective: true,
    runtime_eligible: null,
    profile_id: 'codex-ws-0.144.1-linux-x64-rustls023-aws-lc-caenv1-wbufret256k1',
    runtime_state: 'request_scoped',
    profile_reasons: [],
    runtime_reasons: [
      'proxy_route_not_evaluated',
      'request_model_not_evaluated',
      'quota_runtime_state_not_evaluated',
      'circuit_runtime_state_not_evaluated',
      'concurrency_runtime_state_not_evaluated',
    ],
    ...overrides,
  }
}

describe('Codex WS account state', () => {
  it('only exposes the control for Codex OAuth accounts', () => {
    expect(canConfigureCodexWs('codex', key())).toBe(true)
    expect(canConfigureCodexWs('openai', key())).toBe(false)
    expect(canConfigureCodexWs('codex', key({ auth_type: 'api_key' }))).toBe(false)
  })

  it('requires the flat capability boolean', () => {
    expect(isCodexWsEnabled(key({ capabilities: { codex_official_ws: true } }))).toBe(true)
    expect(isCodexWsEnabled(key({ capabilities: { codex_official_ws: false } }))).toBe(false)
  })

  it('validates every immutable profile identity field while allowing unknown fields', () => {
    const configured = key({
      fingerprint: {
        retained: 'value',
        websocket_transport_profile: {
          schema_version: 3,
          profile_id: 'codex-ws-0.144.1-linux-x64-rustls023-aws-lc-caenv1-wbufret256k1',
          codex_commit: '1f0566d3f59298d1bb88820a0d35294f1eeb07ea',
          tokio_tungstenite_rev: '0e5b2d73aa18dd9f0a50ee9ff199d5aef7594186',
          tungstenite_rev: '4fffad30fe373adbdcffab9545e9e9bf4f2fc19f',
          tungstenite_patch_id: 'aether-tungstenite-0.27-out-buffer-retention-v1',
          write_buffer_size_bytes: 131072,
          max_write_buffer_size_bytes: 17825792,
          max_retained_write_buffer_capacity_bytes: 262144,
          crypto_provider: 'aws-lc-rs',
          retained_nested: true,
        },
      },
    })
    expect(hasValidCodexWsManifest(configured)).toBe(true)
    ;(configured.fingerprint?.websocket_transport_profile as Record<string, unknown>).codex_commit = 'wrong'
    expect(hasValidCodexWsManifest(configured)).toBe(false)
  })

  it('does not present a successful configuration mutation as runtime eligible', () => {
    const presentation = describeCodexWsAccount(
      key({ capabilities: { codex_official_ws: true } }),
      status(),
    )

    expect(presentation.label).toBe('WS 配置有效')
    expect(presentation.tone).toBe('warning')
    expect(presentation.title).toContain('代理路由、请求模型、额度、熔断和并发')
    expect(presentation.title).not.toContain('已生效')
  })

  it('separates profile blockers from runtime blockers', () => {
    const configured = key({ capabilities: { codex_official_ws: true } })
    const profileBlocked = describeCodexWsAccount(configured, status({
      profile_effective: false,
      runtime_eligible: false,
      runtime_state: 'profile_blocked',
      profile_reasons: ['official_endpoint_host_unsupported'],
      runtime_reasons: ['profile_not_effective'],
    }))
    const runtimeBlocked = describeCodexWsAccount(configured, status({
      runtime_eligible: false,
      runtime_reasons: ['provider_key_concurrency_limit_reached'],
    }))

    expect(profileBlocked.label).toBe('WS 前置阻塞')
    expect(profileBlocked.title).toContain('official_endpoint_host_unsupported')
    expect(runtimeBlocked.label).toBe('WS 不可调度')
    expect(runtimeBlocked.title).toContain('provider_key_concurrency_limit_reached')
  })
})
