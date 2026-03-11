<template>
  <div class="space-y-6 pb-8">
    <!-- 活跃度热图 + 请求间隔时间线 -->
    <div class="grid grid-cols-1 xl:grid-cols-2 gap-4">
      <ActivityHeatmapCard
        :data="activityHeatmapData"
        :title="isAdminPage ? '总体活跃天数' : '我的活跃天数'"
        :is-loading="isLoadingHeatmap"
        :has-error="heatmapError"
      />
      <IntervalTimelineCard
        :title="isAdminPage ? '请求间隔时间线' : '我的请求间隔'"
        :is-admin="isAdminPage"
        :hours="24"
      />
    </div>

    <!-- 分析统计 -->
    <!-- 管理员：模型 + 提供商 + API格式（3列） -->
    <div
      v-if="isAdminPage"
      class="grid grid-cols-1 lg:grid-cols-3 gap-4"
    >
      <UsageModelTable
        :data="enhancedModelStats"
        :is-admin="authStore.isAdmin"
      />
      <UsageProviderTable
        :data="providerStats"
        :is-admin="authStore.isAdmin"
      />
      <UsageApiFormatTable
        :data="apiFormatStats"
        :is-admin="authStore.isAdmin"
      />
    </div>
    <!-- 用户：模型 + API格式（2列） -->
    <div
      v-else
      class="grid grid-cols-1 lg:grid-cols-2 gap-4"
    >
      <UsageModelTable
        :data="enhancedModelStats"
        :is-admin="authStore.isAdmin"
      />
      <UsageApiFormatTable
        :data="apiFormatStats"
        :is-admin="false"
      />
    </div>

    <!-- 请求详情 -->
    <UsageRecordsTable
      :records="displayRecords"
      :is-admin="isAdminPage"
      :show-actual-cost="authStore.isAdmin"
      :loading="isLoadingRecords"
      :time-range="timeRange"
      :filter-search="filterSearch"
      :filter-user="filterUser"
      :filter-model="filterModel"
      :filter-provider="filterProvider"
      :filter-api-format="filterApiFormat"
      :filter-status="filterStatus"
      :available-users="availableUsers"
      :available-models="availableModels"
      :available-providers="availableProviders"
      :current-page="currentPage"
      :page-size="pageSize"
      :total-records="effectiveTotalRecords"
      :page-size-options="pageSizeOptions"
      :auto-refresh="globalAutoRefresh"
      @update:time-range="handleTimeRangeChange"
      @update:filter-search="handleFilterSearchChange"
      @update:filter-user="handleFilterUserChange"
      @update:filter-model="handleFilterModelChange"
      @update:filter-provider="handleFilterProviderChange"
      @update:filter-api-format="handleFilterApiFormatChange"
      @update:filter-status="handleFilterStatusChange"
      @update:current-page="handlePageChange"
      @update:page-size="handlePageSizeChange"
      @update:auto-refresh="handleAutoRefreshChange"
      @refresh="refreshData"
      @show-detail="showRequestDetail"
    />

    <!-- 请求详情抽屉 - 仅管理员可见 -->
    <RequestDetailDrawer
      v-if="isAdminPage"
      :is-open="detailModalOpen"
      :request-id="selectedRequestId"
      @close="detailModalOpen = false"
    />
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useRoute } from 'vue-router'
import { useAuthStore } from '@/stores/auth'
import { usageApi } from '@/api/usage'
import { usersApi } from '@/api/users'
import { meApi } from '@/api/me'
import {
  UsageModelTable,
  UsageProviderTable,
  UsageApiFormatTable,
  UsageRecordsTable,
  ActivityHeatmapCard,
  RequestDetailDrawer,
  IntervalTimelineCard
} from '@/features/usage/components'
import {
  useUsageData,
  getDateRangeFromPeriod
} from '@/features/usage/composables'
import type { DateRangeParams, FilterStatusValue } from '@/features/usage/types'
import type { UserOption } from '@/features/usage/components/UsageRecordsTable.vue'
import { log } from '@/utils/logger'
import type { ActivityHeatmap } from '@/types/activity'
import { useToast } from '@/composables/useToast'

