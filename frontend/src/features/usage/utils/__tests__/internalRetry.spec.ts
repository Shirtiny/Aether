import { describe, expect, it } from 'vitest'
import type { InternalRetryInfo } from '@/types/internalRetry'
import { internalRetryLabel, internalRetryStopReason } from '../internalRetry'

const recorded: InternalRetryInfo = {
  version: 1, scope: 'aether', retry_count: 2, complete: true, outcome: 'succeeded', attempts: [],
}
describe('internal retry display', () => {
  it('distinguishes missing history from a recorded zero', () => {
    expect(internalRetryLabel(null)).toBe('内部重试未记录')
    expect(internalRetryLabel({ ...recorded, retry_count: 0, outcome: 'not_retried' })).toBe('未发生内部重试')
    expect(internalRetryLabel({ ...recorded, retry_count: 0, complete: false })).toBe('内部重试记录不完整')
  })
  it('uses the final request outcome rather than recovery of the opening', () => {
    expect(internalRetryLabel(recorded)).toBe('内部重试 2 次 · 最终成功')
    expect(internalRetryLabel({ ...recorded, outcome: 'failed' })).toBe('内部重试 2 次 · 最终失败')
    expect(internalRetryLabel({ ...recorded, outcome: 'in_progress' })).toContain('请求进行中')
    expect(internalRetryLabel({ ...recorded, outcome: 'cancelled' })).toContain('请求已取消')
    expect(internalRetryLabel({ ...recorded, complete: false })).toContain('至少 2')
  })
  it('does not claim that unknown or empty events contain visible text', () => {
    expect(internalRetryStopReason('unknown_event')).toContain('未知事件')
    expect(internalRetryStopReason('empty_output_event')).toContain('空输出事件')
    expect(internalRetryStopReason('budget_exhausted')).toContain('耗尽')
    expect(internalRetryStopReason('tool_started')).toContain('工具')
    expect(internalRetryStopReason(null)).toContain('尚未结束')
  })
})
