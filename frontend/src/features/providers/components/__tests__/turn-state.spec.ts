/* eslint-disable vue/one-component-per-file */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick, ref, type App, type Component } from 'vue'
import ProviderFormDialog from '../ProviderFormDialog.vue'
import PoolAdvancedDialog from '@/features/pool/components/PoolAdvancedDialog.vue'
import TurnStateCollectionStatusPanel from '../TurnStateCollectionStatusPanel.vue'

const mocks = vi.hoisted(() => ({ updateProvider: vi.fn(), getProvidersSummary: vi.fn(), getProvider: vi.fn(), error: vi.fn() }))
vi.mock('@/api/endpoints', async (importOriginal) => ({
  ...await importOriginal<Record<string, unknown>>(), updateProvider: mocks.updateProvider, getProvidersSummary: mocks.getProvidersSummary, getProvider: mocks.getProvider,
}))
vi.mock('@/composables/useToast', () => ({
  useToast: () => ({ success: vi.fn(), warning: vi.fn(), error: mocks.error }),
}))
vi.mock('@/composables/useConfirm', () => ({ useConfirm: () => ({ confirm: vi.fn() }) }))
vi.mock('@/components/ui', async () => {
  const { defineComponent, h, Fragment } = await import('vue')
  const pass = defineComponent({ setup(_, { slots }) { return () => h('div', [slots.default?.(), slots.footer?.()]) } })
  const hidden = defineComponent({ render: () => null })
  const input = defineComponent({
    props: { modelValue: { type: [String, Number], default: '' } }, emits: ['update:modelValue'],
    setup(props, { emit }) { return () => h('textarea', {
      value: props.modelValue,
      onInput: (event: Event) => emit('update:modelValue', (event.target as HTMLTextAreaElement).value),
    }) },
  })
  return {
    Dialog: pass, Label: pass, Tooltip: hidden, TooltipContent: hidden, TooltipProvider: hidden,
    TooltipTrigger: hidden, SelectTrigger: hidden, SelectValue: hidden,
    Select: defineComponent({
      props: { modelValue: { type: [String, Number], default: '' } }, emits: ['update:modelValue'],
      setup(props, { slots, emit }) { return () => h('select', {
        value: props.modelValue,
        onChange: (event: Event) => emit('update:modelValue', (event.target as HTMLSelectElement).value),
      }, slots.default?.()) },
    }),
    SelectContent: defineComponent({ setup(_, { slots }) { return () => h(Fragment, slots.default?.()) } }),
    SelectItem: defineComponent({
      props: { value: { type: [String, Number], required: true } },
      setup(props, { slots }) { return () => h('option', { value: props.value }, slots.default?.()) },
    }), Input: input, Textarea: input,
    Button: defineComponent({ setup(_, { slots }) { return () => h('button', slots.default?.()) } }),
    Switch: defineComponent({
      props: { modelValue: Boolean }, emits: ['update:modelValue'],
      setup(props, { emit }) { return () => h('input', {
        type: 'checkbox', checked: props.modelValue,
        onChange: (event: Event) => emit('update:modelValue', (event.target as HTMLInputElement).checked),
      }) },
    }),
  }
})

let app: App | undefined
let container: HTMLDivElement
async function mount(component: Component, props: Record<string, unknown>) {
  const open = ref(false)
  container = document.createElement('div')
  document.body.appendChild(container)
  app = createApp(defineComponent({ setup: () => () => h(component, { ...props, modelValue: open.value }) }))
  app.mount(container)
  open.value = true
  await nextTick()
}
function element<T extends Element>(selector: string): T {
  const found = container.querySelector<T>(selector)
  if (!found) throw new Error(`Missing element: ${selector}`)
  return found
}
async function toggle(id: string) {
  const input = element<HTMLInputElement>(`#${id}`)
  input.checked = !input.checked
  input.dispatchEvent(new Event('change', { bubbles: true }))
  await nextTick()
}
async function save() {
  const button = Array.from(container.querySelectorAll('button')).find(button => button.textContent?.trim() === '保存')
  if (!button) throw new Error('Save button is missing')
  button.click()
  await nextTick()
}
beforeEach(() => {
  vi.resetAllMocks()
  mocks.updateProvider.mockResolvedValue({})
  mocks.getProvider.mockResolvedValue({ turn_state_collection_status: { available: true, enabled: false, checked_at: 1789776600, models: [] } })
  mocks.getProvidersSummary.mockResolvedValue({ items: [
    { id: 'source-a', name: '渠道 A', is_active: true, turn_state_collection: { enabled: true, models: ['test-model'] } },
    { id: 'source-b', name: '渠道 B', is_active: true, turn_state_collection: { enabled: true, models: ['test-model'] } },
  ], total: 2 })
})
afterEach(() => { app?.unmount(); container?.remove() })