const route = useRoute()
const { warning } = useToast()
const authStore = useAuthStore()

// 判断是否是管理员页面
const isAdminPage = computed(() => route.path.startsWith('/admin'))

// 时间范围选择
const timeRange = ref<DateRangeParams>(getDateRangeFromPeriod('today'))

// 分页状态
const currentPage = ref(1)
const pageSize = ref(20)
const pageSizeOptions = [10, 20, 50, 100]

// 筛选状态
const filterSearch = ref('')
const filterUser = ref('__all__')
const filterModel = ref('__all__')
const filterProvider = ref('__all__')
const filterApiFormat = ref('__all__')
const filterStatus = ref<FilterStatusValue>('__all__')

// 用户列表（仅管理员页面使用）
const availableUsers = ref<UserOption[]>([])

// 使用 composables
const {
  isLoadingRecords,
  providerStats,
  apiFormatStats,
  currentRecords,
  totalRecords,
  enhancedModelStats,
  availableModels,
  availableProviders,
  loadStats,
  loadRecords
} = useUsageData({ isAdminPage })

// 热力图状态
const activityHeatmapData = ref<ActivityHeatmap | null>(null)
const isLoadingHeatmap = ref(false)
const heatmapError = ref(false)

// 加载热力图数据
async function loadHeatmapData() {
  isLoadingHeatmap.value = true
  heatmapError.value = false
  try {
    if (isAdminPage.value) {
      activityHeatmapData.value = await usageApi.getActivityHeatmap()
    } else {
      activityHeatmapData.value = await meApi.getActivityHeatmap()
    }
  } catch (error) {
    log.error('加载热力图数据失败:', error)
    heatmapError.value = true
  } finally {
    isLoadingHeatmap.value = false
  }
}

// 用户页面需要前端筛选
const filteredRecords = computed(() => {
  if (!isAdminPage.value) {
    let records = [...currentRecords.value]

    if (filterModel.value !== '__all__') {
      records = records.filter(record => record.model === filterModel.value)
    }

    if (filterProvider.value !== '__all__') {
      records = records.filter(record => record.provider === filterProvider.value)
    }

    if (filterApiFormat.value !== '__all__') {
      records = records.filter(record =>
        record.api_format?.toUpperCase() === filterApiFormat.value.toUpperCase()
      )
    }

    if (filterStatus.value !== '__all__') {
      if (filterStatus.value === 'stream') {
        records = records.filter(record =>
          record.is_stream && !record.error_message && (!record.status_code || record.status_code === 200)
        )
      } else if (filterStatus.value === 'standard') {
        records = records.filter(record =>
          !record.is_stream && !record.error_message && (!record.status_code || record.status_code === 200)
        )
      } else if (filterStatus.value === 'active') {
        records = records.filter(record =>
          record.status === 'pending' || record.status === 'streaming'
        )
      } else if (filterStatus.value === 'failed') {
        // 失败请求需要同时考虑新旧两种判断方式：
        // 1. 新方式：status = "failed"
        // 2. 旧方式：status_code >= 400 或 error_message 不为空
        records = records.filter(record =>
          record.status === 'failed' ||
          (record.status_code && record.status_code >= 400) ||
          record.error_message
        )
      } else if (filterStatus.value === 'cancelled') {
        records = records.filter(record => record.status === 'cancelled')
      }
    }

    return records
  }
  return currentRecords.value
})

// 获取活跃请求的 ID 列表
const activeRequestIds = computed(() => {
  return currentRecords.value
    .filter(record => record.status === 'pending' || record.status === 'streaming')
    .map(record => record.id)
})

// 检查是否有活跃请求
const hasActiveRequests = computed(() => activeRequestIds.value.length > 0)

// 自动刷新定时器
let autoRefreshTimer: ReturnType<typeof setTimeout> | null = null
let globalAutoRefreshTimer: ReturnType<typeof setInterval> | null = null
let refreshInFlight: Promise<void> | null = null
const AUTO_REFRESH_INTERVAL = 1000 // 1秒刷新一次（用于活跃请求）
const GLOBAL_AUTO_REFRESH_INTERVAL = 3000 // 3秒刷新一次（全局自动刷新）
const globalAutoRefresh = ref(false) // 全局自动刷新开关（默认关闭）
const isPageVisible = ref(typeof document === 'undefined' ? true : !document.hidden)

