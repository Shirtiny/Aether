import { describe, expect, it } from 'vitest'

import {
  getCacheCreationTokens,
  getCacheReadTokens,
  getEffectiveInputTokens,
  getReasoningOutputTokens,
  isDowngradedReasoningTokens,
  mergePositiveTokenCount,
} from '../token-normalization'

describe('usage token normalization', () => {
  it('prefers explicit cache creation totals when present', () => {
    expect(getCacheCreationTokens({
      cache_creation_input_tokens: 18,
      cache_creation_ephemeral_5m_input_tokens: 5,
      cache_creation_ephemeral_1h_input_tokens: 7,
    })).toBe(18)
  })

  it('rehydrates cache creation totals from classified fields when legacy total is zero', () => {
    expect(getCacheCreationTokens({
      cache_creation_input_tokens: 0,
      cache_creation_ephemeral_5m_input_tokens: 12,
      cache_creation_ephemeral_1h_input_tokens: 8,
    })).toBe(20)
  })

  it('keeps cache read and cache creation as separate display values', () => {
    const usage = {
      input_tokens: 1,
      cache_creation_input_tokens: 1530,
      cache_read_input_tokens: 104026,
      output_tokens: 591,
      api_format: 'claude:messages',
    }

    expect(getEffectiveInputTokens(usage)).toBe(1)
    expect(getCacheReadTokens(usage)).toBe(104026)
    expect(getCacheCreationTokens(usage)).toBe(1530)
  })

  it('keeps effective input token normalization unchanged', () => {
    expect(getEffectiveInputTokens({
      input_tokens: 100,
      cache_read_input_tokens: 20,
      api_format: 'openai:chat',
    })).toBe(80)
  })

  it('does not subtract cache read tokens for Claude usage', () => {
    expect(getEffectiveInputTokens({
      input_tokens: 4941,
      cache_creation_input_tokens: 687,
      cache_read_input_tokens: 52873,
      output_tokens: 973,
      api_format: 'claude:messages',
    })).toBe(4941)
  })

  it('reads reasoning output tokens and detects downgrade pattern', () => {
    expect(getReasoningOutputTokens({ reasoning_output_tokens: 516 })).toBe(516)
    expect(getReasoningOutputTokens({ tokens: { reasoning_output: 1034 } })).toBe(1034)
    expect(isDowngradedReasoningTokens(516)).toBe(true)
    expect(isDowngradedReasoningTokens(517)).toBe(false)
  })

  it('prefers explicit reasoning output aliases over legacy reasoning tokens', () => {
    expect(getReasoningOutputTokens({
      reasoning_tokens: 517,
      tokens: { reasoning_output: 516 },
    })).toBe(516)
  })

  it('keeps positive reasoning token counts across stale refresh payloads', () => {
    expect(mergePositiveTokenCount(516, undefined)).toBe(516)
    expect(mergePositiveTokenCount(516, 0)).toBe(516)
    expect(mergePositiveTokenCount(516, 1034)).toBe(1034)
    expect(mergePositiveTokenCount(undefined, 516)).toBe(516)
  })
})
