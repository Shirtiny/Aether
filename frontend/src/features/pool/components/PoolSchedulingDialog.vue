<template>
  <Dialog
    :model-value="modelValue"
    title="号池调度"
    description="管理号池内 Key 的分配模式和排序偏好"
    size="lg"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <div class="max-h-[calc(100dvh-13rem)] space-y-5 overflow-y-auto overscroll-contain pr-1 sm:max-h-[min(72vh,42rem)] sm:space-y-6 sm:pr-2">
      <!-- Section 1: 分配模式 (distribution_mode 互斥组, 五选一) -->
      <div class="space-y-4 rounded-2xl border border-border/60 bg-card/70 p-4">
        <div class="space-y-1">
          <h3 class="text-sm font-medium">
            分配模式
          </h3>
          <p class="text-xs text-muted-foreground">
            控制 Key 的基础分配方式，选择一种模式。
          </p>
        </div>

        <div class="grid grid-cols-2 gap-2 sm:grid-cols-4">
          <button
            v-for="{ index, item } in distributionItems"
            :key="item.preset"
            type="button"
            class="min-h-11 rounded-xl border px-3 py-2.5 text-sm font-medium leading-tight transition-all duration-200"
            :disabled="!item.applicable"
            :class="[
              activeDistributionPreset === item.preset
                ? 'border-primary bg-primary text-primary-foreground shadow-sm shadow-primary/20'
                : item.applicable
                  ? 'border-border/60 bg-background text-foreground hover:border-border hover:bg-muted/40'
                  : 'border-border/30 bg-muted/20 text-muted-foreground/50 cursor-not-allowed'
            ]"
            @click="item.applicable && selectDistribution(index, item.preset)"
          >
            {{ item.label }}
          </button>
        </div>

        <div
          v-if="activeDistributionLabel || activeDistributionDesc"
          class="rounded-xl border border-primary/15 bg-primary/5 px-3 py-2.5"
        >
          <p
            v-if="activeDistributionDesc"
            class="mt-1 text-xs leading-5 text-muted-foreground"
          >
            {{ activeDistributionDesc }}
          </p>
        </div>
      </div>

      <!-- Section 2: 策略调度 (非互斥, 可叠加组合 + 拖拽排序) -->
      <div class="space-y-4 rounded-2xl border border-border/60 bg-card/70 p-4">
        <div class="space-y-1">
          <div class="flex flex-wrap items-center gap-2">
            <h3 class="text-sm font-medium">
              策略调度
            </h3>
            <span class="rounded-full bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">
              已启用 {{ enabledStrategyCount }} 项
            </span>
          </div>
          <p class="text-xs text-muted-foreground">
            在分配模式基础上叠加排序因素，可组合启用。
          </p>
          <p class="text-xs text-muted-foreground">
            桌面端支持拖拽排序，移动端可点按上下调整优先级。
          </p>
        </div>

        <div class="space-y-1.5">
          <div
            v-for="{ index, item } in strategyItems"
            :key="item.preset"
            class="group rounded-xl border px-3 py-2.5 transition-all duration-200"
            :class="[
              !item.applicable
                ? 'border-border/40 bg-muted/20 opacity-80'
                : draggedIndex === index
                  ? 'border-primary/50 bg-primary/5 shadow-md'
                  : dragOverIndex === index
                    ? 'border-primary/30 bg-primary/5'
                    : item.enabled
                      ? 'border-primary/20 bg-primary/5 hover:border-primary/30'
                      : 'border-border/60 bg-background hover:border-border hover:bg-muted/30'
            ]"
            :draggable="item.applicable"
            @dragstart="item.applicable && handleDragStart(index, $event)"
            @dragend="handleDragEnd"
            @dragover.prevent="item.applicable && handleDragOver(index)"
            @dragleave="handleDragLeave"
            @drop="item.applicable && handleDrop(index)"
          >
            <div class="flex items-start gap-2.5">
              <!-- Drag handle -->
              <div
                class="hidden rounded-lg p-1 transition-colors sm:flex sm:shrink-0"
                :class="item.applicable
                  ? 'cursor-grab active:cursor-grabbing text-muted-foreground/40 group-hover:text-muted-foreground'
                  : 'text-muted-foreground/20 cursor-default'"
              >
                <GripVertical class="h-4 w-4" />
              </div>

              <div class="min-w-0 flex-1">
                <div class="flex items-start gap-2.5">
                  <div class="min-w-0 flex-1">
                    <div class="flex flex-wrap items-center gap-2">
                      <span
                        class="text-sm font-medium"
                        :class="!item.applicable ? 'text-muted-foreground' : 'text-foreground'"
                      >{{ item.label }}</span>
                      <span
                        v-if="getStrategyPriority(index)"
                        class="rounded-full bg-primary/10 px-2 py-0.5 text-[11px] font-medium text-primary"
                      >
                        #{{ getStrategyPriority(index) }}
                      </span>
                      <span
                        v-if="!item.applicable"
                        class="rounded-full border border-border/60 bg-muted px-2 py-0.5 text-[11px] text-muted-foreground"
                      >
                        当前不可用
                      </span>
                    </div>
                    <p class="mt-0.5 text-xs leading-5 text-muted-foreground">
                      {{ item.desc }}
                    </p>
                  </div>

                  <div
                    v-if="item.applicable"
                    class="flex shrink-0 items-center gap-1.5 sm:hidden"
                  >
                    <button
                      type="button"
                      class="inline-flex h-7 items-center justify-center rounded-lg border border-border/60 px-2.5 text-[11px] font-medium text-foreground transition-colors hover:bg-muted disabled:cursor-not-allowed disabled:opacity-40"
                      :disabled="!canMoveStrategy(index, -1)"
                      @click="moveStrategy(index, -1)"
                    >
                      上移
                    </button>
                    <button
                      type="button"
                      class="inline-flex h-7 items-center justify-center rounded-lg border border-border/60 px-2.5 text-[11px] font-medium text-foreground transition-colors hover:bg-muted disabled:cursor-not-allowed disabled:opacity-40"
                      :disabled="!canMoveStrategy(index, 1)"
                      @click="moveStrategy(index, 1)"
                    >
                      下移
                    </button>
                  </div>

                  <Switch
                    :model-value="item.enabled && item.applicable"
                    :disabled="!item.applicable"
                    class="mt-0.5 shrink-0"
                    @update:model-value="(v: boolean) => togglePreset(index, v)"
                  />
                </div>

                <div
                  v-if="(item.modeOptions.length > 0 && item.enabled && item.applicable) || item.applicable"
                  class="mt-2 flex flex-col gap-1.5 sm:flex-row sm:items-center sm:justify-between"
                >
                  <!-- Mode sub-config -->
                  <div
                    v-if="item.modeOptions.length > 0 && item.enabled && item.applicable"
                    class="flex flex-wrap gap-1 rounded-lg bg-muted/40 p-1"
                  >
                    <button
                      v-for="modeOpt in item.modeOptions"
                      :key="modeOpt.value"
                      type="button"
                      class="rounded-md px-2.5 py-1 text-xs font-medium transition-all"
                      :class="[
                        item.mode === modeOpt.value
                          ? 'bg-primary text-primary-foreground shadow-sm'
                          : 'text-muted-foreground hover:bg-background/70 hover:text-foreground'
                      ]"
                      @click="setPresetModeByPreset(item.preset, modeOpt.value)"
                    >
                      {{ modeOpt.label }}
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <template #footer>
      <Button
        variant="outline"
        class="min-w-[96px] flex-1 sm:flex-none"
        :disabled="loading"
        @click="emit('update:modelValue', false)"
      >
        取消
      </Button>
      <Button
        class="min-w-[96px] flex-1 sm:flex-none"
        :disabled="loading"
        @click="handleSave"
      >
        {{ loading ? '保存中...' : '保存' }}
      </Button>
    </template>
  </Dialog>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { GripVertical } from 'lucide-vue-next'
