<template>
  <Dialog
    :model-value="modelValue"
    title="账号批量操作"
    :description="dialogDescription"
    size="xl"
    persistent
    @update:model-value="emit('update:modelValue', $event)"
  >
    <div class="space-y-4">
      <div class="flex items-center gap-2">
        <MultiSelect
          :model-value="activeQuickSelectors"
          :options="QUICK_SELECT_OPTIONS"
          placeholder="快捷多选"
          trigger-class="h-8 w-40"
          dropdown-min-width="10rem"
          :disabled="loading || executing || allKeys.length === 0"
          @update:model-value="onQuickSelectChange"
        />
        <Input
          :model-value="searchText"
          placeholder="搜索账号名 / 套餐 / 额度 / 代理状态"
          class="h-8 flex-1"
          @update:model-value="(v) => searchText = String(v || '')"
        />
        <Button
          variant="ghost"
          size="icon"
          class="h-8 w-8 shrink-0"
          :disabled="loading || executing"
          @click="loadAllKeys()"
        >
          <RefreshCw
            class="h-3.5 w-3.5"
            :class="loading ? 'animate-spin' : ''"
          />
        </Button>
      </div>

      <div
        v-if="activeQuickSelectors.length > 0"
        class="flex flex-wrap gap-1"
      >
        <Badge
          v-for="sel in activeQuickSelectors"
          :key="sel"
          variant="secondary"
          class="text-[10px] px-1.5 py-0 h-5 cursor-pointer hover:bg-destructive/10 hover:text-destructive"
          @click="removeQuickSelector(sel)"
        >
          {{ QUICK_SELECT_OPTIONS.find(s => s.value === sel)?.label }}
          <svg
            xmlns="http://www.w3.org/2000/svg"
            width="10"
            height="10"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            class="ml-0.5"
          ><path d="M18 6 6 18" /><path d="m6 6 12 12" /></svg>
        </Badge>
      </div>

      <div class="flex items-center justify-between text-xs">
        <div class="text-muted-foreground">
          共 {{ allKeys.length }} 个账号，筛选 {{ filteredKeys.length }} 个，已选 {{ selectedKeyIds.length }} 个
        </div>
        <div class="flex items-center gap-2">
          <Checkbox
            :checked="isAllFilteredSelected"
            :indeterminate="isPartiallyFilteredSelected"
            :disabled="filteredKeys.length === 0 || loading || executing"
            @update:checked="toggleSelectFiltered"
          />
          <span class="text-muted-foreground">全选筛选结果</span>
        </div>
      </div>

      <div class="max-h-[380px] overflow-y-auto rounded-lg border">
        <div
          v-if="loading"
          class="py-10 text-center text-sm text-muted-foreground"
        >
          正在加载账号列表...
        </div>
        <div
          v-else-if="filteredKeys.length === 0"
          class="py-10 text-center text-sm text-muted-foreground"
        >
          无匹配账号
        </div>
        <label
          v-for="key in pagedKeys"
          :key="key.key_id"
          class="flex items-center gap-2.5 px-3 py-2 border-b last:border-b-0 cursor-pointer hover:bg-muted/30"
        >
          <Checkbox
            :checked="selectedIdSet.has(key.key_id)"
            :disabled="executing"
            @update:checked="(checked) => toggleOne(key.key_id, checked === true)"
          />
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-1.5">
              <span class="text-xs font-medium truncate">{{ key.key_name || '未命名' }}</span>
              <Badge
                v-if="isOAuthInvalid(key)"
                variant="destructive"
                class="text-[10px] px-1 py-0 h-4 shrink-0"
              >OAuth失效</Badge>
              <Badge
                v-else
                variant="outline"
                class="text-[10px] px-1 py-0 h-4 shrink-0"
              >{{ normalizeAuthTypeLabel(key.auth_type) }}</Badge>
              <Badge
                v-if="key.oauth_plan_type"
                variant="outline"
                class="text-[10px] px-1 py-0 h-4 shrink-0"
              >{{ key.oauth_plan_type }}</Badge>
              <Badge
                v-if="isBannedKey(key)"
                variant="destructive"
                class="text-[10px] px-1 py-0 h-4 shrink-0"
              >封号</Badge>
            </div>
            <div class="flex items-center gap-1.5 mt-0.5 text-[11px] text-muted-foreground flex-wrap">
              <span :class="key.is_active ? '' : 'text-destructive'">{{ key.is_active ? '启用' : '禁用' }}</span>
              <span v-if="key.account_quota">{{ shortenQuota(key.account_quota) }}</span>
              <span v-if="key.proxy?.node_id">独立代理</span>
              <span
                v-if="key.last_used_at"
                class="ml-auto shrink-0"
              >{{ formatRelativeTime(key.last_used_at) }}</span>
            </div>
          </div>
        </label>
      </div>

      <div
        v-if="totalPages > 1"
        class="flex items-center justify-between text-xs text-muted-foreground"
      >
        <span>第 {{ currentPage }} / {{ totalPages }} 页</span>
        <div class="flex items-center gap-1">
          <Button
            variant="ghost"
            size="icon"
            class="h-7 w-7"
            :disabled="currentPage <= 1"
            @click="currentPage = 1"
          >
            <ChevronsLeft class="h-3.5 w-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            class="h-7 w-7"
            :disabled="currentPage <= 1"
            @click="currentPage -= 1"
          >
            <ChevronLeft class="h-3.5 w-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            class="h-7 w-7"
            :disabled="currentPage >= totalPages"
            @click="currentPage += 1"
          >
            <ChevronRight class="h-3.5 w-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            class="h-7 w-7"
            :disabled="currentPage >= totalPages"
            @click="currentPage = totalPages"
          >
            <ChevronsRight class="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>

      <div class="space-y-2">
        <div class="flex items-center gap-2">
          <Select v-model="selectedAction">
            <SelectTrigger class="h-8 text-xs flex-1">
              <SelectValue placeholder="选择动作" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem
                v-for="item in ACTION_OPTIONS"
                :key="item.value"
                :value="item.value"
              >
                {{ item.label }}
              </SelectItem>
            </SelectContent>
          </Select>
          <Button
            variant="ghost"
            size="icon"
            class="h-8 w-8 shrink-0"
            :disabled="executing || selectedKeyIds.length === 0 || loading"
            @click="executeAction"
          >
            <Play
              class="h-3.5 w-3.5"
              :class="executing ? 'animate-pulse' : ''"
            />
          </Button>
        </div>
        <ProxyNodeSelect
          v-if="selectedAction === 'set_proxy'"
          :model-value="proxyNodeIdForAction"
          trigger-class="h-8"
          @update:model-value="(v: string) => proxyNodeIdForAction = v"
        />
      </div>

      <div
        v-if="executing && progressTotal > 0"
        class="space-y-1"
      >
        <div class="flex items-center justify-between text-xs text-muted-foreground">
          <span>{{ progressLabel }}</span>
          <span>{{ progressDone }} / {{ progressTotal }}</span>
        </div>
        <div class="h-1.5 w-full rounded-full bg-muted overflow-hidden">
          <div
            class="h-full rounded-full bg-primary transition-all duration-150"
            :style="{ width: `${Math.round((progressDone / progressTotal) * 100)}%` }"
          />
        </div>
      </div>
      <div
        v-else-if="lastResultMessage"
        class="rounded-md border bg-background px-3 py-2 text-xs text-muted-foreground"
      >
        {{ lastResultMessage }}
      </div>
    </div>

    <template #footer>
      <Button
        variant="outline"
        :disabled="executing"
        @click="emit('update:modelValue', false)"
      >
        关闭
      </Button>
    </template>
  </Dialog>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Dialog, Button, Input, Select, SelectTrigger, SelectValue, SelectContent, SelectItem, Checkbox, Badge } from '@/components/ui'