describe('Turn-State 票据开关', () => {
  it('defaults off, validates models, then submits a trimmed unique list', async () => {
    await mount(ProviderFormDialog, { provider: { id: 'source', name: '聪明', is_active: true } })
    expect(element<HTMLInputElement>('#turn-state-collection-enabled').checked).toBe(false)
    await toggle('turn-state-collection-enabled')
    await save()
    expect(mocks.updateProvider).not.toHaveBeenCalled()
    expect(mocks.error).toHaveBeenCalledWith('请填写 Turn-State 票据采集模型列表')
    const input = element<HTMLTextAreaElement>('#turn-state-collection-models')
    input.value = ' model-a\nmodel-b,model-a '
    input.dispatchEvent(new Event('input', { bubbles: true }))
    await nextTick()
    await save()
    expect(mocks.updateProvider).toHaveBeenCalledWith('source', expect.objectContaining({
      config: expect.objectContaining({ turn_state_collection: { enabled: true, models: ['model-a', 'model-b'] } }),
    }))
  })

  it('loads and disables collection without losing the configured models', async () => {
    await mount(ProviderFormDialog, { provider: {
      id: 'source', name: '聪明', is_active: true,
      turn_state_collection: { enabled: true, models: ['test-model'] },
    } })
    expect(element<HTMLTextAreaElement>('#turn-state-collection-models').value).toBe('test-model')
    await toggle('turn-state-collection-enabled')
    await save()
    expect(mocks.updateProvider).toHaveBeenCalledWith('source', expect.objectContaining({
      config: expect.objectContaining({ turn_state_collection: { enabled: false, models: ['test-model'] } }),
    }))
  })

  it('migrates legacy collection when saving the source channel', async () => {
    await mount(ProviderFormDialog, { provider: {
      id: 'source', name: '旧来源', is_active: true,
      congming_turn_state: { enabled: true, models: ['test-model'] },
    } })
    expect(element<HTMLInputElement>('#turn-state-collection-enabled').checked).toBe(true)
    await save()
    expect(mocks.updateProvider.mock.lastCall?.[1].config).toEqual(expect.objectContaining({
      congming_turn_state: null,
      turn_state_collection: { enabled: true, models: ['test-model'] },
    }))
  })

  it('does not reactivate legacy collection when the generic setting is explicitly null', async () => {
    await mount(ProviderFormDialog, { provider: {
      id: 'source', name: '来源', is_active: true,
      turn_state_collection: null,
      congming_turn_state: { enabled: true, models: ['old-model'] },
    } })
    expect(element<HTMLInputElement>('#turn-state-collection-enabled').checked).toBe(false)
    await save()
    expect(mocks.updateProvider.mock.lastCall?.[1].config.turn_state_collection.enabled).toBe(false)
  })

  it('selects another channel and explicitly disables without losing unrelated settings', async () => {
    await mount(PoolAdvancedDialog, { providerId: 'pool', providerType: 'codex', currentConfig: {
      turn_state_source_provider_id: 'source-a', congming_turn_state_override: true,
      scheduling_presets: [{ preset: 'lru', enabled: true }],
    } })
    await nextTick()
    const select = element<HTMLSelectElement>('[data-testid="turn-state-source-select"]')
    expect(select.value).toBe('source-a')
    select.value = 'source-b'
    select.dispatchEvent(new Event('change', { bubbles: true }))
    await nextTick()
    await save()
    expect(mocks.updateProvider).toHaveBeenLastCalledWith('pool', expect.objectContaining({ pool_advanced: expect.objectContaining({
      turn_state_source_provider_id: 'source-b',
      scheduling_presets: [{ preset: 'lru', enabled: true }],
    }) }))
    expect(mocks.updateProvider.mock.lastCall?.[1].pool_advanced).not.toHaveProperty('congming_turn_state_override')
    select.value = '__disabled__'
    select.dispatchEvent(new Event('change', { bubbles: true }))
    await nextTick()
    await save()
    expect(mocks.updateProvider).toHaveBeenLastCalledWith('pool', expect.objectContaining({ pool_advanced: expect.objectContaining({
      turn_state_source_provider_id: null,
      codex_runtime_identity: expect.objectContaining({ enabled: false }),
    }) }))
  })

  it('defaults legacy override to off rather than guessing a channel', async () => {
    await mount(PoolAdvancedDialog, { providerId: 'pool', providerType: 'codex', currentConfig: { congming_turn_state_override: true } })
    expect(element<HTMLSelectElement>('[data-testid="turn-state-source-select"]').value).toBe('__disabled__')
    expect(container.textContent).toContain('请明确选择来源渠道')
  })

  it('retains a missing source and does not change selection on load failure', async () => {
    mocks.getProvidersSummary.mockRejectedValue(new Error('load failed'))
    await mount(PoolAdvancedDialog, { providerId: 'pool', providerType: 'codex', currentConfig: { turn_state_source_provider_id: 'deleted-source' } })
    await nextTick()
    expect(element<HTMLSelectElement>('[data-testid="turn-state-source-select"]').value).toBe('deleted-source')
    expect(container.textContent).toContain('已选来源保持不变')
    await save()
    expect(mocks.updateProvider.mock.lastCall?.[1].pool_advanced.turn_state_source_provider_id).toBe('deleted-source')
  })

  it('loads all source pages including disabled collection entries', async () => {
    mocks.getProvidersSummary.mockResolvedValueOnce({ items: [{ id: 'first', name: 'First', is_active: true }], total: 2 })
    mocks.getProvidersSummary.mockResolvedValueOnce({ items: [{ id: 'last', name: 'Last', is_active: false }], total: 2 })
    await mount(PoolAdvancedDialog, { providerId: 'pool', providerType: 'codex', currentConfig: {} })
    await nextTick()
    expect(mocks.getProvidersSummary).toHaveBeenCalledTimes(2)
    expect(element<HTMLSelectElement>('[data-testid="turn-state-source-select"]').querySelector('option[value="last"]')).not.toBeNull()
  })

  it('does not show the source selector for other provider types', async () => {
    await mount(PoolAdvancedDialog, { providerId: 'pool', providerType: 'custom', currentConfig: {} })
    expect(container.querySelector('[data-testid="turn-state-source-select"]')).toBeNull()
    expect(mocks.getProvidersSummary).not.toHaveBeenCalled()
  })
})