import { Dialog, Button, Switch } from '@/components/ui'
import { useToast } from '@/composables/useToast'
import { parseApiError } from '@/utils/errorParser'
import { updateProvider } from '@/api/endpoints'
import { getPoolSchedulingPresets } from '@/api/endpoints/pool'
import {
  FALLBACK_SCHEDULING_PRESET_DEFS,
  hydrateSchedulingPresetList,
  moveStrategyItem,
  normalizeMutexSelection as normalizeSchedulingMutexSelection,
  normalizePresetDefs,
  normalizeProviderType,
  type HydratedSchedulingPresetItem,
  type SchedulingPresetDefLike,
} from '@/features/pool/utils/poolSchedulingDialog'
import type {
  PoolAdvancedConfig,
  SchedulingPresetItem,
  ProviderWithEndpointsSummary,
} from '@/api/endpoints/types/provider'

type PresetListItem = HydratedSchedulingPresetItem

const props = defineProps<{
  modelValue: boolean
  providerId: string
  providerType?: string
  currentConfig: PoolAdvancedConfig | null
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
  saved: [provider: ProviderWithEndpointsSummary]
}>()

const DISTRIBUTION_GROUP = 'distribution_mode'

const { success, error: showError } = useToast()
const loading = ref(false)
const presetDefs = ref<SchedulingPresetDefLike[]>([])
const presetDefsLoaded = ref(false)
const loadingPresetDefs = ref(false)

