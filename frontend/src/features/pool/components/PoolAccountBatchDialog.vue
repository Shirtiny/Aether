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
          :disabled="loading || executing"
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
          @click="loadKeysPage()"
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
          共 {{ filteredTotal }} 个匹配账号，当前页 {{ pageKeys.length }} 个，已选 {{ selectedCount }} 个
        </div>
        <div class="flex items-center gap-2">
          <Checkbox
            :checked="isAllFilteredSelected"
            :indeterminate="isPartiallyFilteredSelected"
            :disabled="filteredTotal === 0 || loading || executing"
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
          v-else-if="pageKeys.length === 0"
          class="py-10 text-center text-sm text-muted-foreground"
        >
          无匹配账号
        </div>
        <label
          v-for="key in pageKeys"
          :key="key.key_id"
          class="flex items-center gap-2.5 px-3 py-2 border-b last:border-b-0 cursor-pointer hover:bg-muted/30"
        >
          <Checkbox
            :checked="selectAllFiltered || selectedIdSet.has(key.key_id)"
            :disabled="executing || selectAllFiltered"
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
                v-if="getOAuthOrgBadge(key)"
                variant="secondary"
                class="text-[10px] px-1 py-0 h-4 shrink-0"
                :title="getOAuthOrgBadge(key)?.id"
              >{{ getOAuthOrgBadge(key)?.label }}</Badge>
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
            @click="goToPage(1)"
          >
            <ChevronsLeft class="h-3.5 w-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            class="h-7 w-7"
            :disabled="currentPage <= 1"
            @click="goToPage(currentPage - 1)"
          >
            <ChevronLeft class="h-3.5 w-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            class="h-7 w-7"
            :disabled="currentPage >= totalPages"
            @click="goToPage(currentPage + 1)"
          >
            <ChevronRight class="h-3.5 w-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            class="h-7 w-7"
            :disabled="currentPage >= totalPages"
            @click="goToPage(totalPages)"
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
            :disabled="executing || selectedCount === 0 || loading"
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
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { Dialog, Button, Input, Select, SelectTrigger, SelectValue, SelectContent, SelectItem, Checkbox, Badge } from '@/components/ui'
import { MultiSelect } from '@/components/common'
import ProxyNodeSelect from '@/features/providers/components/ProxyNodeSelect.vue'
import { RefreshCw, Play, ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight } from 'lucide-vue-next'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { parseApiError } from '@/utils/errorParser'
import {
  listPoolKeys,
  batchActionPoolKeys,
  getPoolBatchDeleteTask,
  resolvePoolKeySelection,
  type PoolKeyDetail,
  type PoolKeySelectionItem,
} from '@/api/endpoints/pool'
import { exportKey, refreshProviderQuota } from '@/api/endpoints/keys'
import { refreshProviderOAuth } from '@/api/endpoints/provider_oauth'
import { useProxyNodesStore } from '@/stores/proxy-nodes'
import { getOAuthOrgBadge } from '@/utils/oauthIdentity'

type QuickSelectorValue =
  | 'banned'
  | 'no_5h_limit'
  | 'no_weekly_limit'
  | 'plan_free'
  | 'plan_team'
  | 'oauth_invalid'
  | 'proxy_unset'
  | 'proxy_set'
  | 'disabled'
  | 'enabled'

type BatchActionValue =
  | 'export'
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
  providerType?: string
  batchConcurrency?: number | null
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
  changed: []
}>()

const QUICK_SELECT_OPTIONS: Array<{ value: QuickSelectorValue; label: string }> = [
  { value: 'banned', label: '已封号' },
  { value: 'no_5h_limit', label: '无5H限额' },
  { value: 'no_weekly_limit', label: '无周限额' },
  { value: 'plan_free', label: '全部 Free' },
  { value: 'plan_team', label: '全部 Team' },
  { value: 'oauth_invalid', label: 'OAuth 失效' },
  { value: 'proxy_unset', label: '未配置代理' },
  { value: 'proxy_set', label: '已配置独立代理' },
  { value: 'disabled', label: '已禁用' },
  { value: 'enabled', label: '已启用' },
]

