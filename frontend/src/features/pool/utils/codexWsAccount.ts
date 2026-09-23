import type { PoolKeyDetail } from '@/api/endpoints/pool'

export interface CodexWsAccountPresentation {
  label: string
  title: string
  tone: 'warning' | 'danger'
}

export function supportsCodexWs(providerType: string | null | undefined, key: PoolKeyDetail): boolean {
  return providerType?.trim().toLowerCase() === 'codex'
    && key.auth_type.trim().toLowerCase() === 'oauth'
}

/** Built-in support is not a claim of request-scoped scheduling eligibility. */
export function describeCodexWsAccount(key: PoolKeyDetail): CodexWsAccountPresentation {
  if (!key.is_active) {
    return {
      label: 'WS 不可调度',
      title: '账号已停用，官方 Codex WebSocket 不可调度',
      tone: 'danger',
    }
  }
  return {
    label: 'WS 默认支持',
    title: 'Codex OAuth 账号默认支持官方 WebSocket，无需单独开启；全局开关、端点、代理路由、请求模型、额度、熔断和并发在每次调度时判定',
    tone: 'warning',
  }
}
