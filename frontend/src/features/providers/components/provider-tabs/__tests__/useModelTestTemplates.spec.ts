import { beforeEach, describe, expect, it, vi } from 'vitest'
import { AxiosError } from 'axios'
import type { ModelTestRequestTemplate } from '../model-test-templates'

const { getConfig, updateConfig, showError, showSuccess } = vi.hoisted(() => ({
  getConfig: vi.fn(), updateConfig: vi.fn(), showError: vi.fn(), showSuccess: vi.fn(),
}))
vi.mock('@/api/admin', () => ({ adminApi: { getSystemConfig: getConfig, updateSystemConfig: updateConfig } }))
vi.mock('@/composables/useToast', () => ({ useToast: () => ({ success: showSuccess, error: showError }) }))

import { useModelTestTemplates } from '../useModelTestTemplates'
import { MODEL_TEST_TEMPLATES_CONFIG_KEY } from '../model-test-templates'

const template: ModelTestRequestTemplate = {
  id: 'body-1', name: '问答', api_format: 'openai:responses', content: { model: '{{model}}', input: 'Hello' },
}
const initial = { body: [template], headers: [] }

describe('useModelTestTemplates', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    getConfig.mockImplementation(async () => ({ value: structuredClone(initial) }))
    updateConfig.mockImplementation(async (_key, value) => ({ value: structuredClone(value) }))
  })

  it('loads the shared server config and re-fetches for another dialog', async () => {
    const first = useModelTestTemplates()
    const second = useModelTestTemplates()
    await first.load()
    await second.load()
    expect(first.templates.value).toEqual(initial)
    expect(second.templates.value).toEqual(initial)
    expect(first.ready.value).toBe(true)
    expect(getConfig).toHaveBeenCalledTimes(2)
    expect(getConfig).toHaveBeenCalledWith(MODEL_TEST_TEMPLATES_CONFIG_KEY)
  })

  it('merges a create into the latest config without dropping other kinds or newer templates', async () => {
    const state = useModelTestTemplates()
    await state.load()
    const header = { ...template, id: 'header-1', name: '请求头', api_format: null, content: { 'x-test': 'ok' } }
    const remoteTemplate = { ...template, id: 'body-2', name: '其他管理员新增' }
    getConfig.mockResolvedValueOnce({ value: { body: [template, remoteTemplate], headers: [header] } })
    const created = { ...template, id: 'body-3', name: '新模板' }
    expect(await state.save('body', created, true)).toBe(true)
    expect(updateConfig).toHaveBeenCalledWith(MODEL_TEST_TEMPLATES_CONFIG_KEY, {
      body: [template, remoteTemplate, created], headers: [header],
    }, expect.any(String))
    expect(state.templates.value.body).toHaveLength(3)
  })

  it('updates and deletes individual templates', async () => {
    const state = useModelTestTemplates()
    await state.load()
    const edited = { ...template, name: '重命名', content: { input: 'Changed' } }
    expect(await state.save('body', edited, false)).toBe(true)
    expect(state.templates.value.body).toEqual([edited])
    expect(await state.remove('body', template.id)).toBe(true)
    expect(state.templates.value).toEqual({ body: [], headers: [] })
  })

  it('rejects duplicate names and does not recreate a remotely deleted template', async () => {
    const state = useModelTestTemplates()
    await state.load()
    expect(await state.save('body', { ...template, id: 'another' }, true)).toBe(false)
    getConfig.mockResolvedValueOnce({ value: { body: [], headers: [] } })
    expect(await state.save('body', template, false)).toBe(false)
    expect(updateConfig).not.toHaveBeenCalled()
    expect(showError).toHaveBeenCalledWith('此模板已被删除，请刷新后重试')
  })

  it('does not allow writes after a failed or malformed initial read, and can retry', async () => {
    const state = useModelTestTemplates()
    getConfig.mockRejectedValueOnce(new Error('offline'))
    await state.load()
    expect(state.ready.value).toBe(false)
    expect(state.loadError.value).toBeTruthy()
    expect(await state.save('body', template, true)).toBe(false)
    expect(updateConfig).not.toHaveBeenCalled()
    getConfig.mockResolvedValueOnce({ value: { body: 'broken', headers: [] } })
    await state.load()
    expect(state.ready.value).toBe(false)
    await state.load()
    expect(state.ready.value).toBe(true)
    expect(state.loadError.value).toBe('')
  })

  it('preserves the visible saved list when a write fails', async () => {
    const state = useModelTestTemplates()
    await state.load()
    updateConfig.mockRejectedValueOnce(new Error('write failed'))
    expect(await state.remove('body', template.id)).toBe(false)
    expect(state.templates.value).toEqual(initial)
    expect(state.saving.value).toBe(false)
    expect(showSuccess).not.toHaveBeenCalled()
  })

  it('uses an empty list for a missing config only, not authorization failures', async () => {
    const state = useModelTestTemplates()
    const missing = new AxiosError('not found')
    Object.assign(missing, { response: { status: 404 } })
    getConfig.mockRejectedValueOnce(missing)
    await state.load()
    expect(state.templates.value).toEqual({ headers: [], body: [] })
    expect(state.ready.value).toBe(true)
    Object.assign(missing, { response: { status: 403 } })
    getConfig.mockRejectedValueOnce(missing)
    await state.load()
    expect(state.ready.value).toBe(false)
  })
})
