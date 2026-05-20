import type { ProviderKeyBalanceSummary } from '@/api/endpoints'

export function toFiniteNumber(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value === 'string' && value.trim()) {
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : null
  }
  return null
}

export function hasKeyBalanceSummary(summary: ProviderKeyBalanceSummary | null | undefined): summary is ProviderKeyBalanceSummary {
  if (!summary) return false
  const updatedAt = toFiniteNumber(summary.updated_at)
  if (updatedAt === null) return false
  return toFiniteNumber(summary.total_available) !== null
    || toFiniteNumber(summary.total_used) !== null
    || toFiniteNumber(summary.total_granted) !== null
}

export function formatKeyBalanceAmount(value: unknown, currency = 'USD'): string {
  const numberValue = toFiniteNumber(value)
  if (numberValue === null) return '未知'
  const normalizedCurrency = (currency || 'USD').toUpperCase()
  const prefix = normalizedCurrency === 'USD'
    ? '$'
    : normalizedCurrency === 'CNY'
      ? '¥'
      : `${normalizedCurrency} `
  const decimals = Math.abs(numberValue) >= 100 ? 2 : 4
  return `${prefix}${numberValue.toFixed(decimals)}`
}

export function keyBalanceTemplateLabel(architectureId: unknown): string {
  const normalized = String(architectureId || '').trim().toLowerCase().replace(/-/g, '_')
  if (normalized === 'newapi' || normalized === 'new_api') return 'NewAPI'
  if (normalized === 'sub2api') return 'Sub2API'
  if (normalized === 'generic' || normalized === 'custom' || normalized === 'generic_api') return '自定义'
  return normalized || '余额查询'
}

export function formatKeyBalanceUpdatedAt(updatedAt: unknown): string {
  const timestamp = toFiniteNumber(updatedAt)
  if (timestamp === null || timestamp <= 0) return ''
  const now = Math.floor(Date.now() / 1000)
  const diff = now - timestamp
  if (diff <= 60) return '刚刚更新'
  const minutes = Math.floor(diff / 60)
  if (minutes < 60) return `${minutes}分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}小时前`
  const days = Math.floor(hours / 24)
  return `${days}天前`
}
