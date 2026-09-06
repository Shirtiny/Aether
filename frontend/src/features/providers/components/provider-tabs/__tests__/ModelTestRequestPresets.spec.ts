/* eslint-disable vue/one-component-per-file, vue/require-default-prop */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, h, nextTick, reactive, type App } from 'vue'
import ModelTestRequestPresets from '../ModelTestRequestPresets.vue'
import { MODEL_TEST_TEMPLATES_CONFIG_KEY, type ModelTestRequestTemplates } from '../model-test-templates'

const { getConfig, updateConfig } = vi.hoisted(() => ({ getConfig: vi.fn(), updateConfig: vi.fn() }))
vi.mock('@/api/admin', () => ({ adminApi: { getSystemConfig: getConfig, updateSystemConfig: updateConfig } }))
vi.mock('@/composables/useToast', () => ({ useToast: () => ({ success: vi.fn(), error: vi.fn() }) }))

vi.mock('@/components/ui', async () => {
  const { defineComponent, h, Fragment } = await import('vue')
  const input = (tag: string) => defineComponent({
    inheritAttrs: false,
    props: { modelValue: String, disabled: Boolean },
    emits: ['update:modelValue'],
    setup(props, { attrs, slots, emit }) {
      return () => h(tag, {
        ...attrs,
        value: props.modelValue,
        disabled: props.disabled,
        [tag === 'select' ? 'onChange' : 'onInput']: (event: Event) => {
          emit('update:modelValue', (event.target as HTMLInputElement).value)
        },
      }, slots.default?.())
    },
  })
  return {
    Input: input('input'), Textarea: input('textarea'), Select: input('select'),
    Button: defineComponent({
      props: { disabled: Boolean, variant: String, size: String },
      setup(props, { slots }) {
        return () => h('button', { disabled: props.disabled }, slots.default?.())
      },
    }),
    Dialog: defineComponent({
      props: { open: Boolean },
      setup(props, { slots }) {
        return () => props.open ? h('section', { 'data-manager': '' }, [slots.default?.(), slots.footer?.()]) : null
      },
    }),
    SelectTrigger: defineComponent({ setup: () => () => null }),
    SelectValue: defineComponent({ setup: () => () => null }),
    SelectContent: defineComponent({
      setup: (_, { slots }) => () => h(Fragment, slots.default?.()),
    }),
    SelectItem: defineComponent({
      props: { value: String },
      setup: (props, { slots }) => () => h('option', { value: props.value }, slots.default?.()),
    }),
  }
})

const mounted: Array<{ app: App, root: HTMLElement }> = []
let stored: ModelTestRequestTemplates

async function settle() {
  for (let i = 0; i < 6; i++) {
    await Promise.resolve()
    await nextTick()
  }
}

function mountPresets() {
  const root = document.createElement('div')
  document.body.appendChild(root)
  const onBody = vi.fn()
  const onHeaders = vi.fn()
  const props = reactive({
    apiFormat: 'openai:responses', modelName: 'mapped-model',
    requestHeadersDraft: '{"x-current":"ok"}', requestBodyDraft: '{"model":"mapped-model","input":"current"}',
    requestHeadersResetValue: '{}', requestBodyResetValue: '{"model":"mapped-model","messages":[]}',
    'onUpdate:requestHeadersDraft': onHeaders,
    'onUpdate:requestBodyDraft': onBody,
  })
  // A wrapper keeps props reactive when simulating endpoint/model changes.
  const app = createApp({ setup: () => () => h(ModelTestRequestPresets, props) })
  app.mount(root)
  mounted.push({ app, root })
  return { root, props, onBody, onHeaders }
}

function query<T extends Element>(root: HTMLElement, selector: string): T {
  const element = root.querySelector<T>(selector)
  if (!element) throw new Error(`Element not found: ${selector}`)
  return element
}

function button(root: HTMLElement, text: string): HTMLButtonElement {
  const found = [...root.querySelectorAll('button')].find(item => item.textContent?.includes(text))
  if (!found) throw new Error(`Button not found: ${text}`)
  return found
}

async function select(element: HTMLSelectElement, value: string) {
  element.value = value
  element.dispatchEvent(new Event('change', { bubbles: true }))
  await settle()
}

async function input(root: HTMLElement, selector: string, value: string) {
  const element = query<HTMLInputElement | HTMLTextAreaElement>(root, selector)
  element.value = value
  element.dispatchEvent(new Event('input', { bubbles: true }))
  await settle()
}

