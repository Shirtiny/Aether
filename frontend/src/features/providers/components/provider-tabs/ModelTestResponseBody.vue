<template>
  <div
    class="min-w-0 space-y-2"
    data-testid="model-test-full-response"
  >
    <div class="flex flex-wrap items-center gap-2">
      <Button
        v-if="text !== null"
        variant="outline"
        size="sm"
        :aria-pressed="!showJson"
        @click="showJson = false"
      >
        完整文本
      </Button>
      <Button
        variant="outline"
        size="sm"
        :aria-pressed="showJson || text === null"
        @click="showJson = true"
      >
        完整响应 JSON / 原文
      </Button>
      <Button
        variant="ghost"
        size="sm"
        :disabled="!display"
        @click="copyToClipboard(display)"
      >
        复制完整响应
      </Button>
    </div>
    <pre
      v-if="display"
      class="whitespace-pre-wrap break-words [overflow-wrap:anywhere] rounded-md border border-border/60 bg-muted/20 p-3 font-mono text-xs"
    >{{ display }}</pre>
    <p
      v-else
      class="text-xs text-muted-foreground"
    >
      无响应体数据
    </p>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import Button from '@/components/ui/button.vue'
import { useClipboard } from '@/composables/useClipboard'
import { extractModelTestResponseText, formatModelTestResponseBody } from './model-test-response'

const props = defineProps<{ body: unknown }>()
const { copyToClipboard } = useClipboard()
const showJson = ref(false)
const text = computed(() => extractModelTestResponseText(props.body))
const display = computed(() => showJson.value || text.value === null
  ? formatModelTestResponseBody(props.body)
  : text.value)
watch(() => props.body, () => { showJson.value = false })
</script>
