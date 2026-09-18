/* eslint-disable vue/one-component-per-file */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick, ref, type App, type Component } from 'vue'
import ProviderFormDialog from '../ProviderFormDialog.vue'
import PoolAdvancedDialog from '@/features/pool/components/PoolAdvancedDialog.vue'

const mocks = vi.hoisted(() => ({ updateProvider: vi.fn(), error: vi.fn() }))
vi.mock('@/api/endpoints', async (importOriginal) => ({
  ...await importOriginal<Record<string, unknown>>(), updateProvider: mocks.updateProvider,
}))
vi.mock('@/composables/useToast', () => ({
  useToast: () => ({ success: vi.fn(), warning: vi.fn(), error: mocks.error }),
}))
vi.mock('@/composables/useConfirm', () => ({ useConfirm: () => ({ confirm: vi.fn() }) }))
vi.mock('@/components/ui', async () => {
  const { defineComponent, h } = await import('vue')
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
    TooltipTrigger: hidden, Select: hidden, SelectTrigger: hidden, SelectValue: hidden,
    SelectContent: hidden, SelectItem: hidden, Input: input, Textarea: input,
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
beforeEach(() => { vi.clearAllMocks(); mocks.updateProvider.mockResolvedValue({}) })
afterEach(() => { app?.unmount(); container?.remove() })

describe('聪明票据开关', () => {
  it('defaults off, validates models, then submits a trimmed unique list', async () => {
    await mount(ProviderFormDialog, { provider: { id: 'source', name: '聪明', is_active: true } })
    expect(element<HTMLInputElement>('#congming-turn-state-enabled').checked).toBe(false)
    await toggle('congming-turn-state-enabled')
    await save()
    expect(mocks.updateProvider).not.toHaveBeenCalled()
    expect(mocks.error).toHaveBeenCalledWith('请填写聪明票据采集模型列表')
    const input = element<HTMLTextAreaElement>('#congming-turn-state-models')
    input.value = ' model-a\nmodel-b,model-a '
    input.dispatchEvent(new Event('input', { bubbles: true }))
    await nextTick()
    await save()
    expect(mocks.updateProvider).toHaveBeenCalledWith('source', expect.objectContaining({
      config: expect.objectContaining({ congming_turn_state: { enabled: true, models: ['model-a', 'model-b'] } }),
    }))
  })

  it('loads and disables collection without losing the configured models', async () => {
    await mount(ProviderFormDialog, { provider: {
      id: 'source', name: '聪明', is_active: true,
      congming_turn_state: { enabled: true, models: ['test-model'] },
    } })
    expect(element<HTMLTextAreaElement>('#congming-turn-state-models').value).toBe('test-model')
    await toggle('congming-turn-state-enabled')
    await save()
    expect(mocks.updateProvider).toHaveBeenCalledWith('source', expect.objectContaining({
      config: expect.objectContaining({ congming_turn_state: { enabled: false, models: ['test-model'] } }),
    }))
  })

  it('persists the pool override independently and preserves unrelated settings', async () => {
    await mount(PoolAdvancedDialog, { providerId: 'pool', providerType: 'codex', currentConfig: {
      congming_turn_state_override: true, scheduling_presets: [{ preset: 'lru', enabled: true }],
    } })
    expect(element<HTMLInputElement>('#congming-turn-state-override').checked).toBe(true)
    await toggle('congming-turn-state-override')
    await save()
    expect(mocks.updateProvider).toHaveBeenCalledWith('pool', expect.objectContaining({ pool_advanced: expect.objectContaining({
      congming_turn_state_override: false,
      codex_runtime_identity: expect.objectContaining({ enabled: false }),
      scheduling_presets: [{ preset: 'lru', enabled: true }],
    }) }))
  })

  it('does not show the pool switch for other provider types', async () => {
    await mount(PoolAdvancedDialog, { providerId: 'pool', providerType: 'custom', currentConfig: {} })
    expect(container.querySelector('#congming-turn-state-override')).toBeNull()
  })
})
