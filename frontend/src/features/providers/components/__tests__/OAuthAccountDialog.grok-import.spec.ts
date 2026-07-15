/* eslint-disable vue/one-component-per-file, vue/require-default-prop */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import OAuthAccountDialog from '@/features/providers/components/OAuthAccountDialog.vue'

const endpointMocks = vi.hoisted(() => ({
  startProviderLevelOAuth: vi.fn(),
  completeProviderLevelOAuth: vi.fn(),
  importProviderRefreshToken: vi.fn(),
  startBatchImportOAuthTask: vi.fn(),
  getBatchImportOAuthTaskStatus: vi.fn(),
  startDeviceAuthorize: vi.fn(),
  pollDeviceAuthorize: vi.fn(),
  getAwsRegions: vi.fn(),
}))

vi.mock('@/api/endpoints', async () => {
  const actual = await vi.importActual<typeof import('@/api/endpoints/provider_oauth')>(
    '@/api/endpoints/provider_oauth',
  )

  return {
    ...endpointMocks,
    normalizeBatchImportCredentials: actual.normalizeBatchImportCredentials,
  }
})

vi.mock('@/components/ui', async () => {
  const { defineComponent, h } = await import('vue')

  const passthrough = (name: string, tag = 'div') => defineComponent({
    name,
    setup(_, { slots }) {
      return () => h(tag, slots.default?.())
    },
  })

  const Dialog = defineComponent({
    name: 'DialogStub',
    props: {
      modelValue: Boolean,
    },
    setup(props, { slots }) {
      return () => props.modelValue
        ? h('section', [slots.headerActions?.(), slots.default?.(), slots.footer?.()])
        : null
    },
  })

  const Button = defineComponent({
    name: 'ButtonStub',
    inheritAttrs: false,
    props: {
      disabled: Boolean,
      variant: String,
      size: String,
    },
    setup(props, { attrs, slots }) {
      return () => h('button', {
        ...attrs,
        disabled: props.disabled,
        type: attrs.type ?? 'button',
      }, slots.default?.())
    },
  })

  const Textarea = defineComponent({
    name: 'TextareaStub',
    inheritAttrs: false,
    props: {
      modelValue: {
        type: String,
        default: '',
      },
    },
    emits: ['update:modelValue'],
    setup(props, { attrs, emit }) {
      return () => h('textarea', {
        ...attrs,
        value: props.modelValue,
        onInput: (event: Event) => emit('update:modelValue', (event.target as HTMLTextAreaElement).value),
      })
    },
  })

  return {
    Dialog,
    Button,
    Textarea,
    Popover: passthrough('PopoverStub'),
    PopoverTrigger: passthrough('PopoverTriggerStub'),
    PopoverContent: passthrough('PopoverContentStub'),
  }
})

vi.mock('radix-vue', async () => {
  const { defineComponent, h } = await import('vue')
  const passthrough = (name: string) => defineComponent({
    name,
    setup(_, { slots }) {
      return () => h('div', slots.default?.())
    },
  })

  return {
    ComboboxAnchor: passthrough('ComboboxAnchorStub'),
    ComboboxContent: passthrough('ComboboxContentStub'),
    ComboboxEmpty: passthrough('ComboboxEmptyStub'),
    ComboboxInput: passthrough('ComboboxInputStub'),
    ComboboxItem: passthrough('ComboboxItemStub'),
    ComboboxRoot: passthrough('ComboboxRootStub'),
    ComboboxTrigger: passthrough('ComboboxTriggerStub'),
    ComboboxViewport: passthrough('ComboboxViewportStub'),
  }
})

vi.mock('@/components/common/JsonImportInput.vue', async () => {
  const { defineComponent, h } = await import('vue')

  return {
    default: defineComponent({
      name: 'JsonImportInputStub',
      props: {
        modelValue: {
          type: String,
          default: '',
        },
        dropTitle: {
          type: String,
          default: '',
        },
        dropHint: {
          type: String,
          default: '',
        },
        manualPlaceholder: {
          type: String,
          default: '',
        },
        manualDescription: {
          type: String,
          default: '',
        },
        pasteToggleText: {
          type: String,
          default: '',
        },
        fileToggleText: {
          type: String,
          default: '',
        },
      },
      emits: ['update:modelValue'],
      setup(props, { emit }) {
        return () => h('div', [
          h('p', { 'data-testid': 'drop-title' }, props.dropTitle),
          h('p', { 'data-testid': 'drop-hint' }, props.dropHint),
          h('p', { 'data-testid': 'manual-description' }, props.manualDescription),
          h('p', props.pasteToggleText),
          h('p', props.fileToggleText),
          h('textarea', {
            placeholder: props.manualPlaceholder,
            value: props.modelValue,
            onInput: (event: Event) => emit('update:modelValue', (event.target as HTMLTextAreaElement).value),
          }),
        ])
      },
    }),
  }
})