const ACTION_OPTIONS: Array<{ value: BatchActionValue; label: string }> = [
  { value: 'export', label: '导出凭据' },
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
const pageKeys = ref<PoolKeyDetail[]>([])
const filteredTotal = ref(0)
const selectedKeyIds = ref<string[]>([])
const knownKeysById = ref<Record<string, PoolKeyDetail>>({})
const selectAllFiltered = ref(false)
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
const SEARCH_DEBOUNCE_MS = 250

let loadRequestId = 0
let searchDebounceTimer: number | null = null
let suppressFilterWatch = false

const dialogDescription = computed(() => {
  const name = (props.providerName || '').trim()
  return name ? `${name} - 选择账号并批量执行动作` : '选择账号并批量执行动作'
})

const selectedIdSet = computed(() => new Set(selectedKeyIds.value))
const selectedCount = computed(() => (selectAllFiltered.value ? filteredTotal.value : selectedKeyIds.value.length))
const totalPages = computed(() => Math.max(1, Math.ceil(filteredTotal.value / PAGE_SIZE)))
const isAllFilteredSelected = computed(() => selectAllFiltered.value && filteredTotal.value > 0)
const isPartiallyFilteredSelected = computed(() => !selectAllFiltered.value && selectedKeyIds.value.length > 0)

function normalizeText(value: unknown): string {
  return String(value || '').trim().toLowerCase()
}

function sanitizeFileNamePart(value: unknown, fallback: string): string {
  const sanitized = String(value || '')
    .trim()
    .replace(/[^a-zA-Z0-9_\-@.]/g, '_')
    .replace(/_+/g, '_')
    .replace(/^_+|_+$/g, '')
  return sanitized || fallback
}

function formatExportTimestamp(date: Date = new Date()): string {
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}_${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`
}

function getBatchExportFilename(): string {
  const providerType = sanitizeFileNamePart(props.providerType || 'pool', 'pool')
  const providerName = sanitizeFileNamePart(props.providerName || props.providerId.slice(0, 8), 'provider')
  return `aether_${providerType}_${providerName}_batch_export_${formatExportTimestamp()}.json`
}

function downloadJsonFile(data: unknown, filename: string): void {
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = filename
  document.body.appendChild(link)
  link.click()
  document.body.removeChild(link)
  URL.revokeObjectURL(url)
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

function isOAuthInvalid(key: PoolKeyDetail): boolean {
  if (normalizeText(key.auth_type) !== 'oauth') return false
  if (key.oauth_invalid_at != null || normalizeText(key.oauth_invalid_reason)) return true
  if (typeof key.oauth_expires_at === 'number' && key.oauth_expires_at > 0) {
    return key.oauth_expires_at * 1000 <= Date.now()
  }
  return false
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
  return raw.split('|').map((segment) => {
    let value = segment.trim()
    value = value.replace(/剩余\s*/g, '')
    value = value.replace(/％/g, '%')
    value = value.replace(/[（(]\s*(\d+)\s*天\s*(\d+)\s*小时.*?[）)]/g, ' $1d$2h')
    value = value.replace(/[（(]\s*(\d+)\s*小时\s*(\d+)\s*分钟.*?[）)]/g, ' $1h$2m')
    value = value.replace(/[（(]\s*(\d+)\s*小时.*?[）)]/g, ' $1h')
    value = value.replace(/[（(]\s*(\d+)\s*分钟.*?[）)]/g, ' $1m')
    value = value.replace(/[（(]\s*(\d+)\s*天.*?[）)]/g, ' $1d')
    value = value.replace(/[（(].*?[）)]/g, '')
    return value.trim()
  }).join(' | ')
}

function clearSearchDebounce(): void {
  if (searchDebounceTimer !== null) {
    clearTimeout(searchDebounceTimer)
    searchDebounceTimer = null
  }
}

function rememberPageKeys(keys: PoolKeyDetail[]): void {
  if (keys.length === 0) return
  const next = { ...knownKeysById.value }
  for (const key of keys) {
    next[key.key_id] = key
  }
  knownKeysById.value = next
}

function resetSelection(clearKnown = false): void {
  selectAllFiltered.value = false
  selectedKeyIds.value = []
  if (clearKnown) knownKeysById.value = {}
}

function buildSelectionFilters(): { search?: string; quick_selectors?: string[] } {
  const search = searchText.value.trim()
  const quickSelectors = activeQuickSelectors.value.map((value) => String(value))
  return {
    ...(search ? { search } : {}),
    ...(quickSelectors.length > 0 ? { quick_selectors: quickSelectors } : {}),
  }
}

async function loadKeysPage(): Promise<void> {
  if (!props.providerId) {
    pageKeys.value = []
    filteredTotal.value = 0
    resetSelection(true)
    return
  }

  const requestId = ++loadRequestId
  loading.value = true
  const startedAt = performance.now()
  let ok = false
  try {
    const res = await listPoolKeys(props.providerId, {
      page: currentPage.value,
      page_size: PAGE_SIZE,
      status: 'all',
      search: searchText.value.trim() || undefined,
      quick_selectors: activeQuickSelectors.value,
      search_scope: 'full',
    })
    if (requestId !== loadRequestId) return

    pageKeys.value = Array.isArray(res.keys) ? res.keys : []
    filteredTotal.value = Number(res.total || 0)
    rememberPageKeys(pageKeys.value)
    ok = true
  } catch (err) {
    if (requestId !== loadRequestId) return
    pageKeys.value = []
    filteredTotal.value = 0
    showError(parseApiError(err, '加载账号列表失败'))
  } finally {
    if (requestId === loadRequestId) {
      loading.value = false
      // eslint-disable-next-line no-console
      console.info('[PoolAccountBatchDialog] loadKeysPage timing', {
        providerId: props.providerId,
        page: currentPage.value,
        pageSize: PAGE_SIZE,
        search: searchText.value.trim(),
        quickSelectors: activeQuickSelectors.value,
        total: filteredTotal.value,
        count: pageKeys.value.length,
        ok,
        durationMs: Math.round(performance.now() - startedAt),
      })
    }
  }
}

function requestFilteredReload(debounceMs = 0): void {
  if (!props.modelValue) return
  clearSearchDebounce()
  resetSelection()
  lastResultMessage.value = ''
  const run = () => {
    searchDebounceTimer = null
    currentPage.value = 1
    void loadKeysPage()
  }
  if (debounceMs > 0) {
    searchDebounceTimer = window.setTimeout(run, debounceMs)
  } else {
    run()
  }
}

async function goToPage(page: number): Promise<void> {
  const nextPage = Math.min(Math.max(1, page), totalPages.value)
  currentPage.value = nextPage
  await loadKeysPage()
}

function toggleOne(keyId: string, checked: boolean): void {
  const set = new Set(selectedKeyIds.value)
  if (checked) set.add(keyId)
  else set.delete(keyId)
  selectedKeyIds.value = [...set]
}

function toggleSelectFiltered(checked: boolean | 'indeterminate'): void {
  selectAllFiltered.value = checked === true
  if (selectAllFiltered.value) {
    selectedKeyIds.value = []
  }
}

function onQuickSelectChange(values: string[]): void {
  activeQuickSelectors.value = values as QuickSelectorValue[]
  requestFilteredReload()
}

function removeQuickSelector(selector: QuickSelectorValue): void {
  const idx = activeQuickSelectors.value.indexOf(selector)
  if (idx >= 0) {
    activeQuickSelectors.value.splice(idx, 1)
    requestFilteredReload()
  }
}

const DELETE_POLL_INTERVAL_MS = 2000
const DELETE_POLL_MAX_MS = 10 * 60 * 1000
const DELETE_POLL_MAX_FAILURES = 3

async function pollDeleteTask(
  providerId: string,
  taskId: string,
  progressOffset: number,
): Promise<{ status: string; deleted: number }> {
  const deadline = Date.now() + DELETE_POLL_MAX_MS
  let consecutiveFailures = 0
  while (Date.now() < deadline) {
    try {
      const task = await getPoolBatchDeleteTask(providerId, taskId)
      consecutiveFailures = 0
      progressDone.value = progressOffset + task.deleted
      if (task.status === 'completed' || task.status === 'failed') {
        return { status: task.status, deleted: task.deleted }
      }
    } catch {
      consecutiveFailures++
      if (consecutiveFailures >= DELETE_POLL_MAX_FAILURES) {
        return { status: 'failed', deleted: 0 }
      }
    }
    await new Promise((resolve) => setTimeout(resolve, DELETE_POLL_INTERVAL_MS))
  }
  return { status: 'failed', deleted: 0 }
}

async function resolveSelectedItems(): Promise<PoolKeySelectionItem[]> {
  if (!props.providerId) return []

  if (selectAllFiltered.value) {
    progressLabel.value = '正在解析筛选结果...'
    const result = await resolvePoolKeySelection(props.providerId, buildSelectionFilters())
    return Array.isArray(result.items) ? result.items : []
  }

  return selectedKeyIds.value.map((keyId) => {
    const key = knownKeysById.value[keyId]
    return {
      key_id: keyId,
      key_name: key?.key_name || '',
      auth_type: key?.auth_type || 'api_key',
    }
  })
}

async function executeAction(): Promise<void> {
  if (executing.value) return
  if (selectedCount.value === 0) {
    warning('请先选择账号')
    return
  }

  const requestedCount = selectedCount.value
  if (selectedAction.value === 'delete') {
    const confirmed = await confirm({
      title: '删除账号',
      message: `将删除 ${requestedCount} 个账号，操作不可恢复，是否继续？`,
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
  let resolvedCount = 0
  const actionStartedAt = performance.now()
  let actionPhaseMs = 0
  let reloadPhaseMs = 0

  const actionLabel = ACTION_OPTIONS.find((item) => item.value === selectedAction.value)?.label || '执行'
  progressDone.value = 0
  progressTotal.value = 0
  progressLabel.value = selectAllFiltered.value ? '正在解析筛选结果...' : `正在${actionLabel}...`
  lastResultMessage.value = ''

  try {
    const selectedKeys = await resolveSelectedItems()
    resolvedCount = selectedKeys.length
    if (selectedKeys.length === 0) {
      warning('未找到可执行账号，请刷新列表重试')
      return
    }

    progressDone.value = 0
    progressTotal.value = selectedKeys.length
    progressLabel.value = `正在${actionLabel}...`

    if (selectedAction.value === 'refresh_quota') {
      const targetIds = selectedKeys.map((key) => key.key_id)
      const BATCH_SIZE = 20
      const totalBatches = Math.ceil(targetIds.length / BATCH_SIZE)

      for (let i = 0; i < targetIds.length; i += BATCH_SIZE) {
        const batchIndex = Math.floor(i / BATCH_SIZE) + 1
        const batch = targetIds.slice(i, i + BATCH_SIZE)
        progressLabel.value = `正在${actionLabel}...（第 ${batchIndex}/${totalBatches} 批）`

        try {
          const result = await refreshProviderQuota(props.providerId, batch)
          successCount += Number(result.success || 0)
          failedCount += Number(result.failed || 0)
          skippedCount += Math.max(0, batch.length - Number(result.total || 0))
        } catch {
          failedCount += batch.length
        }

        progressDone.value = Math.min(i + BATCH_SIZE, targetIds.length)
      }
    } else if (selectedAction.value === 'export') {
      const exportableKeys = selectedKeys.filter((key) => normalizeText(key.auth_type) === 'oauth')
      const exportedEntries: Array<Record<string, unknown> | null> = Array.from({ length: exportableKeys.length }, () => null)

      skippedCount += selectedKeys.length - exportableKeys.length
      progressDone.value = 0
      progressTotal.value = exportableKeys.length
      if (skippedCount > 0) {
        progressLabel.value = `正在${actionLabel}...（跳过 ${skippedCount} 个非 OAuth 账号）`
      }

      let cursor = 0
      const CONCURRENCY = props.batchConcurrency || 8
      const runNext = async (): Promise<void> => {
        while (cursor < exportableKeys.length) {
          const idx = cursor++
          const key = exportableKeys[idx]
          try {
            exportedEntries[idx] = await exportKey(key.key_id)
            successCount += 1
          } catch (err) {
            failedCount += 1
            // eslint-disable-next-line no-console
            console.error(`[PoolAccountBatchDialog] export failed (${key.key_id}):`, err)
          } finally {
            progressDone.value += 1
          }
        }
      }

      const workers = Array.from(
        { length: Math.min(CONCURRENCY, exportableKeys.length) },
        () => runNext(),
      )
      await Promise.all(workers)

      const exportedData = exportedEntries.filter((item): item is Record<string, unknown> => item !== null)
      if (exportedData.length > 0) {
        downloadJsonFile(exportedData, getBatchExportFilename())
      }
    } else if (selectedAction.value === 'delete') {
      const targetIds = selectedKeys.map((key) => key.key_id)
      const BATCH_SIZE = 2000
      const totalBatches = Math.ceil(targetIds.length / BATCH_SIZE)

      for (let i = 0; i < targetIds.length; i += BATCH_SIZE) {
        const batchIndex = Math.floor(i / BATCH_SIZE) + 1
        const batch = targetIds.slice(i, i + BATCH_SIZE)
        if (totalBatches > 1) {
          progressLabel.value = `正在${actionLabel}...（第 ${batchIndex}/${totalBatches} 批）`
        }

        try {
          const result = await batchActionPoolKeys(props.providerId, {
            key_ids: batch,
            action: 'delete',
          })

          if (result.task_id) {
            progressLabel.value = `正在${actionLabel}...（后台执行中）`
            const taskResult = await pollDeleteTask(props.providerId, result.task_id, i)
            successCount += taskResult.deleted
            if (taskResult.status === 'failed') {
              failedCount += batch.length - taskResult.deleted
            }
          } else {
            successCount += result.affected
          }
        } catch (err) {
          // eslint-disable-next-line no-console
          console.error(`batch delete failed (batch ${batchIndex}/${totalBatches}):`, err)
          failedCount += batch.length
        }

        progressDone.value = Math.min(i + BATCH_SIZE, targetIds.length)
      }
    } else if (['enable', 'disable', 'clear_proxy', 'set_proxy'].includes(selectedAction.value)) {
      const targetIds = selectedKeys.map((key) => key.key_id)
      const BATCH_SIZE = 2000
      const totalBatches = Math.ceil(targetIds.length / BATCH_SIZE)

      for (let i = 0; i < targetIds.length; i += BATCH_SIZE) {
        const batchIndex = Math.floor(i / BATCH_SIZE) + 1
        const batch = targetIds.slice(i, i + BATCH_SIZE)
        if (totalBatches > 1) {
          progressLabel.value = `正在${actionLabel}...（第 ${batchIndex}/${totalBatches} 批）`
        }

        const payload = selectedAction.value === 'set_proxy'
          ? { node_id: proxyNodeIdForAction.value, enabled: true }
          : undefined

        try {
          const result = await batchActionPoolKeys(props.providerId, {
            key_ids: batch,
            action: selectedAction.value as 'enable' | 'disable' | 'clear_proxy' | 'set_proxy',
            ...(payload ? { payload } : {}),
          })
          successCount += result.affected
        } catch (err) {
          // eslint-disable-next-line no-console
          console.error(`batch ${selectedAction.value} failed (batch ${batchIndex}/${totalBatches}):`, err)
          failedCount += batch.length
        }

        progressDone.value = Math.min(i + BATCH_SIZE, targetIds.length)
      }
    } else {
      const CONCURRENCY = props.batchConcurrency || 8
      const tasks: Array<() => Promise<'success' | 'skip'>> = []
      for (const key of selectedKeys) {
        if (selectedAction.value === 'refresh_oauth' && normalizeText(key.auth_type) !== 'oauth') {
          skippedCount += 1
          progressDone.value += 1
          continue
        }
        tasks.push(() => refreshProviderOAuth(key.key_id).then(() => 'success' as const))
      }
      progressTotal.value = selectedKeys.length

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
    if (failedCount > 0 || (selectedAction.value === 'export' && successCount === 0)) warning(lastResultMessage.value)
    else success(lastResultMessage.value)

    actionPhaseMs = performance.now() - actionStartedAt
    if (selectedAction.value !== 'export') {
      const reloadStartedAt = performance.now()
      if (selectedAction.value === 'delete' && successCount > 0) {
        resetSelection(true)
      }
      await loadKeysPage()
      if (pageKeys.value.length === 0 && filteredTotal.value > 0 && currentPage.value > totalPages.value) {
        await goToPage(totalPages.value)
      }
      reloadPhaseMs = performance.now() - reloadStartedAt
      emit('changed')
    }
  } catch (err) {
    showError(parseApiError(err, '批量操作失败'))
  } finally {
    // eslint-disable-next-line no-console
    console.info('[PoolAccountBatchDialog] executeAction timing', {
      providerId: props.providerId,
      action: selectedAction.value,
      requestedCount,
      resolvedCount,
      successCount,
      failedCount,
      skippedCount,
      actionPhaseMs: Math.round(actionPhaseMs),
      reloadPhaseMs: Math.round(reloadPhaseMs),
      totalMs: Math.round(performance.now() - actionStartedAt),
    })
    executing.value = false
    progressTotal.value = 0
    progressDone.value = 0
    progressLabel.value = ''
  }
}

watch(searchText, () => {
  if (suppressFilterWatch || !props.modelValue) return
  requestFilteredReload(SEARCH_DEBOUNCE_MS)
})

watch(
  () => props.modelValue,
  (open) => {
    if (!open) {
      clearSearchDebounce()
      return
    }
    suppressFilterWatch = true
    searchText.value = ''
    lastResultMessage.value = ''
    activeQuickSelectors.value = []
    resetSelection(true)
    filteredTotal.value = 0
    pageKeys.value = []
    currentPage.value = 1
    suppressFilterWatch = false
    proxyNodesStore.ensureLoaded()
    void loadKeysPage()
  },
)

watch(
  () => props.providerId,
  (newId, oldId) => {
    if (!props.modelValue || !newId || newId === oldId) return
    clearSearchDebounce()
    suppressFilterWatch = true
    resetSelection(true)
    filteredTotal.value = 0
    pageKeys.value = []
    currentPage.value = 1
    suppressFilterWatch = false
    void loadKeysPage()
  },
)

onBeforeUnmount(() => {
  clearSearchDebounce()
})
</script>
