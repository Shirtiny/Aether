<template>
  <Dialog
    :open="open"
    size="2xl"
    :title="dialogTitle"
    :description="dialogDescription"
    @update:open="(val: boolean) => { if (!val) emit('close') }"
  >
    <div
      v-if="showSelection"
      class="space-y-2"
    >
      <button
        v-for="endpoint in endpoints"
        :key="endpoint.id"
        type="button"
        class="w-full rounded-lg border border-border/60 px-3 py-3 text-left transition-colors hover:bg-muted/40"
        @click="emit('select-endpoint', endpoint.id)"
      >
        <div class="flex items-center justify-between gap-3">
          <div class="min-w-0">
            <div class="text-sm font-medium">
              {{ formatApiFormat(endpoint.api_format) }}
            </div>
            <div class="mt-1 text-xs text-muted-foreground truncate">
              {{ endpoint.base_url }}
            </div>
          </div>
          <Badge variant="outline">
            {{ endpoint.is_active ? '已启用' : '已禁用' }}
          </Badge>
        </div>
      </button>
      <div
        v-if="endpoints.length === 0"
        class="rounded-lg border border-dashed border-border/60 px-3 py-6 text-center text-sm text-muted-foreground"
      >
        暂无可用于测试的活跃端点
      </div>
    </div>

    <div
      v-else-if="testing"
      class="flex flex-col items-center justify-center gap-3 py-10 text-center"
    >
      <Loader2 class="w-8 h-8 animate-spin text-primary" />
      <div class="space-y-1">
        <p class="text-sm font-medium">
          正在测试模型
        </p>
        <p class="text-xs text-muted-foreground">
          {{ selectingModelName || '-' }}
        </p>
        <p
          v-if="selectedEndpoint"
          class="text-xs text-muted-foreground"
        >
          端点：{{ formatApiFormat(selectedEndpoint.api_format) }} · {{ selectedEndpoint.base_url }}
        </p>
      </div>
    </div>

    <div
      v-else-if="result"
      class="space-y-4"
    >
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-2">
          <Badge :variant="result.success ? 'success' : 'destructive'">
            {{ result.success ? '成功' : '失败' }}
          </Badge>
          <span class="text-sm text-muted-foreground">
            {{ modeLabel }}
          </span>
        </div>
        <div class="text-xs text-muted-foreground">
          候选 {{ result.total_candidates }} / 尝试 {{ result.total_attempts }}
        </div>
      </div>

      <div class="text-sm space-y-1">
        <div>
          <span class="text-muted-foreground">请求模型: </span>
          <span class="font-medium">{{ result.model }}</span>
        </div>
        <div v-if="selectedEndpoint">
          <span class="text-muted-foreground">测试端点: </span>
          <span class="font-medium">{{ formatApiFormat(selectedEndpoint.api_format) }}</span>
          <span class="text-xs text-muted-foreground ml-1">{{ selectedEndpoint.base_url }}</span>
        </div>
        <div v-if="successEffectiveModel">
          <span class="text-muted-foreground">发送模型: </span>
          <span class="font-medium text-primary">{{ successEffectiveModel }}</span>
          <span
            v-if="successEffectiveModel !== result.model"
            class="text-xs text-muted-foreground ml-1"
          >(已映射)</span>
        </div>
      </div>

      <div
        v-if="result.error && !result.success"
        class="rounded-md bg-destructive/10 border border-destructive/20 px-3 py-2 text-xs text-destructive"
      >
        {{ result.error }}
      </div>

      <!-- mobile: list layout -->
      <div
        v-if="result.attempts.length > 0"
        class="space-y-2 sm:hidden"
      >
        <div
          v-for="(attempt, idx) in result.attempts"
          :key="'m' + idx"
          class="rounded-md border px-3 py-2 text-xs"
          :class="attemptRowClass(attempt.status)"
        >
          <div class="flex items-center justify-between gap-2">
            <div class="flex items-center gap-1.5 min-w-0">
              <span class="text-muted-foreground shrink-0">#{{ attempt.candidate_index }}</span>
              <Badge
                :variant="statusVariant(attempt.status)"
                class="text-[10px] px-1.5 py-0 shrink-0"
              >
                {{ attempt.status_code || statusLabel(attempt.status) }}
              </Badge>
              <span
                v-if="attempt.latency_ms != null"
                class="text-muted-foreground shrink-0 tabular-nums"
              >
                {{ attempt.latency_ms }}ms
              </span>
            </div>
          </div>
          <div class="mt-1.5 space-y-0.5">
            <div
              v-if="attempt.key_name"
              class="font-medium truncate"
            >
              {{ attempt.key_name }}
            </div>
            <div class="text-muted-foreground">
              {{ maskKey(attempt.key_id) }}
            </div>
            <div
              v-if="hasEffectiveModel && attempt.effective_model"
              class="text-muted-foreground"
            >
              模型: {{ attempt.effective_model }}
            </div>
            <div
              v-if="attemptDetail(attempt) !== '-'"
              class="text-muted-foreground break-all mt-1"
            >
              {{ attemptDetail(attempt) }}
            </div>
          </div>
        </div>
      </div>

      <!-- desktop: table layout -->
      <div
        v-if="result.attempts.length > 0"
        class="border rounded-md overflow-hidden hidden sm:block"
      >
        <table class="w-full text-xs table-fixed">
          <colgroup>
            <col class="w-8">
            <col class="w-[22%]">
            <col
              v-if="hasEffectiveModel"
              class="w-[18%]"
            >
            <col class="w-16">
            <col class="w-16">
            <col>
          </colgroup>
          <thead>
            <tr class="border-b bg-muted/30">
              <th class="pl-3 pr-1 py-2 text-left font-medium">
                #
              </th>
              <th class="px-3 py-2 text-left font-medium">
                Key
              </th>
              <th
                v-if="hasEffectiveModel"
                class="px-3 py-2 text-left font-medium"
              >
                发送模型
              </th>
              <th class="px-3 py-2 text-left font-medium">
                状态
              </th>
              <th class="px-3 py-2 text-right font-medium">
                延迟
              </th>
              <th class="px-3 py-2 text-left font-medium">
                详情
              </th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="(attempt, idx) in result.attempts"
              :key="idx"
              class="border-b last:border-b-0 align-top"
              :class="attemptRowClass(attempt.status)"
            >
              <td class="pl-3 pr-1 py-2 text-muted-foreground">
                {{ attempt.candidate_index }}
              </td>
              <td class="px-3 py-2">
                <div
                  v-if="attempt.key_name"
                  class="font-medium truncate"
                  :title="attempt.key_name"
                >
                  {{ attempt.key_name }}
                </div>
                <div
                  class="text-muted-foreground truncate"
                  :title="attempt.key_id"
                >
                  {{ maskKey(attempt.key_id) }}
                </div>
              </td>
              <td
                v-if="hasEffectiveModel"
                class="px-3 py-2 truncate"
                :title="attempt.effective_model || '-'"
              >
                {{ attempt.effective_model || '-' }}
              </td>
              <td class="px-3 py-2">
                <Badge
                  :variant="statusVariant(attempt.status)"
                  class="text-[10px] px-1.5 py-0"
                >
                  {{ attempt.status_code || statusLabel(attempt.status) }}
                </Badge>
              </td>
              <td class="px-3 py-2 text-right text-muted-foreground tabular-nums">
                {{ attempt.latency_ms != null ? attempt.latency_ms + 'ms' : '-' }}
              </td>
              <td class="px-3 py-2 text-muted-foreground">
                <div
                  class="break-all line-clamp-2"
                  :title="attemptDetail(attempt)"
                >
                  {{ attemptDetail(attempt) }}
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <div
        v-else
        class="text-center text-sm text-muted-foreground py-4"
      >
        没有可用的候选进行测试
      </div>
    </div>

    <template #footer>
      <Button
        v-if="showResult && canReselect"
        variant="outline"
        size="sm"
        @click="emit('back')"
      >
        重新选择端点
      </Button>
      <Button
        variant="outline"
        size="sm"
        @click="emit('close')"
      >
        {{ showSelection ? '取消' : '关闭' }}
      </Button>
    </template>
  </Dialog>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { Loader2 } from 'lucide-vue-next'