const draggedIndex = ref<number | null>(null)
const dragOverIndex = ref<number | null>(null)
const presetList = ref<PresetListItem[]>([])

function getPresetDefs(): SchedulingPresetDefLike[] {
  if (presetDefs.value.length > 0) {
    return presetDefs.value
  }
  return FALLBACK_SCHEDULING_PRESET_DEFS
}

async function ensurePresetDefsLoaded(): Promise<void> {
  if (presetDefsLoaded.value || loadingPresetDefs.value) return
  loadingPresetDefs.value = true
  try {
    const remoteDefs = await getPoolSchedulingPresets()
    const normalized = normalizePresetDefs(Array.isArray(remoteDefs) ? remoteDefs : [])
    if (normalized.length > 0) {
      presetDefs.value = normalized
    }
  } catch (err) {
    showError(parseApiError(err))
  } finally {
    presetDefsLoaded.value = true
    loadingPresetDefs.value = false
  }
}

function isApplicablePreset(def: SchedulingPresetDefLike): boolean {
  const providerType = normalizeProviderType(props.providerType)
  const providers = Array.isArray(def.providers) ? def.providers : []
  if (providers.length === 0) return true
  if (!providerType) return true
  return providers.includes(providerType)
}

function togglePreset(index: number, enabled: boolean) {
  const item = presetList.value[index]
  if (!item) return
  item.enabled = enabled
}

function selectDistribution(_anchorIndex: number, presetName: string) {
  presetList.value.forEach(item => {
    if (item.mutexGroup === DISTRIBUTION_GROUP) {
      item.enabled = item.preset === presetName && item.applicable
    }
  })
}

function setPresetModeByPreset(preset: string, mode: string) {
  const targetIndex = presetList.value.findIndex(item => item.preset === preset)
  if (targetIndex < 0) return
  presetList.value[targetIndex].mode = mode
}

const distributionItems = computed(() => {
  const items: { index: number; item: PresetListItem }[] = []
  presetList.value.forEach((item, index) => {
    if (item.mutexGroup === DISTRIBUTION_GROUP) {
      items.push({ index, item })
    }
  })
  return items
})

const activeDistributionPreset = computed(() => {
  const found = distributionItems.value.find(({ item }) => item.enabled && item.applicable)
  return found?.item.preset ?? null
})

