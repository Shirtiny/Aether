<template>
  <Dialog
    :model-value="modelValue"
    title="号池调度"
    description="拖拽排序调度维度，越靠前优先级越高"
    size="lg"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <div class="space-y-5">
      <!-- Preset List -->
      <div class="space-y-3">
        <div class="space-y-1">
          <h3 class="text-sm font-medium border-b pb-2">
            调度维度
          </h3>
          <p class="text-xs text-muted-foreground">
            拖拽排序，越靠前优先级越高。不适用当前 Provider 类型的维度已禁用。
          </p>
        </div>

        <div class="space-y-0.5">
          <div
            v-for="(item, index) in presetList"
            v-show="!isMutexFollower(index)"
            :key="item.preset"
            class="group flex items-center gap-3 px-3 py-2.5 rounded-lg border transition-all duration-200"
            :class="[
              !displayItems[index].applicable
                ? 'border-border/30 bg-muted/20 opacity-50'
                : draggedIndex === index
                  ? 'border-primary/50 bg-primary/5 shadow-md scale-[1.01]'
                  : dragOverIndex === index
                    ? 'border-primary/30 bg-primary/5'
                    : 'border-border/50 bg-background hover:border-border hover:bg-muted/30'
            ]"
            :draggable="canDragPreset(index)"
            @dragstart="canDragPreset(index) && handleDragStart(index, $event)"
            @dragend="handleDragEnd"
            @dragover.prevent="canDragPreset(index) && handleDragOver(index)"
            @dragleave="handleDragLeave"
            @drop="canDragPreset(index) && handleDrop(index)"
          >
            <!-- Drag handle -->
            <div
              class="p-1 rounded transition-colors shrink-0"
              :class="canDragPreset(index)
                ? 'cursor-grab active:cursor-grabbing text-muted-foreground/40 group-hover:text-muted-foreground'
                : 'text-muted-foreground/15 cursor-default'"
            >
              <GripVertical class="w-4 h-4" />
            </div>

            <template v-if="item.mutexGroup">
              <div class="flex gap-0.5 p-0.5 bg-muted/40 rounded-md shrink-0">
                <button
                  v-for="member in getMutexGroupItems(index)"
                  :key="member.preset"
                  type="button"
                  class="px-2.5 py-1 text-xs font-medium rounded transition-all"
                  :disabled="!member.applicable"
                  :class="[
                    member.preset === displayItems[index].preset
                      ? 'bg-primary text-primary-foreground shadow-sm'
                      : 'text-muted-foreground hover:text-foreground hover:bg-background/50',
                    !member.applicable ? 'opacity-40 cursor-not-allowed' : ''
                  ]"
                  @click="selectMutexPreset(index, member.preset)"
                >
                  {{ member.label }}
                </button>
              </div>
            </template>
            <Switch
              v-else
              :model-value="item.enabled"
              :disabled="!item.applicable"
              @update:model-value="(v: boolean) => togglePreset(index, v)"
            />

            <!-- Info -->
            <div class="flex-1 min-w-0">
              <div class="flex items-center gap-2">
                <span
                  class="text-sm font-medium"
                  :class="!displayItems[index].applicable ? 'text-muted-foreground' : ''"
                >{{ displayItems[index].label }}</span>
                <span
                  v-if="!displayItems[index].applicable"
                  class="text-[10px] text-muted-foreground/60"
                >
                  (不适用)
                </span>
              </div>
              <p class="text-xs text-muted-foreground mt-0.5">
                {{ displayItems[index].desc }}
              </p>
              <p
                v-if="displayItems[index].evidenceHint"
                class="text-[11px] text-muted-foreground/80 mt-1"
              >
                依据: {{ displayItems[index].evidenceHint }}
              </p>

              <!-- Mode sub-config -->
              <div
                v-if="displayItems[index].modeOptions.length > 0 && displayItems[index].enabled && displayItems[index].applicable"
                class="flex gap-0.5 mt-2 p-0.5 bg-muted/40 rounded-md w-fit"
              >
                <button
                  v-for="modeOpt in displayItems[index].modeOptions"
                  :key="modeOpt.value"
                  type="button"
                  class="px-2.5 py-1 text-xs font-medium rounded transition-all"
                  :class="[
                    displayItems[index].mode === modeOpt.value
                      ? 'bg-primary text-primary-foreground shadow-sm'
                      : 'text-muted-foreground hover:text-foreground hover:bg-background/50'
                  ]"
                  @click="setPresetModeByPreset(displayItems[index].preset, modeOpt.value)"
                >
                  {{ modeOpt.label }}
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <template #footer>
      <Button
        variant="outline"
        :disabled="loading"
        @click="emit('update:modelValue', false)"
      >
        取消
      </Button>
      <Button
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
import type { PoolPresetMeta } from '@/api/endpoints/pool'
import type {
  PoolAdvancedConfig,
  SchedulingPresetItem,
  ProviderWithEndpointsSummary,
} from '@/api/endpoints/types/provider'

