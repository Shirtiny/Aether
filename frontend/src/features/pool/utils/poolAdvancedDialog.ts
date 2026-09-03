import type {
  PoolCodexClientHeaderProfile,
  PoolCodexClientHeadersConfig,
  PoolCodexRuntimeIdentityConfig,
} from '@/api/endpoints/types/provider'

import defaultCodexClientHeaderProfiles from '../../../../../resources/codex-client-header-profiles.json'

export const DEFAULT_CODEX_CLIENT_HEADER_PROFILES: readonly PoolCodexClientHeaderProfile[] =
  defaultCodexClientHeaderProfiles

export function buildDefaultCodexClientHeaderProfiles(): PoolCodexClientHeaderProfile[] {
  return DEFAULT_CODEX_CLIENT_HEADER_PROFILES.map((profile) => ({ ...profile }))
}

export function buildCodexClientHeadersConfig(
  enabled: boolean,
  profiles: readonly PoolCodexClientHeaderProfile[],
): PoolCodexClientHeadersConfig {
  const normalized = profiles.map((profile) => ({
    user_agent: profile.user_agent.trim(),
    originator: profile.originator.trim(),
  }))
  const invalidIndex = normalized.findIndex(profile => !profile.user_agent || !profile.originator)
  if (invalidIndex >= 0) {
    throw new Error(`第 ${invalidIndex + 1} 组 User-Agent 和 Originator 必须同时填写`)
  }
  const seen = new Set<string>()
  for (const [index, profile] of normalized.entries()) {
    const identity = `${profile.user_agent}\u0000${profile.originator}`
    if (seen.has(identity)) {
      throw new Error(`第 ${index + 1} 组 Codex 请求头与已有配置重复`)
    }
    seen.add(identity)
  }
  return {
    enabled,
    profiles: normalized.length > 0 ? normalized : undefined,
  }
}

// 与后端 `codex_runtime_identity` 校验范围一致（apps/aether-gateway/src/codex_runtime_identity.rs）。
export const CODEX_RUNTIME_IDENTITY_THREADS_PER_DAY_RANGE = { min: 1, max: 64 } as const
export const CODEX_RUNTIME_IDENTITY_TURNS_PER_DAY_RANGE = { min: 1, max: 512 } as const
export const DEFAULT_CODEX_RUNTIME_IDENTITY_THREADS_PER_DAY = 8
export const DEFAULT_CODEX_RUNTIME_IDENTITY_TURNS_PER_DAY = 64

function normalizeBoundedInteger(
  value: number | null | undefined,
  range: { readonly min: number, readonly max: number },
): number | null {
  if (typeof value !== 'number' || !Number.isInteger(value)) return null
  if (value < range.min || value > range.max) return null
  return value
}

/**
 * 构造号池「会话身份合成」配置。开启时两个上限必填且必须在范围内；
 * 关闭时仍保留合法的数值，便于重新开启时沿用上次的设置。
 */
export function buildCodexRuntimeIdentityConfig(
  enabled: boolean,
  expectedThreadsPerDay: number | null | undefined,
  expectedTurnsPerDay: number | null | undefined,
): PoolCodexRuntimeIdentityConfig {
  const threads = normalizeBoundedInteger(expectedThreadsPerDay, CODEX_RUNTIME_IDENTITY_THREADS_PER_DAY_RANGE)
  const turns = normalizeBoundedInteger(expectedTurnsPerDay, CODEX_RUNTIME_IDENTITY_TURNS_PER_DAY_RANGE)
  if (enabled) {
    if (threads === null) {
      const { min, max } = CODEX_RUNTIME_IDENTITY_THREADS_PER_DAY_RANGE
      throw new Error(`每日预期 Thread 数必须是 ${min} 到 ${max} 之间的整数`)
    }
    if (turns === null) {
      const { min, max } = CODEX_RUNTIME_IDENTITY_TURNS_PER_DAY_RANGE
      throw new Error(`每日预期 Turn 数必须是 ${min} 到 ${max} 之间的整数`)
    }
    return {
      enabled: true,
      expected_threads_per_day: threads,
      expected_turns_per_day: turns,
    }
  }
  return {
    enabled: false,
    expected_threads_per_day: threads ?? undefined,
    expected_turns_per_day: turns ?? undefined,
  }
}

export type PoolHealthToggleKey =
  | 'health_policy_enabled'
  | 'probing_enabled'
  | 'account_self_check_enabled'
  | 'auto_remove_banned_keys'
  | 'skip_exhausted_accounts'
  | 'sticky_collateral_avoidance_enabled'
  | 'avoid_anonymous'
  | 'codex_quota_weekly_basis'

export interface PoolHealthToggleCard {
  key: PoolHealthToggleKey
  label: string
  description: string
}

export interface PoolCooldownFieldLayout {
  fields: string[]
  desktopColumnsClass: string
}

export interface PoolSecondarySectionLayout {
  wrapperClass: string
}

export interface PoolCostFieldLayout {
  fields: string[]
  desktopColumnsClass: string
}

export function buildPoolHealthToggleCards(): PoolHealthToggleCard[] {
  return [
    {
      key: 'health_policy_enabled',
      label: '健康策略',
      description: '按上游错误自动冷却并跳过异常账号。',
    },
    {
      key: 'probing_enabled',
      label: '自适应热池',
      description: '自动维护热池，缺口时异步补位。',
    },
    {
      key: 'account_self_check_enabled',
      label: '账号自检',
      description: '定时确认账号状态，策略由提供商适配器内置。',
    },
    {
      key: 'auto_remove_banned_keys',
      label: '异常自动清除',
      description: '检测到不可恢复账号异常，或 RT 与 AT 均失效时自动从号池移除。',
    },
    {
      key: 'skip_exhausted_accounts',
      label: '跳过额度耗尽账号',
      description: '当 Codex / Kiro 账号额度已耗尽时，直接标记为不可调度并在请求侧跳过。',
    },
    {
      key: 'sticky_collateral_avoidance_enabled',
      label: '连坐避险',
      description: 'sticky 账号失效后跳过当前号池，避免同会话切到池内其他账号。',
    },
    {
      key: 'avoid_anonymous',
      label: '回避匿名',
      description: '无客户端 session 且不发生格式转换的请求会跳过该提供商，并继续尝试其它候选。',
    },
    {
      key: 'codex_quota_weekly_basis',
      label: '周限优先',
      description: 'Codex 账号按周限判断额度耗尽；关闭后按 5 小时窗口判断。',
    },
  ]
}

export function buildPoolCooldownFieldLayout(): PoolCooldownFieldLayout {
  return {
    fields: [
      'rate_limit_cooldown_seconds',
      'overload_cooldown_seconds',
      'sticky_session_ttl_seconds',
      'global_priority',
    ],
    desktopColumnsClass: 'xl:grid-cols-4',
  }
}

export function buildPoolSecondarySectionLayout(): PoolSecondarySectionLayout {
  return {
    wrapperClass: 'space-y-4',
  }
}

export function buildPoolCostFieldLayout(): PoolCostFieldLayout {
  return {
    fields: [
      'cost_window_seconds',
      'cost_limit_per_key_tokens',
      'cost_soft_threshold_percent',
    ],
    desktopColumnsClass: 'xl:grid-cols-3',
  }
}

export function isCodexFiveHourQuotaBasis(value: unknown): boolean {
  if (typeof value !== 'string') return false
  const normalized = value.trim().toLowerCase().replace(/[-\s]+/g, '_')
  return ['5h', 'five_hour', 'five_hours', '5_hour', '5_hours'].includes(normalized)
}
