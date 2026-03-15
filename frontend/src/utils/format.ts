// Token formatting - intelligent display based on value size
export function formatTokens(num: number | undefined | null): string {
  if (num === undefined || num === null || num === 0) {
    return '0'
  }

  // For very small values (< 1000), show as is without unit
  if (num < 1000) {
    return num.toString()
  }

  // For values 1K-999K, show in thousands
  if (num < 1000000) {
    const thousands = num / 1000
    if (thousands >= 100) {
      return `${Math.round(thousands)  }K`
    } else if (thousands >= 10) {
      return `${thousands.toFixed(1)  }K`
    } else {
      return `${thousands.toFixed(2)  }K`
    }
  }

  // For values >= 1M, show in millions
  const millions = num / 1000000
  if (millions >= 100) {
    return `${Math.round(millions)  }M`
  } else if (millions >= 10) {
    return `${millions.toFixed(1)  }M`
  } else {
    return `${millions.toFixed(2)  }M`
  }
}

// Currency formatting with high precision for small values
export function formatCurrency(amount: number | undefined | null): string {
  if (amount === undefined || amount === null || amount === 0) {
    return '$0.00'
  }

  // For very small amounts (< $0.00001), show up to 8 decimal places
  if (amount > 0 && amount < 0.00001) {
    const formatted = amount.toFixed(8)
    // Remove trailing zeros but keep at least 2 decimal places
    const trimmed = formatted.replace(/(\.\d\d)0+$/, '$1')
    return `$${  trimmed}`
  }

  // For small amounts (< $0.0001), show up to 6 decimal places
  if (amount < 0.0001) {
    const formatted = amount.toFixed(6)
    // Remove trailing zeros but keep at least 2 decimal places
    const trimmed = formatted.replace(/(\.\d\d)0+$/, '$1')
    return `$${  trimmed}`
  }

  // For small amounts (< $0.01), show up to 5 decimal places
  if (amount < 0.01) {
    const formatted = amount.toFixed(5)
    // Remove trailing zeros but keep at least 2 decimal places
    const trimmed = formatted.replace(/(\.\d\d)0+$/, '$1')
    return `$${  trimmed}`
  }

  // For amounts less than $1, show 4 decimal places
  if (amount < 1) {
    const formatted = amount.toFixed(4)
    // Remove trailing zeros but keep at least 2 decimal places
    const trimmed = formatted.replace(/(\.\d\d)0+$/, '$1')
    return `$${  trimmed}`
  }

  // For amounts $1-$100, show 2-3 decimal places
  if (amount < 100) {
    const formatted = amount.toFixed(3)
    // Remove trailing zeros but keep at least 2 decimal places
    const trimmed = formatted.replace(/(\.\d\d)0+$/, '$1')
    return `$${  trimmed}`
  }

  // For larger amounts, show 2 decimal places
  return `$${  amount.toFixed(2)}`
}

// Number formatting with locale support
export function formatNumber(num: number | undefined | null): string {
  if (num === undefined || num === null) {
    return '0'
  }
  return num.toLocaleString('zh-CN')
}

// Date formatting
export function formatDate(dateString: string | undefined | null): string {
  if (!dateString) return '未知'

  return new Date(dateString).toLocaleDateString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit'
  })
}

// Model price formatting (already in per 1M tokens)
export function formatModelPrice(price: number | undefined | null): string {
  if (price === undefined || price === null) {
    return '$0.00'
  }

  // Price is already per 1M tokens, no conversion needed
  if (price < 1) {
    return `$${  price.toFixed(4).replace(/\.?0+$/, '').padEnd(price.toFixed(4).indexOf('.') + 3, '0')}`
  } else {
    return `$${  price.toFixed(2)}`
  }
}

// Billing type formatting
export function formatBillingType(type: string | undefined | null): string {
  const typeMap: Record<string, string> = {
    'pay_as_you_go': '按量付费',
    'monthly_quota': '月卡配额',
    'free_tier': '免费套餐'
  }
  return typeMap[type || ''] || type || '按量付费'
}

// Format cost with 4 decimal places (for cache analysis)
export function formatCost(cost: number | null | undefined): string {
  if (cost === null || cost === undefined) return '-'
  return `$${cost.toFixed(4)}`
}

// Usage count formatting (compact display for large numbers)
export function formatUsageCount(count: number): string {
  if (count >= 1000000) {
    return `${(count / 1000000).toFixed(1)}M`
  } else if (count >= 1000) {
    return `${(count / 1000).toFixed(1)}K`
  }
  return count.toString()
}

// Format remaining time from unix timestamp
export function formatRemainingTime(expireAt: number | undefined, currentTime: number): string {
  if (!expireAt) return '未知'
  const remaining = expireAt - currentTime
  if (remaining <= 0) return '已过期'

  const minutes = Math.floor(remaining / 60)
  const seconds = Math.floor(remaining % 60)
  return `${minutes}分${seconds}秒`
}

// Cache hit rate formatting
export function formatHitRate(rate: number | undefined): string {
  if (typeof rate !== 'number' || Number.isNaN(rate)) return '-'
  return `${rate.toFixed(2)}%`
}

// Rate limit formatting (supports "inherit" semantics: null = inherit system default)
export function formatRateLimitInheritable(rateLimit?: number | null): string {
  if (rateLimit == null) return '跟随系统'
  if (rateLimit === 0) return '不限速'
  return `${rateLimit}/min`
}

// Rate limit formatting (simple: null/0 both mean unlimited)
export function formatRateLimitSimple(rateLimit?: number | null): string {
  if (rateLimit == null || rateLimit === 0) return '不限速'
  return `${rateLimit}/min`
}

// Rate limit state helpers
export function isRateLimitInherited(rateLimit?: number | null): boolean {
  return rateLimit == null
}

export function isRateLimitUnlimited(rateLimit?: number | null): boolean {
  return rateLimit === 0
}
