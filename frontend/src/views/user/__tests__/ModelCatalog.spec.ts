import { afterEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'

import type { PublicGlobalModel } from '@/api/public-models'
import UserModelDetailDrawer from '../components/UserModelDetailDrawer.vue'

vi.mock('@/composables/useClipboard', () => ({
  useClipboard: () => ({
    copyToClipboard: vi.fn(),
  }),
}))

const mountedApps: Array<{ app: App, root: HTMLElement }> = []

function model(overrides: Partial<PublicGlobalModel> = {}): PublicGlobalModel {
  return {
    id: 'gm-test',
    name: 'gpt-5',
    display_name: 'GPT 5',
    is_active: true,
    default_tiered_pricing: null,
    default_price_per_request: null,
    supported_capabilities: ['chat'],
    config: null,
    usage_count: 0,
    ...overrides,
  }
}

function mountDrawer(selectedModel: PublicGlobalModel) {
  const root = document.createElement('div')
  document.body.appendChild(root)
  const app = createApp(UserModelDetailDrawer, {
    open: true,
    model: selectedModel,
    'onUpdate:open': vi.fn(),
  })
  app.mount(root)
  mountedApps.push({ app, root })
  return root
}

afterEach(() => {
  for (const { app, root } of mountedApps.splice(0)) {
    app.unmount()
    root.remove()
  }
  document.body.innerHTML = ''
})

describe('user model catalog detail drawer', () => {
  it('does not render model mapping fields for ordinary users', async () => {
    mountDrawer(model({
      config: {
        description: 'User visible description',
        model_mappings: ['gpt-5-upstream'],
        provider_model_mappings: [{ name: 'provider-gpt-5' }],
      },
    }))
    await nextTick()

    const text = document.body.textContent || ''
    expect(text).toContain('GPT 5')
    expect(text).toContain('User visible description')
    expect(text).not.toContain('模型映射')
    expect(text).not.toContain('gpt-5-upstream')
    expect(text).not.toContain('provider-gpt-5')
  })
})
