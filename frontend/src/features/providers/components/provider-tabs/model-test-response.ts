type JsonRecord = Record<string, unknown>

function isRecord(value: unknown): value is JsonRecord {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

export function formatModelTestResponseBody(value: unknown): string {
  if (value === undefined || value === null) return ''
  return typeof value === 'string' ? value : JSON.stringify(value, null, 2)
}

function join(parts: Array<string | null>): string | null {
  const present = parts.filter((part): part is string => part !== null && part !== '')
  return present.length ? present.join('\n\n') : null
}

// Unlike the compact table preview, this preserves every text part and its
// whitespace. The complete response JSON remains available for non-text fields.
function contentText(value: unknown): string | null {
  if (typeof value === 'string') return value || null
  if (Array.isArray(value)) return join(value.map(contentText))
  if (!isRecord(value)) return null
  const text = typeof value.text === 'string' ? value.text : null
  const thinking = typeof value.thinking === 'string' ? value.thinking : null
  const refusal = typeof value.refusal === 'string' ? value.refusal : null
  return join([
    text,
    thinking,
    refusal,
    contentText(value.content),
    contentText(value.parts),
    contentText(value.summary),
  ])
}

export function extractModelTestResponseText(value: unknown): string | null {
  if (typeof value === 'string') {
    try {
      const parsed: unknown = JSON.parse(value)
      if (isRecord(parsed) || Array.isArray(parsed)) return extractModelTestResponseText(parsed)
    } catch { /* Plain-text and HTML error bodies are displayed verbatim, not rendered. */ }
    return value || null
  }
  if (Array.isArray(value)) return join(value.map(extractModelTestResponseText))
  if (!isRecord(value)) return null

  for (const wrapper of [value.response, value.body]) {
    const text = extractModelTestResponseText(wrapper)
    if (text !== null) return text
  }
  if (Array.isArray(value.choices)) {
    return join(value.choices.map(choice => {
      if (!isRecord(choice)) return null
      const message = isRecord(choice.message) ? choice.message : isRecord(choice.delta) ? choice.delta : choice
      return join([
        contentText(message.reasoning_content),
        contentText(message.thinking),
        contentText(message.content ?? message.text),
        contentText(message.refusal),
      ])
    }))
  }
  if (Array.isArray(value.candidates)) {
    return join(value.candidates.map(candidate => isRecord(candidate) ? contentText(candidate.content) : null))
  }
  return join([
    contentText(value.reasoning_content),
    contentText(value.thinking),
    contentText(value.output) ?? contentText(value.output_text) ?? contentText(value.content),
  ])
}
