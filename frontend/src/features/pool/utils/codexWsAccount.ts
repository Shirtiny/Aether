import type { PoolKeyDetail } from '@/api/endpoints/pool'
import type { CodexWsAccountStatus } from '@/api/endpoints/keys'
import { CODEX_WS_MANIFEST } from '@/constants/codexWs'

export interface CodexWsAccountPresentation {
  label: string
  title: string
  tone: 'success' | 'warning' | 'danger' | 'muted'
}

export function canConfigureCodexWs(providerType: string | null | undefined, key: PoolKeyDetail): boolean {
  return providerType?.trim().toLowerCase() === 'codex'
    && key.auth_type.trim().toLowerCase() === 'oauth'
}

export function isCodexWsEnabled(key: PoolKeyDetail): boolean {
  return key.capabilities?.codex_official_ws === true
}

export function hasValidCodexWsManifest(key: PoolKeyDetail): boolean {
  const profile = key.fingerprint?.websocket_transport_profile
  if (!profile || typeof profile !== 'object' || Array.isArray(profile)) return false
  return Object.entries(CODEX_WS_MANIFEST).every(
    ([field, expected]) => (profile as Record<string, unknown>)[field] === expected,
  )
}

/**
 * Presents configuration truth separately from request-scoped scheduler truth.
 * A successful toggle must never be rendered as runtime-eligible when the
 * backend did not evaluate a concrete proxy route, model and runtime snapshot.
 */
export function describeCodexWsAccount(
  key: PoolKeyDetail,
  status?: CodexWsAccountStatus,
): CodexWsAccountPresentation {
  if (!status) {
    if (!isCodexWsEnabled(key)) {
      return {
        label: 'WS 关闭',
        title: '官方 Codex WebSocket 未启用；HTTP 不受影响',
        tone: 'muted',
      }
    }
    if (!hasValidCodexWsManifest(key)) {
      return {
        label: 'WS 配置异常',
        title: '官方 Codex WebSocket profile 不完整，切换关闭后重新启用可修复',
        tone: 'danger',
      }
    }
    return {
      label: 'WS 已配置',
      title: '官方 Codex WebSocket profile 已配置；代理路由、请求模型、额度、熔断和并发在每次调度时判定',
      tone: 'warning',
    }
  }

  if (!status.configured) {
    return {
      label: 'WS 关闭',
      title: '官方 Codex WebSocket 已关闭；现有连接软排空，HTTP 不受影响',
      tone: 'muted',
    }
  }
  if (status.runtime_state === 'hard_revoked') {
    return {
      label: 'WS 已撤销',
      title: '账号已停用，官方 Codex WebSocket 不可调度',
      tone: 'danger',
    }
  }
  if (!status.profile_effective) {
    const reasons = status.profile_reasons.length > 0
      ? `：${status.profile_reasons.join(', ')}`
      : ''
    return {
      label: 'WS 前置阻塞',
      title: `官方 Codex WebSocket 静态前置条件未满足${reasons}`,
      tone: 'danger',
    }
  }
  if (status.runtime_eligible === true) {
    return {
      label: 'WS 可调度',
      title: `官方 Codex WebSocket 已通过本次运行时资格判定（${status.profile_id || '固定 profile'}）`,
      tone: 'success',
    }
  }
  if (status.runtime_eligible === false) {
    const reasons = status.runtime_reasons.length > 0
      ? `：${status.runtime_reasons.join(', ')}`
      : ''
    return {
      label: 'WS 不可调度',
      title: `官方 Codex WebSocket 当前不可调度${reasons}`,
      tone: 'danger',
    }
  }
  return {
    label: 'WS 配置有效',
    title: '官方 Codex WebSocket profile 有效；代理路由、请求模型、额度、熔断和并发在每次调度时判定',
    tone: 'warning',
  }
}
