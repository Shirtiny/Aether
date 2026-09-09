import { afterEach, describe, expect, it } from 'vitest'
import { createApp, type App } from 'vue'
import type { InternalRetryInfo } from '@/types/internalRetry'
import InternalRetryDetails from '../InternalRetryDetails.vue'
const mounted: Array<{ app: App, root: HTMLElement }> = []
function mount(info?: InternalRetryInfo | null) {
  const root = document.createElement('div')
  document.body.append(root)
  const app = createApp(InternalRetryDetails, { info })
  app.mount(root)
  mounted.push({ app, root })
  return root
}
afterEach(() => { mounted.splice(0).forEach(({ app, root }) => { app.unmount(); root.remove() }) })
describe('internal retry detail', () => {
  it('explains missing history instead of pretending no retry', () => {
    const root = mount(null)
    expect(root.textContent).toContain('未记录')
    expect(root.textContent).not.toContain('未发生内部重试')
  })
  it('shows waits, final failure and the actual replay boundary', () => {
    const root = mount({ version: 1, scope: 'aether', retry_count: 1, complete: true, outcome: 'failed',
      attempts: [{ candidate_index: 0, retry_index: 0, retry_count: 1, stop_reason: 'unknown_event',
        stop_event_type: 'response.new_progress', failures: [{ attempt: 1, status_code: 503, reason: 'overloaded',
          planned_wait_ms: 250, wait_ms: 253, retry_started: true }] }] })
    expect(root.textContent).toContain('内部重试 1 次 · 最终失败')
    expect(root.textContent).toContain('HTTP 503')
    expect(root.textContent).toContain('计划等待 250 ms')
    expect(root.textContent).toContain('实际等待 253 ms')
    expect(root.textContent).toContain('未知事件')
    expect(root.textContent).toContain('response.new_progress')
    expect(root.textContent).not.toContain('最终成功')
  })
})
