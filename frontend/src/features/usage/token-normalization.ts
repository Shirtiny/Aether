type UsageTokenLike = {
  effective_input_tokens?: number | null
  input_tokens?: number | null
  cache_creation_input_tokens?: number | null
  cache_creation_ephemeral_5m_input_tokens?: number | null
  cache_creation_ephemeral_1h_input_tokens?: number | null
  cache_read_input_tokens?: number | null
  reasoning_output_tokens?: number | null
  reasoning_tokens?: number | null
  tokens?: {
    reasoning_output?: number | null
    reasoning_output_tokens?: number | null
    reasoning?: number | null
    reasoning_tokens?: number | null
  } | null
  api_format?: string | null
  endpoint_api_format?: string | null
}

function toNonNegativeNumber(value: number | null | undefined): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.max(value, 0) : 0
}

function firstPositiveNumber(values: Array<number | null | undefined>): number {
  for (const value of values) {
    const normalized = toNonNegativeNumber(value)
    if (normalized > 0) return normalized
  }
  return 0
}

function apiFamily(apiFormat: string | null | undefined): string {
  return String(apiFormat || '')
    .split(':', 1)[0]
    .trim()
    .toLowerCase()
}

export function getCacheCreationTokens(usage: UsageTokenLike): number {
  const explicit = toNonNegativeNumber(usage.cache_creation_input_tokens)
  const classified = toNonNegativeNumber(usage.cache_creation_ephemeral_5m_input_tokens)
    + toNonNegativeNumber(usage.cache_creation_ephemeral_1h_input_tokens)
  if (explicit === 0 && classified > 0) {
    return classified
  }
  return explicit
}

export function getCacheReadTokens(usage: UsageTokenLike): number {
  return toNonNegativeNumber(usage.cache_read_input_tokens)
}

export function getReasoningOutputTokens(usage: UsageTokenLike): number {
  return firstPositiveNumber([
    usage.reasoning_output_tokens,
    usage.tokens?.reasoning_output,
    usage.tokens?.reasoning_output_tokens,
    usage.reasoning_tokens,
    usage.tokens?.reasoning,
    usage.tokens?.reasoning_tokens,
  ])
}

export function isDowngradedReasoningTokens(value: number | null | undefined): boolean {
  const tokens = toNonNegativeNumber(value)
  return tokens > 0 && (tokens + 2) % 518 === 0
}

export function mergePositiveTokenCount(
  existingValue: number | null | undefined,
  nextValue: number | null | undefined
): number | undefined {
  const existingIsPositive = typeof existingValue === 'number' && Number.isFinite(existingValue) && existingValue > 0
  const nextIsPositive = typeof nextValue === 'number' && Number.isFinite(nextValue) && nextValue > 0

  if (existingIsPositive && nextIsPositive) {
    return Math.max(existingValue, nextValue)
  }
  if (existingIsPositive) {
    return existingValue
  }
  if (nextIsPositive) {
    return nextValue
  }
  return existingValue ?? nextValue ?? undefined
}

export function getEffectiveInputTokens(usage: UsageTokenLike): number {
  const explicit = toNonNegativeNumber(usage.effective_input_tokens)
  if (explicit > 0) {
    return explicit
  }

  const inputTokens = toNonNegativeNumber(usage.input_tokens)
  const cacheReadTokens = toNonNegativeNumber(usage.cache_read_input_tokens)
  if (inputTokens === 0 || cacheReadTokens === 0) {
    return inputTokens
  }

  switch (apiFamily(usage.endpoint_api_format || usage.api_format)) {
    case 'openai':
    case 'gemini':
    case 'google':
      return Math.max(inputTokens - cacheReadTokens, 0)
    default:
      return inputTokens
  }
}
