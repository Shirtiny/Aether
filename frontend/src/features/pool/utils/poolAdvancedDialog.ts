import type { PoolCodexClientHeaderProfile } from '@/api/endpoints/types/provider'

export const DEFAULT_CODEX_CLIENT_HEADER_PROFILES: readonly PoolCodexClientHeaderProfile[] = [
  {
    user_agent: 'codex-tui/0.142.0 (Mac OS 26.4.1; arm64) iTerm.app/3.6.10 (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.142.0 (Windows 10.0.26200; x86_64) WindowsTerminal (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.142.0 (Debian 13.0.0; x86_64) xterm-256color (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.142.0 (Ubuntu 22.4.0; x86_64) WindowsTerminal (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.142.0 (Ubuntu 24.4.0; x86_64) WindowsTerminal (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.142.0 (Ubuntu 24.4.0; x86_64) WezTerm/20240203-110809-5046fc22 (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.142.0 (Mac OS 26.2.0; arm64) xterm-256color (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.142.0 (Mac OS 15.6.1; arm64) Apple_Terminal (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.142.0 (Windows 10.0.26200; x86_64) WarpTerminal (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.142.0 (Mac OS 26.5.1; arm64) ghostty/1.3.1 (codex-tui; 0.142.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.141.0 (Debian 13.0.0; x86_64) xterm-256color (codex-tui; 0.141.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.141.0 (Mac OS 15.7.5; arm64) iTerm.app/3.6.6 (codex-tui; 0.141.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.141.0 (Windows 10.0.26200; x86_64) waveterm (codex-tui; 0.141.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.141.0 (Mac OS 26.2.0; arm64) vscode/1.125.0 (codex-tui; 0.141.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'codex-tui/0.134.0 (Mac OS 14.1.0; arm64) iTerm.app/3.6.9 (codex-tui; 0.134.0)',
    originator: 'codex-tui',
  },
  {
    user_agent: 'Codex Desktop/0.142.0 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.616.71553)',
    originator: 'Codex Desktop',
  },
  {
    user_agent: 'Codex Desktop/0.142.0 (Windows 10.0.19045; x86_64) unknown (Codex Desktop; 26.616.81150)',
    originator: 'Codex Desktop',
  },
  {
    user_agent: 'Codex Desktop/0.142.0 (Mac OS 26.5.1; arm64) unknown (Codex Desktop; 26.616.71553)',
    originator: 'Codex Desktop',
  },
  {
    user_agent: 'Codex Desktop/0.142.0-alpha.6 (Mac OS 26.5.0; arm64) unknown (Codex Desktop; 26.616.51431)',
    originator: 'Codex Desktop',
  },
  {
    user_agent: 'Codex Desktop/0.142.0 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.616.81150)',
    originator: 'Codex Desktop',
  },
  {
    user_agent: 'Codex Desktop/0.142.0 (Mac OS 26.5.0; arm64) unknown (Codex Desktop; 26.616.81150)',
    originator: 'Codex Desktop',
  },
  {
    user_agent: 'Codex Desktop/0.142.0 (Mac OS 14.1.0; arm64) unknown (Codex Desktop; 26.616.81150)',
    originator: 'Codex Desktop',
  },
  {
    user_agent: 'Codex Desktop/0.142.0 (Mac OS 13.1.0; x86_64) unknown (Codex Desktop; 26.616.81150)',
    originator: 'Codex Desktop',
  },
  {
    user_agent: 'codex_vscode/0.142.0 (Windows 10.0.19045; x86_64) unknown (VS Code; 26.616.81150)',
    originator: 'codex_vscode',
  },
  {
    user_agent: 'codex_vscode/0.142.0-alpha.1 (Windows 10.0.22631; x86_64) unknown (Windsurf; 26.616.32156)',
    originator: 'codex_vscode',
  },
  {
    user_agent: 'codex_vscode/0.142.0 (Windows 10.0.22631; x86_64) unknown (Antigravity IDE; 26.616.71553)',
    originator: 'codex_vscode',
  },
  {
    user_agent: 'codex_cli_rs/0.93.0 (Windows 10.0.26200; x86_64) vscode/1.108.1',
    originator: 'codex_cli',
  },
  {
    user_agent: 'codex_cli_rs/0.133.0 (Windows 10.0.26200; x64)',
    originator: 'codex_cli_rs',
  },
  {
    user_agent: 'codex_cli_rs/0.125.0 (Mac OS 24.6.0; arm64)',
    originator: 'codex_cli_rs',
  },
  {
    user_agent: 'codex_cli_rs/0.77.0 (Windows 10.0.26100; x86_64) WindowsTerminal',
    originator: 'codex_cli_rs',
  },
  {
    user_agent: 'codex_exec/0.142.0 (Mac OS 15.7.5; arm64) iTerm.app/3.6.6 (codex_exec; 0.142.0)',
    originator: 'codex_exec',
  },
  {
    user_agent: 'codex_sdk_ts/0.136.0 (Windows 10.0.19045; x86_64) unknown (codex_exec; 0.136.0)',
    originator: 'codex_sdk_ts',
  },
]

export function buildDefaultCodexClientHeaderProfiles(): PoolCodexClientHeaderProfile[] {
  return DEFAULT_CODEX_CLIENT_HEADER_PROFILES.map((profile) => ({ ...profile }))
}

export type PoolHealthToggleKey =
  | 'health_policy_enabled'
  | 'probing_enabled'
  | 'account_self_check_enabled'
  | 'auto_remove_banned_keys'
  | 'skip_exhausted_accounts'
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
