import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import ElapsedTimeText from '../ElapsedTimeText.vue'

describe('ElapsedTimeText', () => {
  let app: App<Element> | null = null
  let container: HTMLDivElement

  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-08-01T08:00:00.000Z'))
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => (
      window.setTimeout(() => callback(performance.now()), 16)
    ))
    vi.stubGlobal('cancelAnimationFrame', (id: number) => window.clearTimeout(id))
    container = document.createElement('div')
    document.body.appendChild(container)
  })

  afterEach(() => {
    app?.unmount()
    app = null
    container.remove()
    vi.unstubAllGlobals()
    vi.useRealTimers()
  })

  async function mount(props: Record<string, unknown>) {
    app = createApp(ElapsedTimeText, props)
    app.mount(container)
    await nextTick()
    return container.querySelector('span') as HTMLSpanElement
  }

  it('shows live elapsed time while an active request has no explicit terminal signal', async () => {
    const root = await mount({
      createdAt: '2026-08-01T07:53:20.000Z',
      status: 'streaming',
      responseTimeMs: 3_000,
    })

    expect(root.textContent).toBe('400.00s')
    expect(root.dataset.terminalSyncDelayed).toBeUndefined()
  })

  it('shows terminal transport latency when the backend reports delayed terminal sync', async () => {
    const root = await mount({
      createdAt: '2026-08-01T07:53:20.000Z',
      status: 'streaming',
      responseTimeMs: 3_000,
      terminalSyncPending: true,
      terminalResponseTimeMs: 5_037,
    })

    expect(root.textContent).toBe('5.04s · 终态同步中')
    expect(root.dataset.terminalSyncDelayed).toBe('true')
    expect(root.title).toContain('终态同步延迟')
  })

  it('shows an explicit sync state even when terminal latency is unavailable', async () => {
    const root = await mount({
      status: 'streaming',
      terminalSyncPending: true,
    })

    expect(root.textContent).toBe('终态同步中')
    expect(root.dataset.terminalSyncDelayed).toBe('true')
  })

  it('keeps the authoritative recorded duration after completion', async () => {
    const root = await mount({
      createdAt: '2026-08-01T07:53:20.000Z',
      status: 'completed',
      responseTimeMs: 5_037,
    })

    expect(root.textContent).toBe('5.04s')
    expect(root.dataset.terminalSyncDelayed).toBeUndefined()
  })
})
