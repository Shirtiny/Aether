import type { OAuthOrganizationInfo } from '@/api/endpoints/types/provider'

function formatOAuthIdentityShort(
  value: string | null | undefined,
  head = 8,
  tail = 6,
): string {
  const normalized = String(value || '').trim()
  if (!normalized) return ''
  if (normalized.length <= head + tail + 3) return normalized
  return `${normalized.slice(0, head)}...${normalized.slice(-tail)}`
}

function getPrimaryOAuthOrganizationId(
  value: { oauth_organizations?: OAuthOrganizationInfo[] | null } | null | undefined,
): string | null {
  const organizations = Array.isArray(value?.oauth_organizations) ? value.oauth_organizations : []
  const defaultOrg = organizations.find(
    (org) => org?.is_default && typeof org?.id === 'string' && org.id.trim(),
  )
  if (defaultOrg?.id) return defaultOrg.id.trim()
  const firstWithId = organizations.find(
    (org) => typeof org?.id === 'string' && org.id.trim(),
  )
  return firstWithId?.id?.trim() || null
}

export function getOAuthOrgBadge(
  value: { oauth_organizations?: OAuthOrganizationInfo[] | null } | null | undefined,
): { id: string; label: string } | null {
  const id = getPrimaryOAuthOrganizationId(value)
  if (!id) return null
  return { id, label: formatOAuthIdentityShort(id) }
}
