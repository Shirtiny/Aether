import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectScope, nextTick, ref, type EffectScope } from 'vue'
import type { EndpointAPIKey } from '@/api/endpoints/keys'
import { useModelTestKeys } from '../useModelTestKeys'

const { getKeys } = vi.hoisted(() => ({ getKeys: vi.fn() }))
vi.mock('@/api/endpoints/keys', () => ({ getProviderKeys: getKeys }))
vi.mock('@/composables/useToast', () => ({ useToast: () => ({ error: vi.fn() }) }))

const scopes: EffectScope[] = []
function key(id: string, format = 'openai:responses'): EndpointAPIKey {
  return { id, name: `Account ${id}`, internal_priority: 0, is_active: true, api_formats: [format], auth_type: 'api_key' } as EndpointAPIKey
}
function setup() {
  const providerId = ref('provider-1')
  const endpoint = ref({ id: 'endpoint-1', api_format: 'openai:responses' })
  const scope = effectScope()
  scopes.push(scope)
  const state = scope.run(() => useModelTestKeys({
    providerId: () => providerId.value, providerType: () => 'custom',
    endpoint: () => endpoint.value, fallbackKeys: () => [key('fallback')],
  }))
  if (!state) throw new Error('Missing test scope')
  return { state, providerId, endpoint }
}

describe('model test key selection', () => {
  beforeEach(() => vi.resetAllMocks())
  afterEach(() => scopes.splice(0).forEach(scope => scope.stop()))

  it('loads the full list, filters incompatible/disabled keys and prunes selection after switching endpoints', async () => {
    getKeys.mockResolvedValue([key('b'), key('a'), key('a'), key('chat', 'openai:chat'), { ...key('off'), is_active: false }])
    const { state, endpoint } = setup()
    await state.load()
    expect(state.keyOptions.value.map(item => item.value)).toEqual(['a', 'b'])
    state.select([' a ', 'a', 'b', 'off', 'chat'])
    expect(state.selectedIds.value).toEqual(['a', 'b'])
    endpoint.value = { id: 'chat', api_format: 'openai:chat' }
    await nextTick()
    expect(state.selectedIds.value).toEqual([])
    expect(state.keyOptions.value.map(item => item.value)).toEqual(['chat'])
  })

  it('does not truncate a list containing more than a hundred keys', async () => {
    getKeys.mockResolvedValue(Array.from({ length: 150 }, (_, i) => key(`key-${i}`)))
    const { state } = setup()
    await state.load()
    state.select(state.keyOptions.value.map(item => item.value))
    expect(state.selectedIds.value).toHaveLength(150)
  })

  it('ignores a previous provider fetch arriving after a provider switch', async () => {
    let resolve!: (keys: EndpointAPIKey[]) => void
    getKeys.mockImplementationOnce(() => new Promise<EndpointAPIKey[]>(done => { resolve = done }))
      .mockResolvedValueOnce([key('new')])
    const { state, providerId } = setup()
    const old = state.load()
    providerId.value = 'provider-2'
    await nextTick()
    await state.load()
    resolve([key('old')])
    await old
    expect(state.keyOptions.value.map(item => item.value)).toEqual(['new'])
    expect(state.loading.value).toBe(false)
  })

  it('reports a failed load and permits a retry', async () => {
    getKeys.mockRejectedValueOnce(new Error('offline')).mockResolvedValueOnce([key('retry')])
    const { state } = setup()
    await state.load()
    expect(state.loadError.value).toBe('offline')
    await state.load()
    expect(state.loadError.value).toBe('')
    expect(state.keyOptions.value[0].value).toBe('retry')
  })
})
