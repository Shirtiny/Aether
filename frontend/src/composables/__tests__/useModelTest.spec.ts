import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, ref, type App } from 'vue'
import { AxiosError } from 'axios'
import type { TestModelRequest, TestModelResponse } from '@/api/endpoints/providers'
import { useModelTest, type StartTestParams } from '../useModelTest'

const { direct, failover, trace, showError } = vi.hoisted(() => ({
  direct: vi.fn(), failover: vi.fn(), trace: vi.fn(), showError: vi.fn(),
}))
vi.mock('@/api/endpoints/providers', () => ({ testModel: direct, testModelFailover: failover }))
vi.mock('@/api/requestTrace', () => ({ requestTraceApi: { getRequestTrace: trace } }))
vi.mock('@/composables/useToast', () => ({ useToast: () => ({ success: vi.fn(), error: showError }) }))

const mounted: Array<{ app: App; root: HTMLElement }> = []
function mountTest() {
  const provider = ref('provider-1')
  let state!: ReturnType<typeof useModelTest>
  const app = createApp({ setup() {
    state = useModelTest({ providerId: () => provider.value })
    return () => null
  } })
  const root = document.createElement('div')
  app.mount(root)
  mounted.push({ app, root })
  return { state, provider }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: unknown) => void
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej })
  return { promise, resolve, reject }
}

async function settle() {
  for (let i = 0; i < 8; i++) { await Promise.resolve(); await nextTick() }
}

function params(count = 6): StartTestParams {
  return {
    mode: 'pool', modelName: 'gpt-5.6-sol', displayLabel: 'Sol',
    endpointId: 'endpoint-1', apiFormat: 'openai:responses', endpointBaseUrl: 'https://mock.invalid',
    batchKeys: Array.from({ length: count }, (_, i) => ({ id: `key-${i}`, name: `Account ${i}` })),
    requestHeaders: { 'x-test': 'batch' },
    requestBody: { model: 'gpt-5.6-sol', input: [{ role: 'user', content: 'hello' }], reasoning: { effort: 'low' } },
    applyModelMapping: true, mappedModelName: 'mapped-sol',
  }
}

function response(key: string, status: 'success' | 'failed' | 'skipped' = 'success'): TestModelResponse {
  return {
    success: status === 'success',
    model: 'gpt-5.6-sol',
    attempts: [{
      candidate_index: 0, endpoint_api_format: 'openai:responses', endpoint_base_url: 'https://mock.invalid',
      key_name: key, key_id: key, auth_type: 'oauth', status,
      status_code: status === 'success' ? 200 : status === 'failed' ? 400 : null,
      response_body: status === 'success' ? { output_text: `${key}\n${'长响应'.repeat(2000)}\n完整结尾` } : { detail: 'bad input' },
    }],
  }
}