import { MultiSelect } from '@/components/common'
import ProxyNodeSelect from '@/features/providers/components/ProxyNodeSelect.vue'
import { RefreshCw, Play, ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight } from 'lucide-vue-next'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { parseApiError } from '@/utils/errorParser'
import { listPoolKeys, type PoolKeyDetail } from '@/api/endpoints/pool'
import { deleteEndpointKey, refreshProviderQuota, updateProviderKey } from '@/api/endpoints/keys'
import { refreshProviderOAuth } from '@/api/endpoints/provider_oauth'
import { useProxyNodesStore } from '@/stores/proxy-nodes'

type QuickSelectorValue =
  | 'banned'
  | 'no_quota'
  | 'plan_free'
  | 'plan_team'
  | 'oauth_invalid'
  | 'proxy_unset'
  | 'proxy_set'
  | 'disabled'
  | 'enabled'

type BatchActionValue =
  | 'delete'
  | 'refresh_oauth'
  | 'refresh_quota'
  | 'clear_proxy'
  | 'set_proxy'
  | 'enable'
  | 'disable'

const props = defineProps<{
  modelValue: boolean
  providerId: string
  providerName?: string
  batchConcurrency?: number | null
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
  changed: []
}>()

const QUICK_SELECT_OPTIONS: Array<{ value: QuickSelectorValue; label: string }> = [
  { value: 'banned', label: '已封号' },
  { value: 'no_quota', label: '无额度' },
  { value: 'plan_free', label: '全部 Free' },
  { value: 'plan_team', label: '全部 Team' },
  { value: 'oauth_invalid', label: 'OAuth 失效' },
  { value: 'proxy_unset', label: '未配置代理' },
  { value: 'proxy_set', label: '已配置独立代理' },
  { value: 'disabled', label: '已禁用' },
  { value: 'enabled', label: '已启用' },
]

