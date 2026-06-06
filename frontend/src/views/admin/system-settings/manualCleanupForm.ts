export const MANUAL_USAGE_CLEANUP_CONFIRM_PHRASE = '确认清理'

export type ManualCleanupMode = 'policy' | 'older_than_days' | 'before_now'
export type ManualCleanupTarget = 'detail_body' | 'compressed_body' | 'headers' | 'records'

export const MANUAL_CLEANUP_TARGETS: ManualCleanupTarget[] = [
  'detail_body',
  'compressed_body',
  'headers',
  'records',
]

export const BEFORE_NOW_ALLOWED_TARGETS: ManualCleanupTarget[] = [
  'detail_body',
  'compressed_body',
]

export function normalizeConfirmPhraseInput(raw: string): string {
  return raw.replace(/\r?\n/g, '').trim()
}

export function isConfirmPhraseMatched(raw: string): boolean {
  return normalizeConfirmPhraseInput(raw) === MANUAL_USAGE_CLEANUP_CONFIRM_PHRASE
}

export function normalizeOlderThanDaysInput(raw: string | number | null | undefined): number | null {
  if (raw === null || raw === undefined || raw === '') return null
  const parsed = typeof raw === 'number' ? raw : Number(raw)
  if (!Number.isFinite(parsed) || parsed <= 0) return null
  return Math.floor(parsed)
}

export function allowedTargetsForMode(mode: ManualCleanupMode): ManualCleanupTarget[] {
  return mode === 'before_now' ? BEFORE_NOW_ALLOWED_TARGETS : MANUAL_CLEANUP_TARGETS
}

export function normalizeManualCleanupTargets(
  mode: ManualCleanupMode,
  targets: ManualCleanupTarget[],
): ManualCleanupTarget[] {
  const allowed = new Set(allowedTargetsForMode(mode))
  return targets.filter((target, index) =>
    allowed.has(target) && targets.indexOf(target) === index
  )
}

export function defaultManualCleanupTargets(mode: ManualCleanupMode): ManualCleanupTarget[] {
  return allowedTargetsForMode(mode)
}
