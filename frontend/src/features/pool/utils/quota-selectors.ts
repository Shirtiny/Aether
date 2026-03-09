export function normalizeQuotaSegment(value: string | null | undefined): string {
  return String(value || '')
    .trim()
    .toLowerCase()
    .replace(/％/g, '%')
}

export function getQuotaSegments(accountQuota: string | null | undefined): string[] {
  return String(accountQuota || '')
    .split('|')
    .map((segment) => normalizeQuotaSegment(segment))
    .filter(Boolean)
}

function hasDepletedKeyword(segment: string): boolean {
  return /(无额度|额度不足|已耗尽|耗尽|depleted|exhausted|insufficient)/.test(segment)
}

function hasZeroRemainingText(segment: string): boolean {
  return /剩余\s*0(?:\.0+)?(?!\d)/.test(segment)
}

function hasZeroRatio(segment: string): boolean {
  return [...segment.matchAll(/(\d+(?:\.\d+)?)\s*\/\s*(\d+(?:\.\d+)?)/g)]
    .some(([, used, total]) => Number(used) === 0 && Number(total) > 0)
}

function hasZeroPercent(segment: string): boolean {
  return [...segment.matchAll(/(\d+(?:\.\d+)?)\s*%/g)]
    .some(([, percent]) => Number(percent) === 0)
}

export function isDepletedQuotaSegment(segment: string): boolean {
  if (hasDepletedKeyword(segment)) return true
  if (hasZeroRemainingText(segment)) return true
  if (hasZeroRatio(segment)) return true
  if (hasZeroPercent(segment)) return true
  return false
}

export function hasNoFiveHourLimit(accountQuota: string | null | undefined): boolean {
  return getQuotaSegments(accountQuota)
    .filter((segment) => /5h|5小时/.test(segment))
    .some(isDepletedQuotaSegment)
}

export function hasNoWeeklyLimit(accountQuota: string | null | undefined): boolean {
  return getQuotaSegments(accountQuota)
    .filter((segment) => /周|weekly|week/.test(segment))
    .some(isDepletedQuotaSegment)
}
