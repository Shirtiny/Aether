export interface PromptCaptureItemView {
  source: string
  role: string
  sha256: string
  chars: number | null
  preview: string
  truncated: boolean
}

export interface PromptCaptureView {
  itemCount: number
  roleCounts: Record<string, number>
  items: PromptCaptureItemView[]
}

type JsonRecord = Record<string, unknown>

function asRecord(value: unknown): JsonRecord | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null
  return value as JsonRecord
}

function asString(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

function asNumber(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value === 'string') {
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : null
  }
  return null
}

function asBoolean(value: unknown): boolean {
  return value === true
}

function normalizeRoleCounts(value: unknown): Record<string, number> {
  const record = asRecord(value)
  if (!record) return {}

  return Object.fromEntries(
    Object.entries(record)
      .map(([role, count]) => [role, asNumber(count)] as const)
      .filter((entry): entry is readonly [string, number] => entry[1] !== null),
  )
}

function normalizePromptCaptureItem(value: unknown): PromptCaptureItemView | null {
  const record = asRecord(value)
  if (!record) return null

  const preview = asString(record.preview)
  const sha256 = asString(record.sha256)
  const source = asString(record.source)
  const role = asString(record.role)

  if (!preview && !sha256 && !source && !role) return null

  return {
    source,
    role,
    sha256,
    chars: asNumber(record.chars),
    preview,
    truncated: asBoolean(record.truncated),
  }
}

export function extractPromptCaptureMetadata(
  metadata: Record<string, unknown> | null | undefined,
): PromptCaptureView | null {
  const root = asRecord(metadata)
  if (!root) return null

  const capture = asRecord(root.prompt_capture)
    ?? asRecord(asRecord(root.request_metadata)?.prompt_capture)
  if (!capture) return null

  const items = Array.isArray(capture.items)
    ? capture.items
        .map(normalizePromptCaptureItem)
        .filter((item): item is PromptCaptureItemView => item !== null)
    : []

  if (items.length === 0) return null

  return {
    itemCount: asNumber(capture.item_count) ?? items.length,
    roleCounts: normalizeRoleCounts(capture.role_counts),
    items,
  }
}