describe('ModelTestRequestPresets', () => {
  beforeEach(() => {
    vi.resetAllMocks()
    stored = {
      headers: [{ id: 'header-1', name: '通用请求头', api_format: null, content: { 'x-test': 'yes' } }],
      body: [
        { id: 'body-1', name: 'Responses 测试', api_format: 'openai:responses', content: { model: '{{model}}', input: 'Hello' } },
        { id: 'body-2', name: 'Chat 测试', api_format: 'openai:chat', content: { messages: [] } },
      ],
    }
    getConfig.mockImplementation(async () => ({ value: structuredClone(stored) }))
    updateConfig.mockImplementation(async (_key, value) => {
      stored = structuredClone(value)
      return { value: structuredClone(stored) }
    })
  })

  afterEach(() => {
    for (const { app, root } of mounted.splice(0)) {
      app.unmount()
      root.remove()
    }
  })

  it('applies each preset independently and only on explicit application', async () => {
    const { root, onBody, onHeaders } = mountPresets()
    await settle()
    const selectors = root.querySelectorAll('select')
    expect(selectors[1].textContent).toContain('Responses 测试')
    expect(selectors[1].textContent).not.toContain('Chat 测试')
    await select(selectors[1], 'body-1')
    expect(onBody).not.toHaveBeenCalled()
    query<HTMLButtonElement>(root, '[aria-label="应用请求体预设"]').click()
    expect(JSON.parse(onBody.mock.calls[0][0])).toEqual({ model: 'mapped-model', input: 'Hello' })
    expect(onHeaders).not.toHaveBeenCalled()
    await select(selectors[0], 'header-1')
    query<HTMLButtonElement>(root, '[aria-label="应用请求头预设"]').click()
    expect(JSON.parse(onHeaders.mock.calls[0][0])).toEqual({ 'x-test': 'yes' })
    expect(updateConfig).not.toHaveBeenCalled()
  })

  it('does not overwrite drafts when endpoints change and applies the new endpoint default explicitly', async () => {
    const { root, props, onBody } = mountPresets()
    await settle()
    await select(root.querySelectorAll('select')[1], 'body-1')
    props.apiFormat = 'openai:chat'
    props.requestBodyResetValue = '{"model":"new-model","messages":[{"role":"user","content":"Hi"}]}'
    await settle()
    expect(root.querySelectorAll('select')[1].value).toBe('__default__')
    expect(onBody).not.toHaveBeenCalled()
    query<HTMLButtonElement>(root, '[aria-label="应用请求体预设"]').click()
    expect(onBody).toHaveBeenCalledWith(props.requestBodyResetValue)
  })

  it('saves the current body as a reusable global template and reloads it in another dialog', async () => {
    const { root, onBody } = mountPresets()
    await settle()
    query<HTMLButtonElement>(root, '[aria-label="将当前请求体存为全局模板"]').click()
    await settle()
    expect(query<HTMLTextAreaElement>(root, 'textarea').value).toContain('{{model}}')
    await input(root, '#model-test-template-name', '可复用模板')
    button(root, '保存模板').click()
    await settle()
    expect(updateConfig).toHaveBeenCalledWith(MODEL_TEST_TEMPLATES_CONFIG_KEY, expect.objectContaining({
      body: expect.arrayContaining([expect.objectContaining({
        name: '可复用模板', api_format: 'openai:responses', content: { model: '{{model}}', input: 'current' },
      })]),
    }), expect.any(String))
    expect(onBody).not.toHaveBeenCalled()
    button(root, '关闭').click()
    const other = mountPresets()
    await settle()
    expect(other.root.querySelectorAll('select')[1].textContent).toContain('可复用模板')
  })

  it('edits and renames templates, blocks invalid JSON, and requires deletion confirmation', async () => {
    const { root } = mountPresets()
    await settle()
    button(root, '管理全局模板').click()
    await settle()
    await input(root, '#model-test-template-name', '重命名测试')
    await input(root, '#model-test-template-content', '[]')
    expect(button(root, '保存模板').disabled).toBe(true)
    await input(root, '#model-test-template-content', '{"model":"fixed","input":"edited"}')
    button(root, '保存模板').click()
    await settle()
    expect(stored.body[0]).toMatchObject({ name: '重命名测试', content: { model: 'fixed', input: 'edited' } })
    updateConfig.mockClear()
    button(root, '删除模板').click()
    await settle()
    expect(updateConfig).not.toHaveBeenCalled()
    button(root, '确认删除').click()
    await settle()
    expect(stored.body.map(item => item.id)).toEqual(['body-2'])
    expect(stored.headers).toHaveLength(1)
  })

  it('creates a header template independently, with all endpoint formats as the default scope', async () => {
    const { root } = mountPresets()
    await settle()
    query<HTMLButtonElement>(root, '[aria-label="将当前请求头存为全局模板"]').click()
    await settle()
    await input(root, '#model-test-template-name', '当前请求头')
    button(root, '保存模板').click()
    await settle()
    expect(stored.headers[1]).toMatchObject({
      name: '当前请求头', api_format: null, content: { 'x-current': 'ok' },
    })
    expect(stored.body).toHaveLength(2)
  })

  it('keeps default presets usable on a load failure while disabling global writes', async () => {
    getConfig.mockRejectedValueOnce(new Error('offline'))
    const { root, onHeaders } = mountPresets()
    await settle()
    expect(button(root, '管理全局模板').disabled).toBe(true)
    expect(root.textContent).toContain('offline')
    query<HTMLButtonElement>(root, '[aria-label="应用请求头预设"]').click()
    expect(onHeaders).toHaveBeenCalledWith('{}')
    button(root, '重试').click()
    await settle()
    expect(button(root, '管理全局模板').disabled).toBe(false)
  })
})
