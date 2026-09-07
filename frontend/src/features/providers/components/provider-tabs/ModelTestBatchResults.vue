<template>
  <div class="space-y-4">
    <div class="space-y-2 rounded-lg border border-border/60 bg-muted/20 p-3">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <span class="text-sm font-medium">{{ testing ? '批量测试进行中' : '批量测试结束' }}</span>
        <Button
          v-if="testing"
          variant="outline"
          size="sm"
          @click="emit('cancel')"
        >
          停止批量测试
        </Button>
      </div>
      <div
        role="status"
        class="text-xs text-muted-foreground"
      >
        已完成 {{ completed }}/{{ entries.length }} · 成功 {{ count('success') }} · 失败 {{ count('failed') }} · 跳过 {{ count('skipped') }} · 已取消 {{ count('cancelled') }}
      </div>
      <p class="text-xs text-muted-foreground">
        每个 Key 独立请求，最多并发 3 个；停止后保留已完成结果，已到达上游的请求可能继续执行并计费。响应不截断，可切换完整 JSON 查看所有字段。
      </p>
    </div>

    <section
      v-for="entry in entries"
      :key="entry.requestId"
      :data-key-id="entry.keyId"
      class="min-w-0 space-y-3 rounded-lg border border-border/60 p-3"
    >
      <div class="flex flex-wrap items-center justify-between gap-2">
        <div class="min-w-0 break-all text-sm font-medium">
          {{ entry.keyName || entry.keyId }}
        </div>
        <div class="flex shrink-0 items-center gap-2 text-xs">
          <Badge :variant="entry.status === 'success' ? 'success' : entry.status === 'failed' ? 'destructive' : 'secondary'">
            {{ labels[entry.status] }}
          </Badge>
          <span v-if="entry.statusCode !== null">HTTP {{ entry.statusCode }}</span>
          <span v-if="entry.elapsedMs !== null">{{ entry.elapsedMs }}ms</span>
        </div>
      </div>
      <p class="break-all text-[11px] text-muted-foreground">
        Key ID：{{ entry.keyId }} · 请求 ID：{{ entry.requestId }}
      </p>
      <pre
        v-if="entry.error"
        class="whitespace-pre-wrap break-words [overflow-wrap:anywhere] text-xs text-destructive"
      >{{ entry.error }}</pre>
      <div
        v-for="(attempt, index) in entry.result?.attempts ?? []"
        :key="index"
        class="min-w-0 space-y-2"
      >
        <div class="flex flex-wrap gap-2 text-xs text-muted-foreground">
          <span>{{ attempt.endpoint_api_format }}</span>
          <span v-if="attempt.status_code != null">HTTP {{ attempt.status_code }}</span>
          <span v-if="attempt.latency_ms != null">上游 {{ attempt.latency_ms }}ms</span>
          <span v-if="attempt.effective_model">{{ attempt.effective_model }}</span>
        </div>
        <pre
          v-if="attempt.skip_reason || attempt.error_message"
          class="whitespace-pre-wrap break-words [overflow-wrap:anywhere] text-xs text-destructive"
        >{{ attempt.skip_reason || attempt.error_message }}</pre>
        <ModelTestResponseBody :body="attempt.response_body" />
        <details class="space-y-2 text-xs">
          <summary class="cursor-pointer text-muted-foreground">
            请求与响应头（敏感字段已脱敏）
          </summary>
          <div
            v-for="detail in diagnostics(attempt)"
            :key="detail.name"
            class="space-y-1"
          >
            <div class="font-medium">
              {{ detail.name }}
            </div>
            <pre class="whitespace-pre-wrap break-words [overflow-wrap:anywhere] rounded border p-2">{{ formatModelTestResponseBody(detail.value) || '无数据' }}</pre>
          </div>
        </details>
      </div>
      <ModelTestResponseBody
        v-if="entry.errorResponse != null"
        :body="entry.errorResponse"
      />
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { Badge, Button } from '@/components/ui'
import type { ModelTestBatchEntry } from '@/composables/useModelTest'
import type { TestAttemptDetail } from '@/api/endpoints/providers'
import ModelTestResponseBody from './ModelTestResponseBody.vue'
import { formatModelTestResponseBody } from './model-test-response'

const props = defineProps<{ entries: ModelTestBatchEntry[]; testing: boolean }>()
const emit = defineEmits<{ cancel: [] }>()
const labels: Record<ModelTestBatchEntry['status'], string> = {
  pending: '等待中', running: '测试中', success: '成功', failed: '失败', skipped: '跳过', cancelled: '已取消',
}
const count = (status: ModelTestBatchEntry['status']) => props.entries.filter(entry => entry.status === status).length
const completed = computed(() => props.entries.filter(entry => entry.status !== 'pending' && entry.status !== 'running').length)

function diagnostics(attempt: TestAttemptDetail) {
  return [
    { name: '请求 URL', value: attempt.request_url },
    { name: '请求头', value: attempt.request_headers },
    { name: '请求体', value: attempt.request_body },
    { name: '响应头', value: attempt.response_headers },
  ]
}
</script>
