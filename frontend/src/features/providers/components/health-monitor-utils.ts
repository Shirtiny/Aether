export type HealthBadgeVariant =
  | 'default'
  | 'secondary'
  | 'destructive'
  | 'outline'
  | 'success'
  | 'warning'
  | 'dark'

export interface HealthMonitorAvailability {
  total_attempts: number
  success_rate: number
}

export function getHealthLabel(
  item: HealthMonitorAvailability,
  emptyLabel = '暂无请求'
) {
  if (item.total_attempts <= 0) return emptyLabel
  if (item.success_rate >= 0.95) return '正常'
  if (item.success_rate >= 0.8) return '波动'
  return '异常'
}

export function getHealthBadgeVariant(
  item: HealthMonitorAvailability
): HealthBadgeVariant {
  if (item.total_attempts <= 0) return 'outline'
  if (item.success_rate >= 0.95) return 'success'
  if (item.success_rate >= 0.8) return 'warning'
  return 'destructive'
}

export function getSuccessRateClass(rate: number) {
  if (rate >= 0.95) return 'text-green-600 dark:text-green-400'
  if (rate >= 0.8) return 'text-amber-600 dark:text-amber-400'
  return 'text-red-600 dark:text-red-400'
}

export function getAvailabilityClass(item: HealthMonitorAvailability) {
  if (item.total_attempts <= 0) return ''
  return getSuccessRateClass(item.success_rate)
}

export function formatMs(value?: number | null) {
  if (typeof value !== 'number' || Number.isNaN(value)) return '-'
  const absValue = Math.abs(value)
  if (absValue < 1000) return `${Math.round(value)} ms`
  if (absValue < 60_000) return `${formatDurationNumber(value / 1000)} s`
  return `${formatDurationNumber(value / 60_000)} min`
}

function formatDurationNumber(value: number) {
  return new Intl.NumberFormat('zh-CN', {
    maximumFractionDigits: Math.abs(value) < 10 ? 2 : 1
  }).format(value)
}

export function formatPercent(value: number) {
  if (typeof value !== 'number' || Number.isNaN(value)) return '-'
  return `${(value * 100).toFixed(2)}%`
}

export function formatAvailability(item: HealthMonitorAvailability) {
  if (item.total_attempts <= 0) return '-'
  return formatPercent(item.success_rate)
}

export function formatTps(value?: number | null) {
  if (typeof value !== 'number' || Number.isNaN(value)) return '-'
  return `${new Intl.NumberFormat('zh-CN', {
    maximumFractionDigits: value < 10 ? 2 : value < 100 ? 1 : 0
  }).format(value)} tps`
}

export function formatCompactNumber(value: number) {
  return new Intl.NumberFormat('zh-CN', {
    notation: 'compact',
    maximumFractionDigits: 1
  }).format(value)
}

export function formatTimestamp(timestamp?: string | null) {
  if (!timestamp) return '未知时间'
  const date = new Date(timestamp)
  if (Number.isNaN(date.getTime())) return '未知时间'
  return date.toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit'
  })
}

export function getTimelineColor(status: string) {
  switch (status) {
    case 'healthy':
      return 'bg-green-500/80 dark:bg-green-400/90'
    case 'warning':
      return 'bg-amber-400/80 dark:bg-amber-300/80'
    case 'unhealthy':
      return 'bg-red-500/80 dark:bg-red-400/90'
    default:
      return 'bg-gray-300 dark:bg-gray-600'
  }
}

export function getTimelineLabel(status: string) {
  switch (status) {
    case 'healthy':
      return '健康'
    case 'warning':
      return '波动'
    case 'unhealthy':
      return '异常'
    default:
      return '无请求'
  }
}