// 轮询活跃请求状态（轻量级，只更新状态变化的记录）

let pollInFlight = false
async function pollActiveRequests() {
  if (!isPageVisible.value) return
  if (!hasActiveRequests.value) return
  if (pollInFlight) return
  pollInFlight = true

  try {
    // 根据页面类型选择不同的 API
    const idsParam = activeRequestIds.value.join(',')
    const { requests } = isAdminPage.value
      ? await usageApi.getActiveRequests(activeRequestIds.value)
      : await meApi.getActiveRequests(idsParam)

    let shouldRefresh = false

    const recordMap = new Map(currentRecords.value.map(record => [record.id, record]))

    for (const update of requests) {
      const record = recordMap.get(update.id)
      if (!record) {
        // 后端返回了未知的活跃请求，触发刷新以获取完整数据
        shouldRefresh = true
        continue
      }

      // 状态只允许单向推进，避免异步响应回退（pending -> streaming -> completed/failed/cancelled）
      const statusPriority: Record<string, number> = {
        pending: 0,
        streaming: 1,
        completed: 2,
        failed: 2,
        cancelled: 2
      }
      const currentRank = record.status ? (statusPriority[record.status] ?? 0) : 0
      const newRank = update.status ? (statusPriority[update.status] ?? 0) : 0
      const shouldApply = newRank >= currentRank

      if (shouldApply && record.status !== update.status) {
        record.status = update.status
      }
      if (shouldApply && ['completed', 'failed', 'cancelled'].includes(update.status)) {
        shouldRefresh = true
      }

      if (shouldApply) {
        // 进行中状态也需要持续更新（provider/key/TTFB 可能在 streaming 后才落库）
        record.input_tokens = update.input_tokens
        record.output_tokens = update.output_tokens
        record.cache_creation_input_tokens = update.cache_creation_input_tokens ?? undefined
        record.cache_read_input_tokens = update.cache_read_input_tokens ?? undefined
        record.cost = update.cost
        record.actual_cost = update.actual_cost ?? undefined
        record.rate_multiplier = update.rate_multiplier ?? undefined
        record.response_time_ms = update.response_time_ms ?? undefined
        record.first_byte_time_ms = update.first_byte_time_ms ?? undefined
        // API 格式/格式转换：streaming 时已可确定，轮询时同步更新
        if (update.api_format != null) record.api_format = update.api_format
        if (update.endpoint_api_format != null) record.endpoint_api_format = update.endpoint_api_format
        if (update.has_format_conversion != null) record.has_format_conversion = update.has_format_conversion
        // 模型映射：streaming 时已可确定
        if ('target_model' in update && (typeof update.target_model === 'string' || update.target_model === null)) {
          record.target_model = update.target_model
        }
        // 管理员接口返回额外字段
        // 只有当返回的 provider 不是 pending/unknown 时才更新，避免覆盖已有的正确值
        if ('provider' in update && typeof update.provider === 'string' &&
            update.provider !== 'pending' && update.provider !== 'unknown') {
          record.provider = update.provider
        }
        if ('api_key_name' in update) {
          record.api_key_name = typeof update.api_key_name === 'string' ? update.api_key_name : undefined
        }
      }
    }

    if (shouldRefresh) {
      await refreshData()
    }
  } catch (error) {
    log.error('轮询活跃请求状态失败:', error)
  } finally {
    pollInFlight = false
  }
}

function scheduleNextAutoRefresh() {
  if (autoRefreshTimer) return
  if (!isPageVisible.value || !hasActiveRequests.value) return
  autoRefreshTimer = setTimeout(async () => {
    autoRefreshTimer = null
    await pollActiveRequests()
    scheduleNextAutoRefresh()
  }, AUTO_REFRESH_INTERVAL)
}

// 启动自动刷新
function startAutoRefresh() {
  if (!isPageVisible.value) return
  scheduleNextAutoRefresh()
}