const ACTION_OPTIONS: Array<{ value: BatchActionValue; label: string }> = [
  { value: 'delete', label: '删除账号' },
  { value: 'refresh_oauth', label: '刷新 OAuth' },
  { value: 'refresh_quota', label: '刷新额度' },
  { value: 'clear_proxy', label: '清除代理' },
  { value: 'set_proxy', label: '配置代理' },
  { value: 'enable', label: '启用' },
  { value: 'disable', label: '禁用' },
]

const { success, warning, error: showError } = useToast()
const { confirm } = useConfirm()
const proxyNodesStore = useProxyNodesStore()

const loading = ref(false)
const executing = ref(false)
const allKeys = ref<PoolKeyDetail[]>([])
const selectedKeyIds = ref<string[]>([])
const searchText = ref('')
const selectedAction = ref<BatchActionValue>('delete')
const proxyNodeIdForAction = ref('')
const lastResultMessage = ref('')
const progressTotal = ref(0)
const progressDone = ref(0)
const progressLabel = ref('')
const activeQuickSelectors = ref<QuickSelectorValue[]>([])
const currentPage = ref(1)
const PAGE_SIZE = 50
const dialogDescription = computed(() => {
  const name = (props.providerName || '').trim()
  return name ? `${name} - 选择账号并批量执行动作` : '选择账号并批量执行动作'
})

const selectedIdSet = computed(() => new Set(selectedKeyIds.value))

const filteredKeys = computed(() => {
  const keyword = normalizeText(searchText.value)
  if (!keyword) return allKeys.value
  return allKeys.value.filter((key) => {
    const parts = [
      key.key_name,
      key.auth_type,
      key.oauth_plan_type,
      key.account_quota,
      key.proxy?.node_id ? '独立代理' : '未配置代理',
      key.is_active ? '已启用' : '已禁用',
      key.oauth_invalid_reason,
    ]
    return parts.some((part) => normalizeText(part).includes(keyword))
  })
})

const totalPages = computed(() => Math.max(1, Math.ceil(filteredKeys.value.length / PAGE_SIZE)))

const pagedKeys = computed(() => {
  const start = (currentPage.value - 1) * PAGE_SIZE
  return filteredKeys.value.slice(start, start + PAGE_SIZE)
})

