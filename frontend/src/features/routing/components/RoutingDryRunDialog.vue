<template>
  <div class="space-y-4">
    <div class="grid gap-3 sm:grid-cols-2">
      <input
        v-model="draft.model"
        class="h-10 rounded-md border border-border bg-background px-3 text-sm"
        placeholder="模型"
      >
      <input
        v-model="draft.api_format"
        class="h-10 rounded-md border border-border bg-background px-3 text-sm"
        placeholder="API 格式"
      >
    </div>

    <RoutingTraceViewer
      v-if="trace"
      :trace="trace"
    />
  </div>
</template>

<script setup lang="ts">
import { reactive } from 'vue'

import RoutingTraceViewer from './RoutingTraceViewer.vue'
import type { RoutingDecisionTrace } from '../utils/routingTrace'

const props = defineProps<{
  trace?: RoutingDecisionTrace | null
  model?: string
  apiFormat?: string
}>()

const draft = reactive({
  model: props.model ?? '',
  api_format: props.apiFormat ?? 'openai:chat',
})
</script>