describe('Turn-State 采集状态', () => {
  const snapshot = {
    available: true, enabled: true, checked_at: 1789776600,
    models: [
      { model: 'model-success', result: 'success', last_attempt_at: 1789776500, last_success_at: 1789776500,
        next_attempt_at: 1789778900, error: null, ticket_valid: true, ticket_length: 384, expires_at: 1789780100 },
      { model: 'model-failed', result: 'failed', last_attempt_at: 1789776500, last_success_at: null,
        next_attempt_at: 1789778900, error: '来源返回 HTTP 200，但未返回 x-codex-turn-state 响应头', ticket_valid: false, ticket_length: null, expires_at: null },
    ],
  }
  it('shows per-model success, failure, expiry and scheduling without collecting', async () => {
    mocks.getProvider.mockResolvedValue({ turn_state_collection_status: snapshot })
    await mount(TurnStateCollectionStatusPanel, { providerId: 'source-a' })
    await nextTick()
    expect(container.textContent).toContain('model-success')
    expect(container.textContent).toContain('最近采集成功')
    expect(container.textContent).toContain('有效票据：384 字符')
    expect(container.textContent).toContain('model-failed')
    expect(container.textContent).toContain('未返回 x-codex-turn-state')
    expect(container.textContent).toContain('无可用票据，不执行覆盖')
    expect(container.textContent).toContain('下次尝试')
    expect(container.textContent).toContain('本地到期')
    expect(mocks.getProvider).toHaveBeenCalledExactlyOnceWith('source-a')
    expect(mocks.updateProvider).not.toHaveBeenCalled()
  })

  it('refreshes status and discards stale success if the refresh fails', async () => {
    mocks.getProvider.mockResolvedValueOnce({ turn_state_collection_status: snapshot })
    await mount(TurnStateCollectionStatusPanel, { providerId: 'source-a' })
    await nextTick()
    mocks.getProvider.mockRejectedValueOnce(new Error('offline'))
    element<HTMLButtonElement>('button').click()
    await nextTick()
    await nextTick()
    expect(container.textContent).toContain('采集状态读取失败')
    expect(container.textContent).not.toContain('最近采集成功')
    expect(mocks.getProvider).toHaveBeenCalledTimes(2)
    expect(mocks.updateProvider).not.toHaveBeenCalled()
  })

  it('does not present an unavailable cache as empty or successful collection', async () => {
    mocks.getProvider.mockResolvedValue({ turn_state_collection_status: { ...snapshot, available: false } })
    await mount(TurnStateCollectionStatusPanel, { providerId: 'source-a' })
    await nextTick()
    expect(container.textContent).toContain('运行时缓存不可用')
    expect(container.textContent).not.toContain('最近采集成功')
  })

  it('shows disabled collection and handles an older server without a status field', async () => {
    await mount(TurnStateCollectionStatusPanel, { providerId: 'source-a' })
    await nextTick()
    expect(container.textContent).toContain('采集未启用')
    expect(container.textContent).toContain('尚未配置采集模型')
    mocks.getProvider.mockResolvedValueOnce({})
    element<HTMLButtonElement>('button').click()
    await nextTick()
    await nextTick()
    expect(container.textContent).toContain('当前服务未返回采集状态')
  })
})