const isAllFilteredSelected = computed(() => {
  if (filteredKeys.value.length === 0) return false
  return filteredKeys.value.every((key) => selectedIdSet.value.has(key.key_id))
})

const isPartiallyFilteredSelected = computed(() => {
  if (filteredKeys.value.length === 0) return false
  const selectedCount = filteredKeys.value.filter((key) => selectedIdSet.value.has(key.key_id)).length
  return selectedCount > 0 && selectedCount < filteredKeys.value.length
})

function normalizeText(value: unknown): string {
  return String(value || '').trim().toLowerCase()
}

function normalizeAuthTypeLabel(authType: string): string {
  const text = normalizeText(authType)
  if (text === 'oauth') return 'OAuth'
  if (text === 'service_account') return 'Service'
  return 'API Key'
}

function isBannedKey(key: PoolKeyDetail): boolean {
  const reason = normalizeText(key.oauth_invalid_reason)
  if (reason && /(banned|forbidden|blocked|suspend|封|禁|受限)/.test(reason)) return true
  if (Array.isArray(key.scheduling_reasons)) {
    return key.scheduling_reasons.some((item) => {
      const code = normalizeText(item.code)
      return code === 'account_banned' || code === 'account_forbidden' || code === 'account_blocked'
    })
  }
  return false
}

function hasNoQuota(key: PoolKeyDetail): boolean {
  const quotaText = normalizeText(key.account_quota)
  if (!quotaText) return false
  if (/(无额度|额度不足|已耗尽|耗尽|depleted|exhausted|insufficient)/.test(quotaText)) return true
  if (/剩余\s*0(\.0+)?/.test(quotaText)) return true
  if (/\b0(\.0+)?\s*\/\s*\d/.test(quotaText)) return true
  if (/\b0(\.0+)?%/.test(quotaText)) return true
  return false
}

function isOAuthInvalid(key: PoolKeyDetail): boolean {
  if (normalizeText(key.auth_type) !== 'oauth') return false
  if (key.oauth_invalid_at != null || normalizeText(key.oauth_invalid_reason)) return true
  if (typeof key.oauth_expires_at === 'number' && key.oauth_expires_at > 0) {
    return key.oauth_expires_at * 1000 <= Date.now()
  }
  return false
}

function isFreePlan(key: PoolKeyDetail): boolean {
  return normalizeText(key.oauth_plan_type).includes('free')
}

function isTeamPlan(key: PoolKeyDetail): boolean {
  return normalizeText(key.oauth_plan_type).includes('team')
}

function toggleOne(keyId: string, checked: boolean): void {
  const set = new Set(selectedKeyIds.value)
  if (checked) set.add(keyId)
  else set.delete(keyId)
  selectedKeyIds.value = [...set]
}

function toggleSelectFiltered(checked: boolean | 'indeterminate'): void {
  const shouldSelect = checked === true
  const set = new Set(selectedKeyIds.value)
  if (shouldSelect) {
    for (const key of filteredKeys.value) set.add(key.key_id)
  } else {
    for (const key of filteredKeys.value) set.delete(key.key_id)
  }
  selectedKeyIds.value = [...set]
}

function matchesSelector(key: PoolKeyDetail, selector: QuickSelectorValue): boolean {
  if (selector === 'banned') return isBannedKey(key)
  if (selector === 'no_quota') return hasNoQuota(key)
  if (selector === 'plan_free') return isFreePlan(key)
  if (selector === 'plan_team') return isTeamPlan(key)
  if (selector === 'oauth_invalid') return isOAuthInvalid(key)
  if (selector === 'proxy_unset') return !key.proxy?.node_id
  if (selector === 'proxy_set') return Boolean(key.proxy?.node_id)
  if (selector === 'disabled') return !key.is_active
  if (selector === 'enabled') return key.is_active
  return false
}

function onQuickSelectChange(values: string[]): void {
  activeQuickSelectors.value = values as QuickSelectorValue[]
  applyQuickSelectors()
}

