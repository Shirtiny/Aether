<template>
  <section
    class="rounded-lg border border-border bg-card p-4 space-y-3"
    data-internal-retry-details
  >
    <div class="flex flex-wrap items-center justify-between gap-2">
      <h4 class="text-sm font-medium">
        Aether 内部过载重试
      </h4>
      <span
        class="text-xs rounded bg-muted px-2 py-1"
        data-internal-retry-outcome
      >{{ internalRetryLabel(info) }}</span>
    </div>
    <p
      v-if="!info"
      class="text-xs text-muted-foreground"
    >
      未记录：历史记录、进行中的请求或此执行路径尚无内部重试信息，不能据此认定没有重试。
    </p>
    <template v-else>
      <p class="text-xs text-muted-foreground">
        仅包含 Aether 同执行计划内的过载重试，不包含 sub2api 补救。候选级重试/切换见请求轨迹；最终结果以请求终态为准。
      </p>
      <p
        v-if="!info.complete"
        class="text-xs text-amber-600"
      >
        部分执行尝试未记录，次数可能不完整。
      </p>
      <div
        v-for="(attempt, index) in info.attempts"
        :key="index"
        class="space-y-2 border-t border-border pt-3 text-xs"
      >
        <p class="font-medium">
          执行候选 #{{ attempt.candidate_index ?? '?' }} · 内部重试 {{ attempt.retry_count }} 次
        </p>
        <ul
          v-if="attempt.failures.length"
          class="space-y-1 text-muted-foreground"
        >
          <li
            v-for="failure in attempt.failures"
            :key="failure.attempt"
            class="flex flex-wrap gap-x-2 gap-y-1"
          >
            <span>第 {{ failure.attempt }} 次尝试：HTTP {{ failure.status_code }}</span>
            <span>{{ failure.reason === 'overloaded' ? '上游过载' : '不符合过载重试条件' }}</span>
            <span v-if="failure.planned_wait_ms != null">计划等待 {{ failure.planned_wait_ms }} ms</span>
            <span v-if="failure.wait_ms != null">实际等待 {{ failure.wait_ms }} ms</span>
            <span>{{ failure.retry_started ? '已发起重试' : '未继续发起重试' }}</span>
          </li>
        </ul>
        <p>{{ internalRetryStopReason(attempt.stop_reason) }}</p>
        <p
          v-if="attempt.stop_event_type"
          class="font-mono break-all text-muted-foreground"
        >
          事件：{{ attempt.stop_event_type }}
        </p>
      </div>
    </template>
  </section>
</template>

<script setup lang="ts">
import type { InternalRetryInfo } from '@/types/internalRetry'
import { internalRetryLabel, internalRetryStopReason } from '../utils/internalRetry'
defineProps<{ info?: InternalRetryInfo | null }>()
</script>
