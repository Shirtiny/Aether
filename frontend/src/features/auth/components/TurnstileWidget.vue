<template>
  <div
    ref="containerRef"
    class="min-h-[1px]"
  />
</template>

<script setup lang="ts">
import { onBeforeUnmount, ref } from 'vue'

interface Props {
  siteKey: string
}

type TurnstileWidgetId = string

interface TurnstileRenderOptions {
  sitekey: string
  action?: string
  execution?: 'render' | 'execute'
  appearance?: 'always' | 'execute' | 'interaction-only'
  callback?: (token: string) => void
  'error-callback'?: () => void
  'expired-callback'?: () => void
  'timeout-callback'?: () => void
}

interface TurnstileApi {
  render: (container: HTMLElement, options: TurnstileRenderOptions) => TurnstileWidgetId
  execute: (widgetId: TurnstileWidgetId) => void
  reset: (widgetId: TurnstileWidgetId) => void
  remove?: (widgetId: TurnstileWidgetId) => void
}

declare global {
  interface Window {
    turnstile?: TurnstileApi
    __aetherTurnstileScriptPromise?: Promise<void>
  }
}

const props = defineProps<Props>()
const containerRef = ref<HTMLElement | null>(null)
const widgetId = ref<TurnstileWidgetId | null>(null)
let pendingReject: ((error: Error) => void) | null = null

function loadTurnstileScript(): Promise<void> {
  if (window.turnstile) {
    return Promise.resolve()
  }
  if (window.__aetherTurnstileScriptPromise) {
    return window.__aetherTurnstileScriptPromise
  }
  window.__aetherTurnstileScriptPromise = new Promise((resolve, reject) => {
    const rejectAndReset = (script: HTMLScriptElement) => {
      script.remove()
      delete window.__aetherTurnstileScriptPromise
      reject(new Error('Turnstile script failed'))
    }
    const existing = document.querySelector<HTMLScriptElement>(
      'script[data-aether-turnstile="true"]'
    )
    if (existing) {
      existing.addEventListener('load', () => resolve(), { once: true })
      existing.addEventListener('error', () => rejectAndReset(existing), {
        once: true,
      })
      return
    }
    const script = document.createElement('script')
    script.src = 'https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit'
    script.async = true
    script.defer = true
    script.dataset.aetherTurnstile = 'true'
    script.onload = () => resolve()
    script.onerror = () => rejectAndReset(script)
    document.head.appendChild(script)
  })
  return window.__aetherTurnstileScriptPromise
}

function clearWidget() {
  if (!widgetId.value || !window.turnstile) return
  if (window.turnstile.remove) {
    window.turnstile.remove(widgetId.value)
  } else {
    window.turnstile.reset(widgetId.value)
  }
  widgetId.value = null
}

async function execute(action: string): Promise<string> {
  await loadTurnstileScript()
  const turnstile = window.turnstile
  const container = containerRef.value
  if (!turnstile || !container) {
    throw new Error('Turnstile unavailable')
  }

  clearWidget()

  return new Promise((resolve, reject) => {
    pendingReject = reject
    const id = turnstile.render(container, {
      sitekey: props.siteKey,
      action,
      execution: 'execute',
      appearance: 'interaction-only',
      callback: (token: string) => {
        pendingReject = null
        resolve(token)
      },
      'error-callback': () => {
        pendingReject = null
        reject(new Error('Turnstile challenge failed'))
      },
      'expired-callback': () => {
        pendingReject = null
        reject(new Error('Turnstile token expired'))
      },
      'timeout-callback': () => {
        pendingReject = null
        reject(new Error('Turnstile challenge timed out'))
      },
    })
    widgetId.value = id
    turnstile.execute(id)
  })
}

function reset() {
  if (pendingReject) {
    pendingReject(new Error('Turnstile reset'))
    pendingReject = null
  }
  clearWidget()
}

onBeforeUnmount(reset)

defineExpose({
  execute,
  reset,
})
</script>