const activeDistributionDesc = computed(() => {
  const found = distributionItems.value.find(({ item }) => item.enabled && item.applicable)
  return found?.item.desc ?? null
})

const activeDistributionLabel = computed(() => {
  const found = distributionItems.value.find(({ item }) => item.enabled && item.applicable)
  return found?.item.label ?? null
})

const strategyItems = computed(() => {
  const items: { index: number; item: PresetListItem }[] = []
  presetList.value.forEach((item, index) => {
    if (!item.mutexGroup) {
      items.push({ index, item })
    }
  })
  return items
})

const enabledStrategyPriorityMap = computed(() => {
  const priorities = new Map<number, number>()
  let priority = 0

  strategyItems.value.forEach(({ index, item }) => {
    if (!item.enabled || !item.applicable) return
    priority += 1
    priorities.set(index, priority)
  })

  return priorities
})

const enabledStrategyCount = computed(() => enabledStrategyPriorityMap.value.size)

function getStrategyPriority(index: number): number | null {
  return enabledStrategyPriorityMap.value.get(index) ?? null
}

function canMoveStrategy(index: number, direction: -1 | 1): boolean {
  const strategyIndexes = strategyItems.value.map(({ index: currentIndex }) => currentIndex)
  const currentPosition = strategyIndexes.indexOf(index)
  if (currentPosition === -1) return false
  const targetPosition = currentPosition + direction
  return targetPosition >= 0 && targetPosition < strategyIndexes.length
}

function moveStrategy(index: number, direction: -1 | 1) {
  presetList.value = moveStrategyItem(presetList.value, index, direction)
}

function handleDragStart(index: number, event: DragEvent) {
  draggedIndex.value = index
  if (event.dataTransfer) {
    event.dataTransfer.effectAllowed = 'move'
    event.dataTransfer.setData('text/html', '')
  }
}

function handleDragEnd() {
  draggedIndex.value = null
  dragOverIndex.value = null
}

function handleDragOver(index: number) {
  dragOverIndex.value = index
}

function handleDragLeave() {
  dragOverIndex.value = null
}

function handleDrop(dropIndex: number) {
  if (draggedIndex.value === null || draggedIndex.value === dropIndex) {
    draggedIndex.value = null
    dragOverIndex.value = null
    return
  }
  const items = [...presetList.value]
  const [draggedItem] = items.splice(draggedIndex.value, 1)
  items.splice(dropIndex, 0, draggedItem)
  presetList.value = items
  draggedIndex.value = null
  dragOverIndex.value = null
}

watch(() => props.modelValue, async (open) => {
  if (!open) return
  await ensurePresetDefsLoaded()
  presetList.value = hydrateSchedulingPresetList(
    props.currentConfig,
    getPresetDefs(),
    isApplicablePreset,
  )
})

async function handleSave() {
  loading.value = true
  try {
    presetList.value = normalizeSchedulingMutexSelection(presetList.value)
    const schedulingPresets: SchedulingPresetItem[] = presetList.value.map(item => {
      const result: SchedulingPresetItem = {
        preset: item.preset,
        enabled: item.enabled && item.applicable,
      }
      if (item.modeOptions.length > 0 && item.mode) {
        result.mode = item.mode
      }
      return result
    })

    const mergedAdvanced: Record<string, unknown> = {
      ...(props.currentConfig ?? {}),
      scheduling_presets: schedulingPresets,
    }
    const payload: Parameters<typeof updateProvider>[1] = {
      pool_advanced: mergedAdvanced as PoolAdvancedConfig,
    }
    const updatedProvider = await updateProvider(props.providerId, payload)

    success('号池调度已保存')
    emit('saved', updatedProvider)
    emit('update:modelValue', false)
  } catch (err) {
    showError(parseApiError(err))
  } finally {
    loading.value = false
  }
}
</script>