// 停止自动刷新
function stopAutoRefresh() {
  if (autoRefreshTimer) {
    clearTimeout(autoRefreshTimer)
    autoRefreshTimer = null
  }
}

// 监听活跃请求状态，自动启动/停止刷新
// 1秒轮询始终用于活跃请求的实时更新，不受全局刷新影响
watch(hasActiveRequests, (hasActive) => {
  if (hasActive && isPageVisible.value) {
    startAutoRefresh()
  } else {
    stopAutoRefresh()
  }
}, { immediate: true })

// 启动全局自动刷新
function startGlobalAutoRefresh() {
  if (!isPageVisible.value) return
  if (globalAutoRefreshTimer) return
  globalAutoRefreshTimer = setInterval(refreshData, GLOBAL_AUTO_REFRESH_INTERVAL)
}

// 停止全局自动刷新
function stopGlobalAutoRefresh() {
  if (globalAutoRefreshTimer) {
    clearInterval(globalAutoRefreshTimer)
    globalAutoRefreshTimer = null
  }
}

// 处理自动刷新开关变化
function handleAutoRefreshChange(value: boolean) {
  globalAutoRefresh.value = value
  if (value) {
    if (isPageVisible.value) {
      refreshData() // 立即刷新一次
    }
    startGlobalAutoRefresh()
  } else {
    stopGlobalAutoRefresh()
  }
}

function handleVisibilityChange() {
  isPageVisible.value = !document.hidden
  if (!isPageVisible.value) {
    stopAutoRefresh()
    stopGlobalAutoRefresh()
    return
  }
  if (hasActiveRequests.value) {
    startAutoRefresh()
  }
  if (globalAutoRefresh.value) {
    refreshData()
    startGlobalAutoRefresh()
  }
}

// 组件卸载时清理定时器
onUnmounted(() => {
  document.removeEventListener('visibilitychange', handleVisibilityChange)
  stopAutoRefresh()
  stopGlobalAutoRefresh()
})

// 用户页面的前端分页（后端一次性返回所有记录，前端分页+筛选）
const paginatedRecords = computed(() => {
  if (!isAdminPage.value) {
    const start = (currentPage.value - 1) * pageSize.value
    const end = start + pageSize.value
    return filteredRecords.value.slice(start, end)
  }
  return currentRecords.value
})

// 用户页面使用前端筛选后的总数，管理员页面使用后端返回的总数
const effectiveTotalRecords = computed(() => {
  if (!isAdminPage.value) {
    return filteredRecords.value.length
  }
  return totalRecords.value
})

// 显示的记录
const displayRecords = computed(() => paginatedRecords.value)


// 详情弹窗状态
const detailModalOpen = ref(false)
const selectedRequestId = ref<string | null>(null)

// 初始化加载
onMounted(async () => {
  document.addEventListener('visibilitychange', handleVisibilityChange)

  // 所有数据源并行加载（stats/heatmap/records/users 之间没有数据依赖）
  const statsTask = loadStats(timeRange.value).catch(err => {
    log.error('加载统计数据失败:', err)
    warning('统计数据加载失败，请刷新重试')
  })
  const heatmapTask = loadHeatmapData().catch(err => {
    log.error('加载热力图数据失败:', err)
  })

  const tasks: Promise<unknown>[] = [statsTask, heatmapTask]

  if (isAdminPage.value) {
    // 管理员页面：stats 和 records 分开加载（后端分页）
    tasks.push(loadRecords(
      { page: currentPage.value, pageSize: pageSize.value },
      getCurrentFilters()
    ))
    tasks.push(
      usersApi.getAllUsers().then(users => {
        availableUsers.value = users.map(u => ({ id: u.id, username: u.username, email: u.email }))
      })
    )
  }
  // 用户页面：loadStats 已包含记录加载，不需要单独调用 loadRecords

  await Promise.allSettled(tasks)

  if (globalAutoRefresh.value && isPageVisible.value) {
    startGlobalAutoRefresh()
  }
})