interface PresetModeOption {
  value: string
  label: string
}

interface PresetListItem {
  preset: string
  label: string
  desc: string
  enabled: boolean
  mode: string | null
  modeOptions: PresetModeOption[]
  applicable: boolean
  mutexGroup: string | null
  evidenceHint: string
}

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

const FALLBACK_PRESET_DEFS: PoolPresetMeta[] = [
  {
    name: 'lru',
    label: 'LRU 轮转',
    description: '最久未使用的 Key 优先',
    mutex_group: 'distribution_mode',
    evidence_hint: '依据 LRU 时间戳（最近未使用优先）',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'free_team_first',
    label: 'Free/Team 优先',
    description: '优先消耗低档账号（依赖 plan_type）',
    evidence_hint: '依据 plan_type（oauth_plan_type 或 upstream_metadata）',
    providers: ['codex', 'kiro'],
    modes: [
      { value: 'free_only', label: 'Free' },
      { value: 'team_only', label: 'Team' },
      { value: 'both', label: '全部' },
    ],
    default_mode: 'both',
  },
  {
    name: 'quota_balanced',
    label: '额度平均',
    description: '优先选额度消耗最少的账号',
    evidence_hint: '依据账号配额使用率；无配额时回退到窗口成本使用',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'recent_refresh',
    label: '额度刷新优先',
    description: '优先选即将刷新额度的账号',
    evidence_hint: '依据账号额度重置倒计时（next_reset / reset_seconds）',
    providers: ['codex', 'kiro'],
    modes: null,
    default_mode: null,
  },
  {
    name: 'single_account',
    label: '单号优先',
    description: '集中使用同一账号（反向 LRU）',
    mutex_group: 'distribution_mode',
    evidence_hint: '先按账号优先级（internal_priority），同级再按反向 LRU 集中',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'priority_first',
    label: '优先级优先',
    description: '按账号优先级顺序调度（数字越小越优先）',
    evidence_hint: '依据 internal_priority（支持拖拽/手工编辑）',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'health_first',
    label: '健康优先',
    description: '优先选择健康分更高、失败更少的账号',
    evidence_hint: '依据 health_by_format 聚合分（含熔断/失败衰减）',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'latency_first',
    label: '延迟优先',
    description: '优先选择最近延迟更低的账号',
    evidence_hint: '依据号池延迟窗口均值（latency_window_seconds）',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'cost_first',
    label: '成本优先',
    description: '优先选择窗口消耗更低的账号',
    evidence_hint: '依据窗口成本/Token 用量，缺失时回退配额使用率',
    providers: [],
    modes: null,
    default_mode: null,
  },
]

const DEFAULT_ENABLED_PRESETS = new Set(['lru', 'quota_balanced'])

const { success, error: showError } = useToast()
const loading = ref(false)
const presetDefs = ref<PoolPresetMeta[]>([])
const presetDefsLoaded = ref(false)
const loadingPresetDefs = ref(false)

