<template>
  <span
    class="tabular-nums"
    :title="displayTitle"
    :data-terminal-sync-delayed="terminalSyncDelayed ? 'true' : undefined"
  >{{ displayText }}</span>
</template>

<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from 'vue'

const props = withDefaults(defineProps<{
  createdAt?: string | null
  status?: string | null
  responseTimeMs?: number | null
  terminalSyncPending?: boolean
  terminalResponseTimeMs?: number | null
  precision?: number
}>(), {
  createdAt: null,
  status: null,
  responseTimeMs: null,
  terminalSyncPending: false,
  terminalResponseTimeMs: null,
  precision: 2,
})

const now = ref(Date.now())
const precision = computed(() => Math.max(0, props.precision))
const isActive = computed(() => props.status === 'pending' || props.status === 'streaming')

let rafId: number | null = null

function parseCreatedAtMs(value: string | null | undefined): number {
  if (!value) return Number.NaN
  // 后端有时返回无时区时间，按 UTC 解析，和列表时间显示逻辑保持一致
  const normalized = /(?:Z|[+-]\d{2}:\d{2})$/i.test(value) ? value : `${value}Z`
  return new Date(normalized).getTime()
}

function stopRaf() {
  if (rafId == null) return
  cancelAnimationFrame(rafId)
  rafId = null
}

function tick() {
  now.value = Date.now()
  rafId = requestAnimationFrame(tick)
}

function startRaf() {
  stopRaf()
  now.value = Date.now()
  rafId = requestAnimationFrame(tick)
}

watch(isActive, (active) => {
  if (active) {
    startRaf()
  } else {
    stopRaf()
  }
}, { immediate: true })

onUnmounted(() => {
  stopRaf()
})

const displayText = computed(() => {
  if (!isActive.value) {
    if (props.responseTimeMs == null) return '-'
    return `${(props.responseTimeMs / 1000).toFixed(precision.value)}s`
  }

  if (terminalSyncDelayed.value) {
    const terminalResponseTimeMs = resolvedTerminalResponseTimeMs.value
    if (terminalResponseTimeMs == null) return '终态同步中'
    return `${(terminalResponseTimeMs / 1000).toFixed(precision.value)}s · 终态同步中`
  }

  if (!props.createdAt) return '-'

  const createdAtMs = parseCreatedAtMs(props.createdAt)
  if (Number.isNaN(createdAtMs)) return '-'

  const elapsedMs = Math.max(0, now.value - createdAtMs)
  return `${(elapsedMs / 1000).toFixed(precision.value)}s`
})

const terminalSyncDelayed = computed(() => {
  return isActive.value && props.terminalSyncPending === true
})

const resolvedTerminalResponseTimeMs = computed(() => {
  if (
    props.terminalResponseTimeMs != null &&
    Number.isFinite(props.terminalResponseTimeMs) &&
    props.terminalResponseTimeMs > 0
  ) {
    return props.terminalResponseTimeMs
  }
  if (
    props.responseTimeMs != null &&
    Number.isFinite(props.responseTimeMs) &&
    props.responseTimeMs > 0
  ) {
    return props.responseTimeMs
  }
  return null
})

const displayTitle = computed(() => {
  if (!terminalSyncDelayed.value) return undefined
  const terminalResponseTimeMs = resolvedTerminalResponseTimeMs.value
  if (!props.createdAt || terminalResponseTimeMs == null) {
    return 'Provider 请求已结束，正在等待用量终态同步'
  }
  const createdAtMs = parseCreatedAtMs(props.createdAt)
  if (Number.isNaN(createdAtMs)) return 'Provider 请求已结束，正在等待用量终态同步'
  const syncDelayMs = Math.max(0, now.value - createdAtMs - terminalResponseTimeMs)
  return `Provider 请求耗时 ${(terminalResponseTimeMs / 1000).toFixed(precision.value)}s，用量终态同步延迟 ${(syncDelayMs / 1000).toFixed(precision.value)}s`
})
</script>