function removeQuickSelector(selector: QuickSelectorValue): void {
  const idx = activeQuickSelectors.value.indexOf(selector)
  if (idx >= 0) {
    activeQuickSelectors.value.splice(idx, 1)
    applyQuickSelectors()
  }
}

function applyQuickSelectors(): void {
  if (activeQuickSelectors.value.length === 0) {
    selectedKeyIds.value = []
    return
  }
  const matched = allKeys.value.filter((key) =>
    activeQuickSelectors.value.some((sel) => matchesSelector(key, sel))
  )
  selectedKeyIds.value = matched.map((key) => key.key_id)
}

function formatRelativeTime(value: string): string {
  const ts = new Date(value).getTime()
  if (!Number.isFinite(ts)) return '-'
  const diff = Date.now() - ts
  if (diff < 60_000) return '刚刚'
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}分钟前`
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}小时前`
  return `${Math.floor(diff / 86_400_000)}天前`
}

function shortenQuota(raw: string): string {
  // "周剩余 0.0%（5天3小时后重置）|5H剩余100.0％（5小时0分钟后重置）"
  // -> "周0.0% 5d3h | 5H100.0% 5h"
  return raw.split('|').map((seg) => {
    let s = seg.trim()
    s = s.replace(/剩余\s*/g, '')
    s = s.replace(/％/g, '%')
    s = s.replace(/[（(]\s*(\d+)\s*天\s*(\d+)\s*小时.*?[）)]/g, ' $1d$2h')
    s = s.replace(/[（(]\s*(\d+)\s*小时\s*(\d+)\s*分钟.*?[）)]/g, ' $1h$2m')
    s = s.replace(/[（(]\s*(\d+)\s*小时.*?[）)]/g, ' $1h')
    s = s.replace(/[（(]\s*(\d+)\s*分钟.*?[）)]/g, ' $1m')
    s = s.replace(/[（(]\s*(\d+)\s*天.*?[）)]/g, ' $1d')
    s = s.replace(/[（(].*?[）)]/g, '')
    return s.trim()
  }).join(' | ')
}

async function loadAllKeys(): Promise<void> {
  if (!props.providerId) {
    allKeys.value = []
    selectedKeyIds.value = []
    return
  }
  loading.value = true
  try {
    const pageSize = 200
    let page = 1
    let total = 0
    const collected: PoolKeyDetail[] = []

    while (page <= 50) {
      const res = await listPoolKeys(props.providerId, {
        page,
        page_size: pageSize,
        status: 'all',
      })
      const keys = Array.isArray(res.keys) ? res.keys : []
      collected.push(...keys)
      total = Number(res.total || 0)
      if (keys.length < pageSize || collected.length >= total) break
      page += 1
    }

    allKeys.value = collected
    const validIds = new Set(collected.map((key) => key.key_id))
    selectedKeyIds.value = selectedKeyIds.value.filter((id) => validIds.has(id))
  } catch (err) {
    showError(parseApiError(err, '加载账号列表失败'))
    allKeys.value = []
    selectedKeyIds.value = []
  } finally {
    loading.value = false
  }
}