const draggedIndex = ref<number | null>(null)
const dragOverIndex = ref<number | null>(null)
const presetList = ref<PresetListItem[]>([])

function normalizeProviderType(value: string | undefined): string {
  return (value || '').trim().toLowerCase()
}

function normalizePresetName(value: unknown): string {
  return String(value ?? '').trim().toLowerCase()
}

function normalizeMode(value: unknown): string | null {
  const normalized = String(value ?? '').trim().toLowerCase()
  return normalized || null
}

function normalizeMutexGroup(value: unknown): string | null {
  const normalized = String(value ?? '').trim().toLowerCase()
  return normalized || null
}

function normalizePresetDefs(defs: PoolPresetMeta[]): PoolPresetMeta[] {
  const ordered: PoolPresetMeta[] = []
  const seen = new Set<string>()
  for (const raw of defs) {
    const name = normalizePresetName(raw.name)
    if (!name || seen.has(name)) continue
    seen.add(name)
    const providers = Array.isArray(raw.providers)
      ? raw.providers.map(p => normalizeProviderType(p)).filter(Boolean)
      : []
    const modes = Array.isArray(raw.modes)
      ? raw.modes
        .map(mode => ({
          value: normalizePresetName(mode.value),
          label: String(mode.label ?? '').trim() || String(mode.value ?? '').trim(),
        }))
        .filter(mode => Boolean(mode.value))
      : null
    const defaultMode = normalizeMode(raw.default_mode)
    ordered.push({
      name,
      label: String(raw.label ?? '').trim() || name,
      description: String(raw.description ?? '').trim(),
      providers,
      modes: modes && modes.length > 0 ? modes : null,
      default_mode: defaultMode,
      mutex_group: normalizeMutexGroup(raw.mutex_group),
      evidence_hint: String(raw.evidence_hint ?? '').trim() || null,
    })
  }
  return ordered
}

