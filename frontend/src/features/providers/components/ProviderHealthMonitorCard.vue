<template>
  <Card
    variant="default"
    class="overflow-hidden"
  >
    <HealthMonitorHeader
      v-model:lookback-hours="lookbackHours"
      :title="title"
      description="仅展示活跃提供商，展开后查看该提供商下的模型健康明细"
      :loading="loading"
      @refresh="refreshData"
    />

    <div class="p-6">
      <div
        v-if="loadingMonitors"
        class="flex items-center justify-center py-12"
      >
        <Loader2 class="w-6 h-6 animate-spin text-muted-foreground" />
        <span class="ml-2 text-muted-foreground">加载中...</span>
      </div>

      <div
        v-else-if="providers.length === 0"
        class="flex flex-col items-center justify-center py-12 text-muted-foreground"
      >
        <Server class="w-12 h-12 mb-3 opacity-30" />
        <p>暂无活跃提供商健康数据</p>
        <p class="text-xs mt-1">
          当前没有活跃提供商或尚未产生请求记录
        </p>
      </div>

      <div
        v-else
        class="space-y-3"
      >
        <Collapsible
          v-for="provider in providers"
          :key="provider.provider_id"
          v-model:open="expandedProviders[provider.provider_id]"
          class="overflow-hidden rounded-xl border border-border/60 bg-card/60"
        >
          <CollapsibleTrigger as-child>
            <button
              type="button"
              class="flex w-full flex-col gap-4 p-4 text-left transition-colors hover:bg-muted/30 lg:flex-row lg:items-center lg:justify-between"
            >
              <div class="flex min-w-0 items-start gap-3">
                <div class="flex h-11 w-11 flex-shrink-0 items-center justify-center rounded-xl border border-border/60 bg-muted/50">
                  <Server class="h-5 w-5 text-muted-foreground" />
                </div>
                <div class="min-w-0">
                  <div class="flex min-w-0 flex-wrap items-center gap-2">
                    <ChevronRight
                      class="h-4 w-4 text-muted-foreground transition-transform"
                      :class="{ 'rotate-90': expandedProviders[provider.provider_id] }"
                    />
                    <h4 class="truncate text-sm font-semibold">
                      {{ provider.provider_name }}
                    </h4>
                    <Badge
                      variant="outline"
                      class="font-mono text-[11px]"
                    >
                      {{ provider.provider_type || 'custom' }}
                    </Badge>
                    <Badge :variant="getHealthBadgeVariant(provider)">
                      {{ getHealthLabel(provider) }}
                    </Badge>
                  </div>
                  <p class="mt-1 text-xs text-muted-foreground">
                    {{ getProviderMetaText(provider) }}
                  </p>
                </div>
              </div>

              <HealthMetricGrid
                class="lg:max-w-2xl"
                :avg-latency-ms="provider.avg_latency_ms"
                :avg-first-byte-ms="provider.avg_first_byte_ms"
                :avg-tps="provider.avg_tps"
                :total-attempts="provider.total_attempts"
                :success-rate="provider.success_rate"
              />
            </button>
          </CollapsibleTrigger>

          <CollapsibleContent class="border-t border-border/50 px-4 pb-4 pt-4">
            <div
              v-if="provider.models.length === 0"
              class="rounded-lg border border-dashed border-border/60 py-8 text-center text-sm text-muted-foreground"
            >
              该提供商在当前时间范围内暂无模型请求
            </div>
            <div
              v-else
              class="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4"
            >
              <div
                v-for="model in provider.models"
                :key="`${provider.provider_id}-${model.model}`"
                class="relative overflow-hidden rounded-xl border border-border/60 bg-card/80 p-4 transition-colors hover:border-primary/50"
              >
                <div class="absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-primary/40 to-transparent" />
                <div class="flex items-start justify-between gap-3">
                  <div class="flex min-w-0 items-center gap-3">
                    <div class="flex h-11 w-11 flex-shrink-0 items-center justify-center rounded-xl border border-border/60 bg-muted/50">
                      <Bot class="h-5 w-5 text-muted-foreground" />
                    </div>
                    <h4 class="min-w-0 truncate text-sm font-semibold">
                      {{ model.display_name || model.model }}
                    </h4>
                  </div>
                  <Badge
                    :variant="getHealthBadgeVariant(model)"
                    class="shrink-0"
                  >
                    {{ getHealthLabel(model) }}
                  </Badge>
                </div>

                <HealthMetricGrid
                  class="mt-4"
                  :avg-latency-ms="model.avg_latency_ms"
                  :avg-first-byte-ms="model.avg_first_byte_ms"
                  :avg-tps="model.avg_tps"
                  :total-attempts="model.total_attempts"
                  :success-rate="model.success_rate"
                />

                <div class="mt-4 flex items-center justify-between gap-3 text-[11px] uppercase tracking-wide text-muted-foreground">
                  <span>History (60pts)</span>
                  <span class="truncate normal-case tracking-normal">
                    {{ formatCompactNumber(model.total_attempts) }} 次请求
                  </span>
                </div>

                <HealthStatusTimeline
                  class="mt-2"
                  :timeline="model.timeline"
                  :time-range-start="model.time_range_start"
                  :time-range-end="model.time_range_end"
                  :generated-at="generatedAt"
                  :lookback-hours="parseInt(lookbackHours)"
                  entity-label="模型"
                  :entity-name="model.model"
                />
              </div>
            </div>
          </CollapsibleContent>
        </Collapsible>
      </div>
    </div>
  </Card>