import { Dialog, Badge } from '@/components/ui'
import Button from '@/components/ui/button.vue'
import { formatApiFormat } from '@/api/endpoints/types/api-format'
import type { TestModelFailoverResponse, TestAttemptDetail } from '@/api/endpoints/providers'

type TestEndpointOption = {
  id: string
  api_format: string
  base_url: string
  is_active: boolean
}

const props = defineProps<{
  open: boolean
  result: TestModelFailoverResponse | null
  mode?: 'global' | 'direct'
  selectingModelName?: string | null
  endpoints?: TestEndpointOption[]
  selectedEndpoint?: TestEndpointOption | null
  testing?: boolean
  showEndpointSelector?: boolean
}>()

const emit = defineEmits<{
  close: []
  back: []
  'select-endpoint': [endpointId: string]
}>()

const endpoints = computed(() => props.endpoints ?? [])
const showSelection = computed(() => props.open && !!props.showEndpointSelector && !props.testing && !props.result)
const showResult = computed(() => !!props.result)
const canReselect = computed(() => !!props.showEndpointSelector && endpoints.value.length > 1)

const dialogTitle = computed(() => {
  if (props.result) return '模型测试结果'
  return '模型测试'
})

const dialogDescription = computed(() => {
  if (showSelection.value && props.selectingModelName) {
    return `为 ${props.selectingModelName} 选择端点`
  }
  if (props.testing && props.selectedEndpoint) {
    return `正在通过 ${formatApiFormat(props.selectedEndpoint.api_format)} 测试 ${props.selectingModelName || '模型'}`
  }
  return ''
})

const modeLabel = computed(() => {
  if (props.mode === 'global') return '模拟外部请求'
  if (props.mode === 'direct') return '直接测试'
  return ''
})

const successEffectiveModel = computed(() => {
  if (!props.result) return null
  const successAttempt = props.result.attempts.find(a => a.status === 'success')
  return successAttempt?.effective_model || null
})

const hasEffectiveModel = computed(() => {
  if (!props.result) return false
  return props.result.attempts.some(a => a.effective_model && a.effective_model !== props.result?.model)
})

function statusVariant(status: string) {
  if (status === 'success') return 'success' as const
  if (status === 'failed') return 'destructive' as const
  return 'secondary' as const
}

function statusLabel(status: string) {
  if (status === 'success') return '成功'
  if (status === 'failed') return '失败'
  if (status === 'skipped') return '跳过'
  return status
}

function attemptRowClass(status: string) {
  if (status === 'success') return 'bg-green-500/5'
  if (status === 'failed') return 'bg-red-500/5'
  if (status === 'skipped') return 'bg-muted/20'
  return ''
}

function maskKey(key: string): string {
  if (key.length <= 8) return key
  return `${key.slice(0, 4)}...${key.slice(-4)}`
}

function attemptDetail(attempt: TestAttemptDetail): string {
  if (attempt.skip_reason) return attempt.skip_reason
  if (attempt.error_message) return attempt.error_message
  if (attempt.status === 'success') return attempt.endpoint_base_url
  return '-'
}
</script>