function getPresetDefs(): PoolPresetMeta[] {
  if (presetDefs.value.length > 0) {
    return presetDefs.value
  }
  return FALLBACK_PRESET_DEFS
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

function isApplicablePreset(def: PoolPresetMeta): boolean {
  const providerType = normalizeProviderType(props.providerType)
  const providers = Array.isArray(def.providers) ? def.providers : []
  if (providers.length === 0) return true
  if (!providerType) return true
  return providers.includes(providerType)
}

function getModeOptions(def: PoolPresetMeta): PresetModeOption[] {
  const modes = Array.isArray(def.modes) ? def.modes : []
  return modes
    .map(mode => ({
      value: normalizePresetName(mode.value),
      label: String(mode.label ?? '').trim() || String(mode.value ?? '').trim(),
    }))
    .filter(mode => Boolean(mode.value))
}

function defaultModeForPreset(def: PoolPresetMeta): string | null {
  const options = getModeOptions(def)
  if (options.length === 0) return null
  const normalizedDefault = normalizeMode(def.default_mode)
  if (normalizedDefault && options.some(option => option.value === normalizedDefault)) {
    return normalizedDefault
  }
  return options[0].value
}

function buildDefaultPresetList(): PresetListItem[] {
  return getPresetDefs().map(def => ({
    preset: def.name,
    label: def.label,
    desc: def.description,
    enabled: DEFAULT_ENABLED_PRESETS.has(def.name),
    mode: defaultModeForPreset(def),
    modeOptions: getModeOptions(def),
    applicable: isApplicablePreset(def),
    mutexGroup: normalizeMutexGroup(def.mutex_group),
    evidenceHint: String(def.evidence_hint ?? '').trim(),
  }))
}

function isNewFormatPresetItem(item: unknown): item is SchedulingPresetItem {
  return typeof item === 'object' && item !== null && 'preset' in item
}

function resolveMode(def: PoolPresetMeta, mode: unknown): string | null {
  const options = getModeOptions(def)
  if (options.length === 0) return null
  const normalized = normalizeMode(mode)
  if (normalized && options.some(option => option.value === normalized)) {
    return normalized
  }
  return defaultModeForPreset(def)
}

function loadFromConfig(cfg: PoolAdvancedConfig | null): PresetListItem[] {
  const defs = getPresetDefs()
  const defsByName = new Map(defs.map(def => [def.name, def]))
  const defaults = buildDefaultPresetList()
  if (!cfg) return defaults

  const rawPresets = cfg.scheduling_presets
  if (!Array.isArray(rawPresets) || rawPresets.length === 0) {
    if (cfg.scheduling_mode === 'lru' || (!cfg.scheduling_mode && cfg.lru_enabled !== false)) {
      return defaults.map(item => ({
        ...item,
        enabled: item.preset === 'lru',
      }))
    }
    return defaults
  }

  const first = rawPresets[0]
  if (isNewFormatPresetItem(first)) {
    const configItems = rawPresets as SchedulingPresetItem[]
    const ordered: PresetListItem[] = []
    const seen = new Set<string>()

    for (const ci of configItems) {
      const presetName = normalizePresetName(ci.preset)
      const def = defsByName.get(presetName)
      if (!def || seen.has(presetName)) continue
      seen.add(presetName)
      ordered.push({
        preset: presetName,
        label: def.label,
        desc: def.description,
        enabled: ci.enabled !== false,
        mode: resolveMode(def, ci.mode),
        modeOptions: getModeOptions(def),
        applicable: isApplicablePreset(def),
        mutexGroup: normalizeMutexGroup(def.mutex_group),
        evidenceHint: String(def.evidence_hint ?? '').trim(),
      })
    }

    for (const def of defs) {
      if (seen.has(def.name)) continue
      ordered.push({
        preset: def.name,
        label: def.label,
        desc: def.description,
        enabled: false,
        mode: defaultModeForPreset(def),
        modeOptions: getModeOptions(def),
        applicable: isApplicablePreset(def),
        mutexGroup: normalizeMutexGroup(def.mutex_group),
        evidenceHint: String(def.evidence_hint ?? '').trim(),
      })
    }
    return ordered
  }

  const legacyPresets = rawPresets as string[]
  const lruEnabled = cfg.lru_enabled !== false
  const ordered: PresetListItem[] = []
  const seen = new Set<string>()

  const lruDef = defsByName.get('lru')
  if (lruDef) {
    ordered.push({
      preset: 'lru',
      label: lruDef.label,
      desc: lruDef.description,
      enabled: lruEnabled,
      mode: null,
      modeOptions: [],
      applicable: isApplicablePreset(lruDef),
      mutexGroup: normalizeMutexGroup(lruDef.mutex_group),
      evidenceHint: String(lruDef.evidence_hint ?? '').trim(),
    })
    seen.add('lru')
  }

  for (const name of legacyPresets) {
    const presetName = normalizePresetName(name)
    const def = defsByName.get(presetName)
    if (!def || seen.has(presetName)) continue
    seen.add(presetName)
    ordered.push({
      preset: presetName,
      label: def.label,
      desc: def.description,
      enabled: true,
      mode: resolveMode(def, undefined),
      modeOptions: getModeOptions(def),
      applicable: isApplicablePreset(def),
      mutexGroup: normalizeMutexGroup(def.mutex_group),
      evidenceHint: String(def.evidence_hint ?? '').trim(),
    })
  }

  for (const def of defs) {
    if (seen.has(def.name)) continue
    ordered.push({
      preset: def.name,
      label: def.label,
      desc: def.description,
      enabled: false,
      mode: defaultModeForPreset(def),
      modeOptions: getModeOptions(def),
      applicable: isApplicablePreset(def),
      mutexGroup: normalizeMutexGroup(def.mutex_group),
      evidenceHint: String(def.evidence_hint ?? '').trim(),
    })
  }
  return ordered
}

function normalizeMutexSelection(items: PresetListItem[]): PresetListItem[] {
  const next = [...items]
  const groups = new Map<string, number[]>()

  next.forEach((item, index) => {
    if (!item.mutexGroup) return
    if (!groups.has(item.mutexGroup)) groups.set(item.mutexGroup, [])
    groups.get(item.mutexGroup)?.push(index)
  })

  for (const indexes of groups.values()) {
    if (indexes.length <= 1) continue
    const enabledApplicable = indexes.find(index => {
      const item = next[index]
      return item.enabled && item.applicable
    })
    const firstApplicable = indexes.find(index => next[index].applicable)
    const winner = enabledApplicable ?? firstApplicable ?? indexes[0]
    indexes.forEach((index) => {
      next[index].enabled = index === winner && next[index].applicable
    })
  }

  return next
}

function togglePreset(index: number, enabled: boolean) {
  const item = presetList.value[index]
  if (!item) return
  if (!item.mutexGroup) {
    item.enabled = enabled
    return
  }

  const memberIndexes = getMutexGroupIndexes(index)
  if (enabled) {
    for (const memberIndex of memberIndexes) {
      const member = presetList.value[memberIndex]
      if (!member) continue
      member.enabled = member.preset === item.preset && member.applicable
    }
    return
  }

  for (const memberIndex of memberIndexes) {
    const member = presetList.value[memberIndex]
    if (!member) continue
    member.enabled = false
  }
}

function setPresetModeByPreset(preset: string, mode: string) {
  const targetIndex = presetList.value.findIndex(item => item.preset === preset)
  if (targetIndex < 0) return
  presetList.value[targetIndex].mode = mode
}

const mutexGroupIndexMap = computed<Record<string, number[]>>(() => {
  const grouped: Record<string, number[]> = {}
  presetList.value.forEach((item, index) => {
    if (!item.mutexGroup) return
    if (!grouped[item.mutexGroup]) grouped[item.mutexGroup] = []
    grouped[item.mutexGroup].push(index)
  })
  return grouped
})

function getMutexGroupIndexes(index: number): number[] {
  const item = presetList.value[index]
  if (!item?.mutexGroup) return []
  return mutexGroupIndexMap.value[item.mutexGroup] || []
}

function isMutexFollower(index: number): boolean {
  const memberIndexes = getMutexGroupIndexes(index)
  if (memberIndexes.length <= 1) return false
  return memberIndexes[0] !== index
}

function getMutexGroupItems(index: number): PresetListItem[] {
  return getMutexGroupIndexes(index)
    .map(memberIndex => presetList.value[memberIndex])
    .filter((item): item is PresetListItem => Boolean(item))
}

function getMutexSelectedItem(index: number): PresetListItem | null {
  const members = getMutexGroupItems(index)
  if (members.length === 0) return null
  return members.find(member => member.enabled && member.applicable)
    || members.find(member => member.applicable)
    || members[0]
}

function selectMutexPreset(index: number, presetName: string) {
  const memberIndexes = getMutexGroupIndexes(index)
  if (memberIndexes.length === 0) return
  for (const memberIndex of memberIndexes) {
    const member = presetList.value[memberIndex]
    if (!member) continue
    member.enabled = member.preset === presetName && member.applicable
  }
}

function canDragPreset(index: number): boolean {
  const item = presetList.value[index]
  if (!item) return false
  if (item.mutexGroup) return false
  return item.applicable
}

const EMPTY_DISPLAY_ITEM: PresetListItem = {
  preset: '',
  label: '',
  desc: '',
  enabled: false,
  mode: null,
  modeOptions: [],
  applicable: false,
  mutexGroup: null,
  evidenceHint: '',
}

const displayItems = computed<PresetListItem[]>(() =>
  presetList.value.map((item, index) => {
    if (!item) return EMPTY_DISPLAY_ITEM
    if (!item.mutexGroup) return item
    return getMutexSelectedItem(index) || item
  }),
)

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
  presetList.value = normalizeMutexSelection(loadFromConfig(props.currentConfig))
})

async function handleSave() {
  loading.value = true
  try {
    presetList.value = normalizeMutexSelection(presetList.value)
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

    // 合并已有配置，仅覆盖 scheduling_presets，保留其他字段
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