// 处理时间范围变化
async function handleTimeRangeChange(value: DateRangeParams) {
  timeRange.value = value
  currentPage.value = 1 // 重置到第一页
  await loadStats(timeRange.value)
  if (isAdminPage.value) {
    await loadRecords({ page: 1, pageSize: pageSize.value }, getCurrentFilters())
  }
  // 用户页面：loadStats 已包含记录加载
}

// 处理分页变化
async function handlePageChange(page: number) {
  currentPage.value = page
  if (isAdminPage.value) {
    await loadRecords({ page, pageSize: pageSize.value }, getCurrentFilters())
  }
  // 用户页面使用前端分页，无需重新请求
}

// 处理每页大小变化
async function handlePageSizeChange(size: number) {
  pageSize.value = size
  currentPage.value = 1  // 重置到第一页
  if (isAdminPage.value) {
    await loadRecords({ page: 1, pageSize: size }, getCurrentFilters())
  }
  // 用户页面使用前端分页，无需重新请求
}

// 获取当前筛选参数
function getCurrentFilters() {
  return {
    search: filterSearch.value.trim() || undefined,
    user_id: filterUser.value !== '__all__' ? filterUser.value : undefined,
    model: filterModel.value !== '__all__' ? filterModel.value : undefined,
    provider: filterProvider.value !== '__all__' ? filterProvider.value : undefined,
    api_format: filterApiFormat.value !== '__all__' ? filterApiFormat.value : undefined,
    status: filterStatus.value !== '__all__' ? filterStatus.value : undefined
  }
}

// 处理筛选变化
async function handleFilterSearchChange(value: string) {
  filterSearch.value = value
  currentPage.value = 1

  if (isAdminPage.value) {
    await loadRecords({ page: 1, pageSize: pageSize.value }, getCurrentFilters())
  }
  // 用户页面：search 需要重新从后端拉取数据（后端支持 search 参数）
  // 但通过 filteredRecords 做前端过滤已覆盖，无需额外请求
}

async function handleFilterUserChange(value: string) {
  filterUser.value = value
  currentPage.value = 1  // 重置到第一页

  if (isAdminPage.value) {
    await loadRecords({ page: 1, pageSize: pageSize.value }, getCurrentFilters())
  }
}

async function handleFilterModelChange(value: string) {
  filterModel.value = value
  currentPage.value = 1  // 重置到第一页

  if (isAdminPage.value) {
    await loadRecords({ page: 1, pageSize: pageSize.value }, getCurrentFilters())
  }
}

async function handleFilterProviderChange(value: string) {
  filterProvider.value = value
  currentPage.value = 1

  if (isAdminPage.value) {
    await loadRecords({ page: 1, pageSize: pageSize.value }, getCurrentFilters())
  }
}

async function handleFilterApiFormatChange(value: string) {
  filterApiFormat.value = value
  currentPage.value = 1

  if (isAdminPage.value) {
    await loadRecords({ page: 1, pageSize: pageSize.value }, getCurrentFilters())
  }
}

async function handleFilterStatusChange(value: string) {
  filterStatus.value = value as FilterStatusValue
  currentPage.value = 1

  if (isAdminPage.value) {
    await loadRecords({ page: 1, pageSize: pageSize.value }, getCurrentFilters())
  }
}

// 刷新数据
async function refreshData() {
  if (!isPageVisible.value) return
  if (refreshInFlight) return refreshInFlight

  refreshInFlight = (async () => {
    if (isAdminPage.value) {
      // loadStats 会同步更新 currentDateRange，随后 loadRecords 复用同一时间范围
      await Promise.all([
        loadStats(timeRange.value),
        loadRecords({ page: currentPage.value, pageSize: pageSize.value }, getCurrentFilters())
      ])
      return
    }

    await loadStats(timeRange.value)
    // 用户页面：loadStats 已包含记录加载
  })()

  try {
    await refreshInFlight
  } finally {
    refreshInFlight = null
  }
}

// 显示请求详情
function showRequestDetail(id: string) {
  if (!isAdminPage.value) return
  selectedRequestId.value = id
  detailModalOpen.value = true
}

</script>

<style scoped>
</style>