vi.mock('@/components/ui/Label.vue', () => ({}))
vi.mock('./ProxyNodeSelect.vue', () => ({}))
vi.mock('@/features/providers/components/ProxyNodeSelect.vue', async () => {
  const { defineComponent, h } = await import('vue')
  return {
    default: defineComponent({
      name: 'ProxyNodeSelectStub',
      setup() {
        return () => h('div')
      },
    }),
  }
})

vi.mock('@/stores/proxy-nodes', () => ({
  useProxyNodesStore: () => ({
    nodes: [],
    onlineNodes: [],
    loading: false,
    ensureLoaded: vi.fn(),
  }),
}))

vi.mock('@/composables/useToast', () => ({
  useToast: () => ({
    success: vi.fn(),
    error: vi.fn(),
  }),
}))

vi.mock('@/composables/useClipboard', () => ({
  useClipboard: () => ({
    copyToClipboard: vi.fn(),
  }),
}))

vi.mock('@/composables/useTotp', () => ({
  useTotp: () => ({
    code: { value: '' },
    remaining: { value: 0 },
    start: vi.fn(),
    stop: vi.fn(),
  }),
}))

vi.mock('lucide-vue-next', async () => {
  const { defineComponent, h } = await import('vue')
  const Icon = defineComponent({
    name: 'IconStub',
    setup() {
      return () => h('span')
    },
  })

  return {
    UserPlus: Icon,
    Copy: Icon,
    ExternalLink: Icon,
    Globe: Icon,
    AlertCircle: Icon,
    ShieldCheck: Icon,
    ChevronsUpDown: Icon,
    Check: Icon,
  }
})

const mountedApps: Array<{ app: App, root: HTMLElement }> = []

function mountDialog(providerType = 'grok') {
  const root = document.createElement('div')
  document.body.appendChild(root)
  const app = createApp(OAuthAccountDialog, {
    open: true,
    providerId: 'provider-1',
    providerType,
  })
  app.mount(root)
  mountedApps.push({ app, root })
  return root
}

async function settle() {
  await nextTick()
  await Promise.resolve()
}

function getButton(root: HTMLElement, text: string) {
  return Array.from(root.querySelectorAll('button'))
    .find(button => button.textContent?.includes(text))
}

function getImportTextarea(root: HTMLElement) {
  const textarea = root.querySelector('textarea[placeholder*="xAI OAuth"]')
  if (!(textarea instanceof HTMLTextAreaElement)) {
    throw new Error('Expected import textarea to exist')
  }
  return textarea
}

