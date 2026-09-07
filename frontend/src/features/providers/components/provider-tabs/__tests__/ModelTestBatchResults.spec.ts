import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, h, nextTick, reactive, type App } from 'vue'
import type { ModelTestBatchEntry } from '@/composables/useModelTest'
import ModelTestBatchResults from '../ModelTestBatchResults.vue'
import ModelTestResponseBody from '../ModelTestResponseBody.vue'
import ModelTestDialog from '../ModelTestDialog.vue'

const { copy, getConfig } = vi.hoisted(() => ({ copy: vi.fn(), getConfig: vi.fn() }))
vi.mock('@/composables/useClipboard', () => ({ useClipboard: () => ({ copyToClipboard: copy }) }))
vi.mock('@/composables/useToast', () => ({ useToast: () => ({ success: vi.fn(), error: vi.fn() }) }))
vi.mock('@/api/admin', () => ({ adminApi: { getSystemConfig: getConfig } }))

const mounted: Array<{ app: App; root: HTMLElement }> = []
function mount(render: () => ReturnType<typeof h>) {
  const root = document.createElement('div')
  document.body.appendChild(root)
  const app = createApp({ render })
  app.mount(root)
  mounted.push({ app, root })
  return root
}

async function settle() {
  for (let i = 0; i < 5; i++) { await Promise.resolve(); await nextTick() }
}

function button(root: Element, text: string): HTMLButtonElement {
  const result = [...root.querySelectorAll('button')].find(item => item.textContent?.includes(text))
  if (!result) throw new Error(`Missing button: ${text}`)
  return result
}

const fullText = `段落一\n\n  ${'完整响应内容'.repeat(1000)}\n尾部不能丢失`
function entry(index: number): ModelTestBatchEntry {
  return {
    keyId: `key-${index}`, keyName: `账号 ${index}`, requestId: `request-${index}`,
    status: 'success', error: null, errorResponse: null, elapsedMs: 510, statusCode: 200,
    result: {
      success: true, model: 'gpt-5.6-sol', provider: { id: 'provider', name: 'Provider' },
      total_attempts: 1, total_candidates: 1,
      attempts: [{
        candidate_index: 0, key_id: `key-${index}`, key_name: `账号 ${index}`, auth_type: 'oauth',
        endpoint_api_format: 'openai:responses', endpoint_base_url: 'https://test.invalid',
        status: 'success', status_code: 200, latency_ms: 500,
        request_body: { model: 'gpt-5.6-sol', input: [] },
        response_body: { output_text: fullText, usage: { total_tokens: 42 } },
      }],
    },
  }
}

describe('batch test UI and full responses (real UI components)', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    getConfig.mockResolvedValue({ value: { headers: [], body: [] } })
    vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} })
  })
  afterEach(() => {
    for (const { app, root } of mounted.splice(0)) { app.unmount(); root.remove() }
    vi.unstubAllGlobals()
  })

  it('shows and copies the full text or complete JSON without folding or truncation', async () => {
    const body = { output_text: fullText, usage: { total_tokens: 42 } }
    const root = mount(() => h(ModelTestResponseBody, { body }))
    expect(root.querySelector('pre')?.textContent).toBe(fullText)
    expect(root.querySelector('[class*="line-clamp"]')).toBeNull()
    button(root, '复制完整响应').click()
    expect(copy).toHaveBeenLastCalledWith(fullText)
    button(root, '完整响应 JSON').click()
    await nextTick()
    expect(JSON.parse(root.querySelector('pre')?.textContent ?? '')).toEqual(body)
    button(root, '复制完整响应').click()
    expect(copy).toHaveBeenLastCalledWith(JSON.stringify(body, null, 2))
  })

  it('displays HTML errors as inert complete text', () => {
    const body = `<img src="x" onerror="alert(1)">\n${fullText}`
    const root = mount(() => h(ModelTestResponseBody, { body }))
    expect(root.querySelector('pre')?.textContent).toBe(body)
    expect(root.querySelector('img')).toBeNull()
  })

  it('shows all keys including more than twenty, with every full response', () => {
    const entries = Array.from({ length: 25 }, (_, i) => entry(i))
    const root = mount(() => h(ModelTestBatchResults, { entries, testing: false }))
    expect(root.querySelectorAll('section[data-key-id]')).toHaveLength(25)
    for (const section of root.querySelectorAll('section[data-key-id]')) {
      expect(section.querySelector('[data-testid="model-test-full-response"] pre')?.textContent).toBe(fullText)
    }
    expect(root.textContent).toContain('已完成 25/25')
  })

  it('shows intermediate results and a stop control while the other keys run', async () => {
    const onCancelBatch = vi.fn()
    const entries = [entry(0), { ...entry(1), result: null, status: 'running' as const }]
    mount(() => h(ModelTestDialog, { open: true, result: null, batchMode: true, batchResults: entries, testing: true, onCancelBatch }))
    await settle()
    expect(document.body.textContent).toContain(fullText)
    expect(document.body.textContent).toContain('已完成 1/2')
    expect(document.body.textContent).toContain('账号 1')
    button(document.body, '停止批量测试').click()
    expect(onCancelBatch).toHaveBeenCalledTimes(1)
    expect([...document.body.querySelectorAll('button')].some(item => item.textContent?.trim() === '返回')).toBe(false)
  })

  it('supports opt-in batch mode and explicit select-all, disabling an empty or loading batch', async () => {
    const state = reactive({ batchMode: false, selectedKeyIds: [] as string[], keyOptionsLoading: false })
    const onStart = vi.fn()
    mount(() => h(ModelTestDialog, {
      ...state, open: true, result: null, keyOptions: [{ value: 'key-a', label: 'A' }, { value: 'key-b', label: 'B' }],
      requestBodyDraft: '{}', requestHeadersDraft: '{}',
      'onUpdate:batchMode': (value: boolean) => { state.batchMode = value },
      'onUpdate:selectedKeyIds': (value: string[]) => { state.selectedKeyIds = value },
      onStart,
    }))
    await settle()
    button(document.body, '批量测试').click()
    await settle()
    expect(button(document.body, '开始批量测试').disabled).toBe(true)
    button(document.body, '全选 2 个 Key').click()
    await settle()
    expect(state.selectedKeyIds).toEqual(['key-a', 'key-b'])
    expect(button(document.body, '开始批量测试').disabled).toBe(false)
    button(document.body, '开始批量测试').click()
    expect(onStart).toHaveBeenCalledTimes(1)
    state.keyOptionsLoading = true
    await settle()
    expect(button(document.body, '开始批量测试').disabled).toBe(true)
  })

  it('defaults the ordinary test inspector to the complete response rather than request JSON', async () => {
    mount(() => h(ModelTestDialog, { open: true, result: entry(0).result }))
    await settle()
    expect(document.body.querySelector('[data-testid="model-test-full-response"] pre')?.textContent).toBe(fullText)
  })
})
