/* eslint-disable vue/one-component-per-file, vue/require-default-prop */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import EndpointFormDialog from '../EndpointFormDialog.vue'
import type { ProviderEndpoint, ProviderType, ProviderWithEndpointsSummary } from '@/api/endpoints'

const mocks = vi.hoisted(() => ({
  updateEndpoint: vi.fn(),
  endpointUpdated: vi.fn(),
}))

vi.mock('@/api/endpoints', () => ({
  updateEndpoint: mocks.updateEndpoint,
  createEndpoint: vi.fn(),
  deleteEndpoint: vi.fn(),
  getDefaultBodyRules: vi.fn().mockResolvedValue({ body_rules: [] }),
}))
vi.mock('@/api/admin', () => ({
  adminApi: { getApiFormats: vi.fn().mockResolvedValue({ formats: [] }) },
}))
vi.mock('@/stores/proxy-nodes', () => ({
  useProxyNodesStore: () => ({ nodes: [], ensureLoaded: vi.fn() }),
}))
vi.mock('@/components/common/AlertDialog.vue', () => ({ default: { render: () => null } }))
vi.mock('../ProxyNodeSelect.vue', () => ({ default: { render: () => null } }))
vi.mock('../EndpointConditionEditor.vue', () => ({ default: { render: () => null } }))

vi.mock('@/components/ui', async () => {
  const { defineComponent, h } = await import('vue')
  const passthrough = defineComponent({
    setup(_, { slots }) { return () => h('div', slots.default?.()) },
  })
  const hidden = defineComponent({ render: () => null })
  return {
    Dialog: passthrough,
    Button: defineComponent({
      props: { disabled: Boolean },
      setup(props, { slots }) {
        return () => h('button', { disabled: props.disabled }, slots.default?.())
      },
    }),
    Input: defineComponent({
      props: { modelValue: String, disabled: Boolean },
      emits: ['update:modelValue'],
      setup(props, { emit }) {
        return () => h('input', {
          value: props.modelValue,
          disabled: props.disabled,
          onInput: (event: Event) => emit('update:modelValue', (event.target as HTMLInputElement).value),
        })
      },
    }),
    Label: passthrough,
    Badge: passthrough,
    Collapsible: hidden,
    CollapsibleTrigger: hidden,
    CollapsibleContent: hidden,
    Popover: hidden,
    PopoverTrigger: hidden,
    PopoverContent: hidden,
    Select: hidden,
    SelectTrigger: hidden,
    SelectValue: hidden,
    SelectContent: hidden,
    SelectItem: hidden,
    Switch: hidden,
    Textarea: hidden,
  }
})

const officialUrl = 'https://chatgpt.com/backend-api/codex'
let app: App | undefined
let container: HTMLDivElement

async function mountDialog(providerType: ProviderType, baseUrl = officialUrl) {
  container = document.createElement('div')
  document.body.appendChild(container)
  app = createApp(EndpointFormDialog, {
    modelValue: true,
    provider: { id: 'provider-1', name: 'Test', provider_type: providerType } as ProviderWithEndpointsSummary,
    endpoints: [{
      id: 'endpoint-1',
      provider_id: 'provider-1',
      api_format: 'openai:responses',
      base_url: baseUrl,
      is_active: true,
      config: { upstream_stream_policy: 'force_stream' },
    } as ProviderEndpoint],
    onEndpointUpdated: mocks.endpointUpdated,
  })
  app.mount(container)
  await nextTick()
  const input = container.querySelector('input')
  if (!input) throw new Error('Base URL input is missing')
  return input
}

async function editUrl(input: HTMLInputElement, url: string) {
  input.value = url
  input.dispatchEvent(new Event('input', { bubbles: true }))
  await nextTick()
}

beforeEach(() => {
  vi.clearAllMocks()
  mocks.updateEndpoint.mockResolvedValue({})
})

afterEach(() => {
  app?.unmount()
  container?.remove()
})

describe('EndpointFormDialog Base URL', () => {
  it('allows editing and saving the Codex URL without changing its fixed endpoint controls', async () => {
    const input = await mountDialog('codex')
    expect(input.disabled).toBe(false)
    expect(input.value).toBe(officialUrl)
    expect(container.querySelector('button[title="删除"]')).toBeNull()

    await editUrl(input, 'https://proxy.example/custom/codex')
    const saveButton = container.querySelector<HTMLButtonElement>('button[title="保存"]')
    expect(saveButton).not.toBeNull()
    saveButton?.click()
    await nextTick()

    expect(mocks.updateEndpoint).toHaveBeenCalledWith('endpoint-1', {
      base_url: 'https://proxy.example/custom/codex',
    })
    expect(mocks.endpointUpdated).toHaveBeenCalledOnce()
  })

  it('loads a saved custom URL and supports undoing changes', async () => {
    const input = await mountDialog('codex', 'https://proxy.example/custom/codex')
    expect(input.value).toBe('https://proxy.example/custom/codex')
    await editUrl(input, officialUrl)
    const undoButton = container.querySelector<HTMLButtonElement>('button[title="撤销"]')
    expect(undoButton).not.toBeNull()
    undoButton?.click()
    await nextTick()

    expect(input.value).toBe('https://proxy.example/custom/codex')
    expect(container.querySelector('button[title="保存"]')).toBeNull()
    expect(mocks.updateEndpoint).not.toHaveBeenCalled()
  })

  it('keeps custom-provider URLs editable', async () => {
    const input = await mountDialog('custom')
    expect(input.disabled).toBe(false)
  })

  it.each<ProviderType>(['claude_code', 'gemini_cli', 'chatgpt_web', 'kiro'])(
    'keeps %s URLs read-only', async (providerType) => {
      const input = await mountDialog(providerType)
      expect(input.disabled).toBe(true)
    },
  )
})