async function executeAction(): Promise<void> {
  if (executing.value) return
  if (selectedKeyIds.value.length === 0) {
    warning('请先选择账号')
    return
  }

  const selectedMap = new Set(selectedKeyIds.value)
  const selectedKeys = allKeys.value.filter((key) => selectedMap.has(key.key_id))
  if (selectedKeys.length === 0) {
    warning('未找到可执行账号，请刷新列表重试')
    return
  }

  if (selectedAction.value === 'delete') {
    const confirmed = await confirm({
      title: '删除账号',
      message: `将删除 ${selectedKeys.length} 个账号，操作不可恢复，是否继续？`,
      confirmText: '确认删除',
      variant: 'destructive',
    })
    if (!confirmed) return
  }

  if (selectedAction.value === 'set_proxy' && !proxyNodeIdForAction.value) {
    warning('请先选择代理节点')
    return
  }

  executing.value = true
  let successCount = 0
  let failedCount = 0
  let skippedCount = 0

  const actionLabel = ACTION_OPTIONS.find((a) => a.value === selectedAction.value)?.label || '执行'
  progressDone.value = 0
  progressTotal.value = selectedKeys.length
  progressLabel.value = `正在${actionLabel}...`
  lastResultMessage.value = ''

  try {
    if (selectedAction.value === 'refresh_quota') {
      const targetIds = selectedKeys.map((key) => key.key_id)
      const result = await refreshProviderQuota(props.providerId, targetIds)
      successCount = Number(result.success || 0)
      failedCount = Number(result.failed || 0)
      skippedCount = Math.max(0, targetIds.length - Number(result.total || 0))
      progressDone.value = targetIds.length
    } else {
      const CONCURRENCY = props.batchConcurrency || 8
      const taskForKey = (key: PoolKeyDetail): (() => Promise<'success' | 'skip'>) | null => {
        if (selectedAction.value === 'delete') {
          return () => deleteEndpointKey(key.key_id).then(() => 'success' as const)
        }
        if (selectedAction.value === 'refresh_oauth') {
          if (normalizeText(key.auth_type) !== 'oauth') return null
          return () => refreshProviderOAuth(key.key_id).then(() => 'success' as const)
        }
        if (selectedAction.value === 'clear_proxy') {
          return () => updateProviderKey(key.key_id, { proxy: null }).then(() => 'success' as const)
        }
        if (selectedAction.value === 'set_proxy') {
          return () => updateProviderKey(key.key_id, {
            proxy: { node_id: proxyNodeIdForAction.value, enabled: true },
          }).then(() => 'success' as const)
        }
        if (selectedAction.value === 'enable') {
          return () => updateProviderKey(key.key_id, { is_active: true }).then(() => 'success' as const)
        }
        if (selectedAction.value === 'disable') {
          return () => updateProviderKey(key.key_id, { is_active: false }).then(() => 'success' as const)
        }
        return null
      }

      const tasks: Array<() => Promise<'success' | 'skip'>> = []
      for (const key of selectedKeys) {
        const task = taskForKey(key)
        if (task) tasks.push(task)
        else {
          skippedCount += 1
          progressDone.value += 1
        }
      }
      progressTotal.value = selectedKeys.length

      // 并发执行，限制并发数
      let cursor = 0
      const runNext = async (): Promise<void> => {
        while (cursor < tasks.length) {
          const idx = cursor++
          try {
            await tasks[idx]()
            successCount += 1
          } catch {
            failedCount += 1
          }
          progressDone.value += 1
        }
      }
      const workers = Array.from({ length: Math.min(CONCURRENCY, tasks.length) }, () => runNext())
      await Promise.all(workers)
    }

    lastResultMessage.value = `执行完成：成功 ${successCount}，失败 ${failedCount}，跳过 ${skippedCount}`
    if (failedCount > 0) warning(lastResultMessage.value)
    else success(lastResultMessage.value)

    const shouldClearSelection = selectedAction.value === 'delete'
    const previousSelection = new Set(selectedKeyIds.value)
    await loadAllKeys()
    if (shouldClearSelection) {
      selectedKeyIds.value = []
    } else {
      const existingIds = new Set(allKeys.value.map((key) => key.key_id))
      selectedKeyIds.value = [...previousSelection].filter((id) => existingIds.has(id))
    }
    emit('changed')
  } catch (err) {
    showError(parseApiError(err, '批量操作失败'))
  } finally {
    executing.value = false
    progressTotal.value = 0
    progressDone.value = 0
    progressLabel.value = ''
  }
}

watch(
  () => props.modelValue,
  (open) => {
    if (!open) return
    searchText.value = ''
    lastResultMessage.value = ''
    activeQuickSelectors.value = []
    proxyNodesStore.ensureLoaded()
    loadAllKeys()
  },
)

watch(
  () => props.providerId,
  (newId, oldId) => {
    if (!props.modelValue || !newId || newId === oldId) return
    selectedKeyIds.value = []
    loadAllKeys()
  },
)

watch(filteredKeys, () => {
  currentPage.value = 1
})
</script>
