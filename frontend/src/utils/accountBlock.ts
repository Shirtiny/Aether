// -- 按原因细分的关键词组 --

// 封禁类 (suspended / banned)
const KEYWORDS_SUSPENDED = [
  'suspended',
  'account_block',
  'account blocked',
  '封禁',
  '封号',
  '被封',
  '账户已封禁',
  '账号异常',
]

// 停用类 (disabled / deactivated)
const KEYWORDS_DISABLED = [
  'account has been disabled',
  'account disabled',
  'account has been deactivated',
  'account_deactivated',
  'account deactivated',
  'organization has been disabled',
  'organization_disabled',
  'deactivated_workspace',
  'deactivated',
  '访问被禁止',
  '账户访问被禁止',
]

const KEYWORDS_TOKEN_INVALID = [
  'authentication token has been invalidated',
  'token has been invalidated',
  'codex token 无效或已过期',
]

// 需要验证类
const KEYWORDS_VERIFICATION = [
  'validation_required',
  'verify your account',
]

// 合并的完整列表
const ACCOUNT_BLOCK_REASON_KEYWORDS = [
  ...KEYWORDS_SUSPENDED,
  ...KEYWORDS_DISABLED,
  ...KEYWORDS_TOKEN_INVALID,
  ...KEYWORDS_VERIFICATION,
]

export function isAccountLevelBlockReason(reason: string | null | undefined): boolean {
  if (!reason) return false
  const text = reason.trim()
  if (!text) return false
  if (text.startsWith('[ACCOUNT_BLOCK]')) return true
  if (text.startsWith('[OAUTH_EXPIRED]')) return true
  const lowered = text.toLowerCase()
  return ACCOUNT_BLOCK_REASON_KEYWORDS.some(keyword => lowered.includes(keyword))
}

export function classifyAccountBlockLabel(reason: string): string {
  if (reason.trim().startsWith('[OAUTH_EXPIRED]')) return 'Token 失效'
  const lowered = reason.toLowerCase()
  if (KEYWORDS_TOKEN_INVALID.some(kw => lowered.includes(kw))) return 'Token 失效'
  if (KEYWORDS_VERIFICATION.some(kw => lowered.includes(kw))) return '需要验证'
  if (lowered.includes('deactivated_workspace')) return '工作区停用'
  if (KEYWORDS_DISABLED.some(kw => lowered.includes(kw))) return '账号停用'
  if (KEYWORDS_SUSPENDED.some(kw => lowered.includes(kw))) return '账号封禁'
  return '账号异常'
}

export function cleanAccountBlockReason(reason: string): string {
  return reason.replace(/^\[(ACCOUNT_BLOCK|OAUTH_EXPIRED)\]\s*/i, '').trim()
}

export function isRefreshFailedReason(reason: string | null | undefined): boolean {
  if (!reason) return false
  return reason.trim().startsWith('[REFRESH_FAILED]')
}

export function isOAuthExpiredReason(reason: string | null | undefined): boolean {
  if (!reason) return false
  return reason.trim().startsWith('[OAUTH_EXPIRED]')
}