</template>

<script setup lang="ts">
import { ref, onMounted, watch } from 'vue'
import { Bot, ChevronRight, Loader2, Server } from 'lucide-vue-next'
import Card from '@/components/ui/card.vue'
import Badge from '@/components/ui/badge.vue'
import Collapsible from '@/components/ui/collapsible.vue'
import CollapsibleTrigger from '@/components/ui/collapsible-trigger.vue'
import CollapsibleContent from '@/components/ui/collapsible-content.vue'
import HealthMetricGrid from './HealthMetricGrid.vue'
import HealthMonitorHeader from './HealthMonitorHeader.vue'
import HealthStatusTimeline from './HealthStatusTimeline.vue'
import { getProviderStatusMonitor } from '@/api/endpoints/health'
import type { ProviderStatusMonitor } from '@/api/endpoints/types'
import { useToast } from '@/composables/useToast'
import { parseApiError } from '@/utils/errorParser'
import {
  formatCompactNumber,
  getHealthBadgeVariant,
  getHealthLabel
} from './health-monitor-utils'

const props = withDefaults(defineProps<{
  title?: string
}>(), {
  title: '提供商健康监控'
})

const { error: showError } = useToast()

const loading = ref(false)
const loadingMonitors = ref(false)
const providers = ref<ProviderStatusMonitor[]>([])
const generatedAt = ref<string | null>(null)
const lookbackHours = ref('6')
const expandedProviders = ref<Record<string, boolean>>({})

async function loadMonitors() {
  loadingMonitors.value = true
  try {
    const data = await getProviderStatusMonitor({
      lookback_hours: parseInt(lookbackHours.value),
      provider_limit: 50,
      per_provider_model_limit: 12,
      per_model_limit: 100
    })
    providers.value = data.providers || []
    generatedAt.value = data.generated_at || null
    ensureExpandedProviderState()
  } catch (err: unknown) {
    showError(parseApiError(err, '加载提供商健康监控数据失败'), '错误')
  } finally {
    loadingMonitors.value = false
  }
}

async function refreshData() {
  loading.value = true
  try {
    await loadMonitors()
  } finally {
    loading.value = false
  }
}

function ensureExpandedProviderState() {
  const next = { ...expandedProviders.value }
  for (const provider of providers.value) {
    if (!(provider.provider_id in next)) {
      next[provider.provider_id] = false
    }
  }
  expandedProviders.value = next
}

function getProviderMetaText(provider: ProviderStatusMonitor) {
  const attempts = `${formatCompactNumber(provider.total_attempts)} 次请求`
  return `${provider.model_count} 个模型 / ${attempts}`
}

watch(lookbackHours, () => {
  loadMonitors()
})

onMounted(() => {
  refreshData()
})
</script>
