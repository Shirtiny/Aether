<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from 'vue'
import { Button } from '@/components/ui'
import { getProvider, getAccountTurnStateStatus, type TurnStateCollectionStatus } from '@/api/endpoints'

const props = defineProps<{ providerId: string, keyId?: string }>()
const status = ref<TurnStateCollectionStatus | null>(null)
const loading = ref(false)
const error = ref('')
let generation = 0

async function refresh() {
  const request = ++generation
  loading.value = true
  error.value = ''
  // Do not keep displaying an old success after a failed status refresh.
  status.value = null
  try {
    const snapshot = props.keyId
      ? await getAccountTurnStateStatus(props.providerId, props.keyId)
      : (await getProvider(props.providerId)).turn_state_collection_status
    if (request !== generation) return
    status.value = snapshot ?? null
    if (!status.value) error.value = '当前服务未返回采集状态'
  } catch {
    if (request === generation) error.value = '采集状态读取失败，请重试'
  } finally {
    if (request === generation) loading.value = false
  }
}
watch(() => [props.providerId, props.keyId], refresh, { immediate: true })
onBeforeUnmount(() => { generation += 1 })

const labels = {
  disabled: '已关闭', pending: '等待采集', collecting: '采集中',
  success: '最近采集成功', failed: '最近采集失败', unknown: '等待下次调度（旧版未记录结果）',
}
function time(timestamp: number | null) {
  return timestamp === null ? '暂无' : new Date(timestamp * 1000).toLocaleString()
}
</script>

<template>
  <div
    class="space-y-2 border-t pt-3"
    data-testid="turn-state-status"
  >
    <div class="flex items-center justify-between gap-2">
      <span class="text-sm font-medium">采集状态</span>
      <Button
        type="button"
        variant="outline"
        size="sm"
        :disabled="loading"
        @click="refresh"
      >
        {{ loading ? '读取中…' : '刷新状态' }}
      </Button>
    </div>
    <p class="text-xs text-muted-foreground">
      仅显示已保存配置；刷新状态不会发起采集请求。时间按浏览器时区显示，票据有效表示本地未过期，不保证上游接受。
    </p>
    <p
      v-if="error"
      class="text-xs text-destructive"
      role="alert"
    >
      {{ error }}
    </p>
    <p
      v-else-if="status && !status.available"
      class="text-xs text-destructive"
      role="alert"
    >
      运行时缓存不可用，暂时无法确认采集状态。
    </p>
    <template v-else-if="status">
      <p
        v-if="!status.enabled"
        class="text-xs text-muted-foreground"
      >
        采集未启用或来源渠道已停用。
      </p>
      <p
        v-if="status.models.length === 0"
        class="text-xs text-muted-foreground"
      >
        尚未配置采集模型。
      </p>
      <div
        v-for="item in status.models"
        :key="item.model"
        class="space-y-1 rounded border p-2 text-xs"
      >
        <div class="flex flex-wrap items-center justify-between gap-2">
          <span class="font-medium">{{ item.model }}</span>
          <span :class="item.result === 'failed' ? 'text-destructive' : 'text-muted-foreground'">{{ labels[item.result] }}</span>
        </div>
        <p>{{ item.ticket_valid ? `有效票据：${item.ticket_length} 字符` : '无可用票据，不执行覆盖' }}</p>
        <p
          v-if="item.error"
          class="break-words text-destructive"
        >
          {{ item.error }}
        </p>
        <div class="grid gap-1 text-muted-foreground sm:grid-cols-2">
          <span>最近尝试：{{ time(item.last_attempt_at) }}</span>
          <span>最近成功：{{ time(item.last_success_at) }}</span>
          <span>本地到期：{{ time(item.expires_at) }}</span>
          <span>下次尝试：{{ !status.enabled ? '已关闭' : item.next_attempt_at === null ? '等待后台调度' : time(item.next_attempt_at) }}</span>
        </div>
      </div>
      <p class="text-xs text-muted-foreground">
        状态读取于：{{ time(status.checked_at) }}
      </p>
    </template>
  </div>
</template>
