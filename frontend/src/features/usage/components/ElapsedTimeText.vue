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
  precision?: number
  syncDelayThresholdMs?: number
}>(), {
  createdAt: null,
  status: null,
  responseTimeMs: null,
  precision: 2,
  syncDelayThresholdMs: 60_000,
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

  if (!props.createdAt) return '-'

  const createdAtMs = parseCreatedAtMs(props.createdAt)
  if (Number.isNaN(createdAtMs)) return '-'

  const elapsedMs = Math.max(0, now.value - createdAtMs)
  if (terminalSyncDelayed.value && props.responseTimeMs != null) {
    return `${(props.responseTimeMs / 1000).toFixed(precision.value)}s · 终态同步中`
  }
  return `${(elapsedMs / 1000).toFixed(precision.value)}s`
})

const terminalSyncDelayed = computed(() => {
  if (!isActive.value || !props.createdAt) return false
  if (props.responseTimeMs == null || !Number.isFinite(props.responseTimeMs) || props.responseTimeMs <= 0) {
    return false
  }
  const createdAtMs = parseCreatedAtMs(props.createdAt)
  if (Number.isNaN(createdAtMs)) return false
  const elapsedMs = Math.max(0, now.value - createdAtMs)
  const thresholdMs = Math.max(1_000, props.syncDelayThresholdMs)
  return elapsedMs >= props.responseTimeMs + thresholdMs
})

const displayTitle = computed(() => {
  if (!terminalSyncDelayed.value || !props.createdAt || props.responseTimeMs == null) return undefined
  const createdAtMs = parseCreatedAtMs(props.createdAt)
  if (Number.isNaN(createdAtMs)) return undefined
  const syncDelayMs = Math.max(0, now.value - createdAtMs - props.responseTimeMs)
  return `请求已记录 ${(props.responseTimeMs / 1000).toFixed(precision.value)}s 响应耗时，终态同步延迟 ${(syncDelayMs / 1000).toFixed(precision.value)}s`
})
</script>