describe('useModelTest batch mode', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    trace.mockResolvedValue(null)
  })
  afterEach(() => {
    for (const { app, root } of mounted.splice(0)) { app.unmount(); root.remove() }
  })

  it('tests every key once with at most three concurrent requests, even after success', async () => {
    const pending = new Map<string, ReturnType<typeof deferred<TestModelResponse>>>()
    let active = 0
    let peak = 0
    direct.mockImplementation((request: TestModelRequest) => {
      const item = deferred<TestModelResponse>()
      pending.set(request.api_key_ids?.[0] ?? '', item)
      peak = Math.max(peak, ++active)
      return item.promise.finally(() => { active -= 1 })
    })
    const { state } = mountTest()
    const input = params()
    const originalBody = structuredClone(input.requestBody)
    const running = state.startTest(input)
    expect(direct).toHaveBeenCalledTimes(3)
    if (input.requestBody) input.requestBody.reasoning = { effort: 'high' }
    expect(state.batchResults.value.map(entry => entry.status)).toEqual(['running', 'running', 'running', 'pending', 'pending', 'pending'])
    for (let i = 0; i < 6; i++) {
      expect(pending.has(`key-${i}`)).toBe(true)
      pending.get(`key-${i}`)?.resolve(response(`key-${i}`))
      await settle()
    }
    await running
    expect(peak).toBe(3)
    expect(direct).toHaveBeenCalledTimes(6)
    expect(failover).not.toHaveBeenCalled()
    expect(trace).not.toHaveBeenCalled()
    const requestIds = new Set<string>()
    direct.mock.calls.forEach(([request], i) => {
      expect(request).toMatchObject({
        provider_id: 'provider-1', endpoint_id: 'endpoint-1', mode: 'direct',
        api_key_ids: [`key-${i}`], request_headers: { 'x-test': 'batch' }, request_body: originalBody,
        apply_model_mapping: true, mapped_model_name: 'mapped-sol',
      })
      requestIds.add(request.request_id)
    })
    expect(requestIds.size).toBe(6)
    expect(state.batchResults.value.every(entry => entry.status === 'success')).toBe(true)
    expect(state.batchResults.value[5].result?.attempts[0].response_body).toEqual(response('key-5').attempts?.[0].response_body)
    expect(state.testing.value).toBe(false)
  })

  it('keeps per-key API failures, HTTP response bodies and skipped results without stopping the batch', async () => {
    const errorBody = { detail: 'upstream failed', extra: '完整错误'.repeat(400) }
    const error = new AxiosError('HTTP 502')
    Object.assign(error, { response: { status: 502, data: errorBody } })
    direct.mockResolvedValueOnce(response('key-0', 'failed'))
      .mockRejectedValueOnce(error)
      .mockResolvedValueOnce(response('key-2', 'skipped'))
      .mockResolvedValueOnce(response('key-3'))
    const { state } = mountTest()
    await state.startTest(params(4))
    expect(state.batchResults.value.map(entry => entry.status)).toEqual(['failed', 'failed', 'skipped', 'success'])
    expect(state.batchResults.value[1]).toMatchObject({ statusCode: 502, errorResponse: errorBody })
    expect(state.batchResults.value[0].result?.attempts[0].response_body).toEqual({ detail: 'bad input' })
    expect(state.dialogOpen.value).toBe(true)
  })

  it('cancels queued and in-flight work while retaining completed responses', async () => {
    const pending = Array.from({ length: 6 }, () => deferred<TestModelResponse>())
    direct.mockImplementation((request: TestModelRequest) => pending[Number(request.api_key_ids?.[0].split('-')[1])].promise)
    const { state } = mountTest()
    const running = state.startTest(params())
    pending[0].resolve(response('key-0'))
    await settle()
    expect(direct).toHaveBeenCalledTimes(4)
    state.cancelBatch()
    expect(state.testing.value).toBe(false)
    expect(state.batchResults.value.map(entry => entry.status)).toEqual(['success', 'cancelled', 'cancelled', 'cancelled', 'cancelled', 'cancelled'])
    expect(direct.mock.calls.every(([, options]) => options.signal.aborted)).toBe(true)
    pending.forEach((item, i) => item.resolve(response(`key-${i}`)))
    await running
    expect(direct).toHaveBeenCalledTimes(4)
    expect(state.batchResults.value[1].result).toBeNull()
    expect(state.batchResults.value[0].result?.success).toBe(true)
    state.backToSetup()
    expect(state.batchResults.value).toEqual([])
  })

  it('does not let a closed old batch overwrite or stop a newer test', async () => {
    const old = deferred<TestModelResponse>()
    const current = deferred<TestModelResponse>()
    direct.mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise)
    const { state } = mountTest()
    const first = state.startTest(params(1))
    state.resetState()
    const second = state.startTest(params(1))
    old.resolve(response('old'))
    await first
    expect(state.testing.value).toBe(true)
    expect(state.batchResults.value[0].result).toBeNull()
    current.resolve(response('current'))
    await second
    expect(state.batchResults.value[0].result?.attempts[0].key_id).toBe('current')
  })

  it('aborts a batch when the provider changes', async () => {
    const pending = deferred<TestModelResponse>()
    direct.mockReturnValue(pending.promise)
    const { state, provider } = mountTest()
    const running = state.startTest(params(4))
    provider.value = 'provider-2'
    await nextTick()
    expect(state.dialogOpen.value).toBe(false)
    expect(direct.mock.calls[0][1].signal.aborted).toBe(true)
    pending.resolve(response('key-0'))
    await running
    expect(direct).toHaveBeenCalledTimes(3)
    expect(state.batchResults.value).toEqual([])
  })

  it('requires explicit keys and endpoint, and deduplicates selections', async () => {
    const { state } = mountTest()
    await state.startTest(params(0))
    await state.startTest({ ...params(1), endpointId: undefined })
    expect(direct).not.toHaveBeenCalled()
    expect(showError).toHaveBeenCalledTimes(2)
    direct.mockResolvedValue(response('key-0'))
    await state.startTest({ ...params(1), batchKeys: [{ id: 'key-0', name: 'A' }, { id: ' key-0 ', name: 'B' }] })
    expect(direct).toHaveBeenCalledTimes(1)
  })

  it('keeps ordinary pool tests on the existing failover route', async () => {
    failover.mockResolvedValue({ ...response('key-0'), total_candidates: 2, total_attempts: 1 })
    const { state } = mountTest()
    await state.startTest({ ...params(), batchKeys: undefined, apiKeyIds: ['key-0', 'key-1'] })
    expect(direct).not.toHaveBeenCalled()
    expect(failover.mock.calls[0][0]).toMatchObject({ mode: 'pool', api_key_ids: ['key-0', 'key-1'] })
    expect(state.testResult.value?.success).toBe(true)
    expect(state.batchResults.value).toEqual([])
  })

  it('keeps ordinary direct tests and their full attempt data working', async () => {
    direct.mockResolvedValue(response('key-0'))
    const { state } = mountTest()
    await state.startTest({ ...params(1), batchKeys: undefined, mode: 'direct', apiKeyIds: ['key-0'] })
    expect(state.testResult.value?.attempts[0].response_body).toEqual(response('key-0').attempts?.[0].response_body)
    expect(state.testing.value).toBe(false)
  })
})
