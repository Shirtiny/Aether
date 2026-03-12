export type PasswordPolicyLevel = 'weak' | 'medium' | 'strong'

export const PASSWORD_POLICY_OPTIONS: Array<{
  value: PasswordPolicyLevel
  label: string
  description: string
}> = [
  {
    value: 'weak',
    label: '弱密码',
    description: '至少 6 个字符',
  },
  {
    value: 'medium',
    label: '中等密码',
    description: '至少 8 个字符，且包含字母和数字',
  },
  {
    value: 'strong',
    label: '强密码',
    description: '至少 8 个字符，且包含大小写字母、数字和特殊字符',
  },
]

export function normalizePasswordPolicyLevel(value: unknown): PasswordPolicyLevel {
  if (value === 'medium' || value === 'strong') {
    return value
  }
  return 'weak'
}

export function getPasswordPolicyHint(level: unknown): string {
  switch (normalizePasswordPolicyLevel(level)) {
    case 'medium':
      return '至少 8 个字符，且需包含字母和数字'
    case 'strong':
      return '至少 8 个字符，且需包含大写字母、小写字母、数字和特殊字符'
    case 'weak':
    default:
      return '至少 6 个字符'
  }
}

export function getPasswordPolicyPlaceholder(level: unknown): string {
  switch (normalizePasswordPolicyLevel(level)) {
    case 'medium':
      return '至少 8 位，含字母和数字'
    case 'strong':
      return '至少 8 位，含大小写字母、数字和特殊字符'
    case 'weak':
    default:
      return '至少 6 个字符'
  }
}

export function validatePasswordByPolicy(password: string, level: unknown): string {
  if (!password) {
    return ''
  }

  const normalized = normalizePasswordPolicyLevel(level)

  if (password.length < 6) {
    return '密码长度至少为6个字符'
  }

  if (normalized === 'medium') {
    if (password.length < 8) {
      return '密码长度至少为8个字符'
    }
    if (!/[A-Za-z]/.test(password)) {
      return '密码必须包含至少一个字母'
    }
    if (!/[0-9]/.test(password)) {
      return '密码必须包含至少一个数字'
    }
  }

  if (normalized === 'strong') {
    if (password.length < 8) {
      return '密码长度至少为8个字符'
    }
    if (!/[A-Z]/.test(password)) {
      return '密码必须包含至少一个大写字母'
    }
    if (!/[a-z]/.test(password)) {
      return '密码必须包含至少一个小写字母'
    }
    if (!/[0-9]/.test(password)) {
      return '密码必须包含至少一个数字'
    }
    if (!/[!@#$%^&*()_+\-=[\]{};:'",.<>?/\\|`~]/.test(password)) {
      return '密码必须包含至少一个特殊字符'
    }
  }

  return ''
}