describe('OAuthAccountDialog Grok xAI OAuth', () => {
  beforeEach(() => {
    endpointMocks.startProviderLevelOAuth.mockReset()
    endpointMocks.completeProviderLevelOAuth.mockReset()
    endpointMocks.importProviderRefreshToken.mockReset()
    endpointMocks.startBatchImportOAuthTask.mockReset()
    endpointMocks.getBatchImportOAuthTaskStatus.mockReset()
    endpointMocks.startDeviceAuthorize.mockReset()
    endpointMocks.pollDeviceAuthorize.mockReset()
    endpointMocks.getAwsRegions.mockReset()

    endpointMocks.startProviderLevelOAuth.mockResolvedValue({
      authorization_url: 'https://auth.x.ai/oauth2/authorize?client_id=xai-client',
      redirect_uri: 'http://127.0.0.1:56121/callback',
      instructions: 'Complete xAI OAuth authorization',
      provider_type: 'grok',
    })
    endpointMocks.completeProviderLevelOAuth.mockResolvedValue({
      provider_type: 'grok',
      has_refresh_token: true,
      email: 'grok@example.com',
      replaced: false,
    })

    endpointMocks.importProviderRefreshToken.mockResolvedValue({
      provider_type: 'grok',
      has_refresh_token: false,
      email: 'grok@example.com',
      replaced: false,
    })
    endpointMocks.startBatchImportOAuthTask.mockResolvedValue({
      task_id: 'task-1',
      status: 'submitted',
      total: 2,
      processed: 0,
      success: 0,
      failed: 0,
      progress_percent: 0,
    })
  })

  afterEach(() => {
    vi.useRealTimers()
    for (const { app, root } of mountedApps.splice(0)) {
      app.unmount()
      root.remove()
    }
  })

  it('opens Grok in xAI device authorization mode', async () => {
    const root = mountDialog('grok')
    await settle()

    // The authorization-code flow needs a redirect target this deployment
    // cannot receive, so Grok must not fall back to it.
    expect(endpointMocks.startProviderLevelOAuth).not.toHaveBeenCalled()
    expect(root.textContent).toContain('设备授权')
    expect(root.textContent).toContain('导入 Token')
    expect(root.textContent).not.toContain('前往授权')
    expect(root.textContent).not.toContain('Grok sso/session token')
  })

  it('starts the device grant without Kiro region or start_url', async () => {
    endpointMocks.startDeviceAuthorize.mockResolvedValue({
      session_id: 'session-1',
      user_code: 'ABCD-1234',
      verification_uri: 'https://x.ai/device',
      verification_uri_complete: 'https://x.ai/device?code=ABCD-1234',
      expires_in: 600,
      interval: 5,
      auth_type: 'device',
    })
    const root = mountDialog('grok')
    await settle()

    getButton(root, '开始授权')?.click()
    await settle()

    expect(endpointMocks.startDeviceAuthorize).toHaveBeenCalledWith('provider-1', {
      auth_type: 'device',
      login_option: undefined,
      start_url: undefined,
      region: undefined,
      proxy_node_id: undefined,
    })
    expect(root.textContent).toContain('ABCD-1234')
  })

  it('self-polls the device grant instead of waiting on a callback', async () => {
    endpointMocks.startDeviceAuthorize.mockResolvedValue({
      session_id: 'session-1',
      user_code: 'ABCD-1234',
      verification_uri: 'https://x.ai/device',
      verification_uri_complete: 'https://x.ai/device?code=ABCD-1234',
      expires_in: 600,
      interval: 5,
      auth_type: 'device',
    })
    endpointMocks.pollDeviceAuthorize.mockResolvedValue({
      status: 'pending',
      replaced: false,
    })
    const root = mountDialog('grok')
    await settle()

    getButton(root, '开始授权')?.click()
    await settle()

    // Kiro's shared default auth_type is a social one; Grok must not inherit
    // its callback prompt.
    expect(root.querySelector('textarea[placeholder*="callback"]')).toBeNull()
    expect(root.textContent).not.toContain('验证')
  })

  it('uses the backend interval after xAI asks the device poller to slow down', async () => {
    vi.useFakeTimers()
    endpointMocks.startDeviceAuthorize.mockResolvedValue({
      session_id: 'session-1',
      user_code: 'ABCD-1234',
      verification_uri: 'https://x.ai/device',
      verification_uri_complete: 'https://x.ai/device?code=ABCD-1234',
      expires_in: 600,
      interval: 31,
      auth_type: 'device',
    })
    endpointMocks.pollDeviceAuthorize
      .mockResolvedValueOnce({ status: 'slow_down', interval: 36, replaced: false })
      .mockResolvedValue({ status: 'pending', interval: 36, replaced: false })
    const root = mountDialog('grok')
    await settle()

    getButton(root, '开始授权')?.click()
    await settle()

    await vi.advanceTimersByTimeAsync(31_000)
    expect(endpointMocks.pollDeviceAuthorize).toHaveBeenCalledTimes(1)

    // The previous 30-second cap would already have sent this second poll.
    await vi.advanceTimersByTimeAsync(35_000)
    expect(endpointMocks.pollDeviceAuthorize).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1_000)
    expect(endpointMocks.pollDeviceAuthorize).toHaveBeenCalledTimes(2)
  })

  it('imports only official xAI OAuth token fields', async () => {
    const root = mountDialog('grok')
    await settle()

    Array.from(root.querySelectorAll('button'))
      .filter(button => button.textContent?.includes('导入 Token'))
      .at(-1)
      ?.click()
    await settle()

    const textarea = getImportTextarea(root)
    textarea.value = JSON.stringify({
      refresh_token: 'xai-refresh-token',
      access_token: 'xai-access-token',
      expires_at: 1_800_000_000,
      email: 'grok@example.com',
      account_name: 'xAI Account',
      sso_token: 'must-not-be-imported',
      cf_clearance: 'must-not-be-imported',
    })
    textarea.dispatchEvent(new Event('input'))
    await settle()

    Array.from(root.querySelectorAll('button'))
      .filter(button => button.textContent?.includes('导入 Token'))
      .at(-1)
      ?.click()
    await settle()

    expect(endpointMocks.importProviderRefreshToken).toHaveBeenCalledWith('provider-1', {
      refresh_token: 'xai-refresh-token',
      access_token: 'xai-access-token',
      expires_at: 1_800_000_000,
      name: undefined,
      email: 'grok@example.com',
      account_id: undefined,
      account_user_id: undefined,
      plan_type: undefined,
      pool_tier: undefined,
      sso_rw_token: undefined,
      cf_cookies: undefined,
      cf_clearance: undefined,
      user_agent: undefined,
      browser_profile: undefined,
      user_id: undefined,
      account_name: 'xAI Account',
      proxy_node_id: undefined,
    })
  })
})
