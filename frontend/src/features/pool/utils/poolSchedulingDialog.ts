export interface SchedulingDialogPresetLike {
  mutexGroup: string | null
  enabled: boolean
  applicable?: boolean
}

export interface SchedulingPresetModeOptionLike {
  value: string
  label: string
}

export interface SchedulingPresetDefLike {
  name: string
  label?: string | null
  description?: string | null
  providers?: string[] | null
  modes?: SchedulingPresetModeOptionLike[] | null
  default_mode?: string | null
  mutex_group?: string | null
  evidence_hint?: string | null
}

export interface SchedulingPresetConfigItemLike {
  preset: string
  enabled?: boolean
  mode?: string | null
}

export interface PoolSchedulingConfigLike {
  scheduling_presets?: unknown
  scheduling_mode?: string | null
  lru_enabled?: boolean | null
}

export interface HydratedSchedulingPresetItem extends SchedulingDialogPresetLike {
  preset: string
  label: string
  desc: string
  mode: string | null
  modeOptions: SchedulingPresetModeOptionLike[]
  mutexGroup: string | null
  evidenceHint: string
}

const DISTRIBUTION_GROUP = 'distribution_mode'
const DEFAULT_ENABLED_PRESETS = new Set(['cache_affinity', 'recent_refresh'])

export const FALLBACK_SCHEDULING_PRESET_DEFS: SchedulingPresetDefLike[] = [
  {
    name: 'no_weight',
    label: '无权重',
    description: '基础分配不叠加任何排序权重；会话粘性由高级设置里的 TTL 控制',
    mutex_group: DISTRIBUTION_GROUP,
    evidence_hint: '仅保留策略调度项，不使用 LRU / 优先级 / 负载权重；TTL 大于 0 时成功后写入会话绑定',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'cache_affinity',
    label: '缓存亲和',
    description: '优先复用最近使用过的 Key，利用 Prompt Caching；会话粘性由高级设置里的 TTL 控制',
    mutex_group: DISTRIBUTION_GROUP,
    evidence_hint: '依据 LRU 时间戳（最近使用优先，与 LRU 轮转相反）',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'lru',
    label: 'LRU 轮转',
    description: '最久未使用的 Key 优先；会话粘性由高级设置里的 TTL 控制',
    mutex_group: DISTRIBUTION_GROUP,
    evidence_hint: '依据 LRU 时间戳（最近未使用优先）',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'single_account',
    label: '单号优先',
    description: '集中使用同一账号（反向 LRU）；会话粘性由高级设置里的 TTL 控制',
    mutex_group: DISTRIBUTION_GROUP,
    evidence_hint: '先按账号优先级（internal_priority），同级再按反向 LRU 集中',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'load_balance',
    label: '负载均衡',
    description: '随机分散 Key 使用，均匀分摊负载；会话粘性由高级设置里的 TTL 控制',
    mutex_group: DISTRIBUTION_GROUP,
    evidence_hint: '每次随机分值，实现完全均匀分散',
    providers: [],
    modes: null,
    default_mode: null,
  },
  {
    name: 'free_first',
    label: 'Free 优先',
    description: '优先消耗 Free 账号（依赖 plan_type）',
    evidence_hint: '依据 plan_type（Free 账号优先调度）',
    providers: ['codex', 'kiro'],
    modes: null,
    default_mode: null,
  },
  {
    name: 'team_first',
    label: 'Team 优先',
    description: '优先消耗 Team 账号（依赖 plan_type）',
    evidence_hint: '依据 plan_type（Team 账号优先调度）',
    providers: ['codex', 'kiro'],
    modes: null,
    default_mode: null,
  },
  {
    name: 'plus_first',
    label: 'Plus 优先',
    description: '优先消耗 Plus 账号（依赖 plan_type）',
    evidence_hint: '依据 plan_type（Plus 账号优先调度）',
    providers: ['codex', 'kiro'],
    modes: null,
    default_mode: null,
  },
  {
    name: 'pro_first',
    label: 'Pro 优先',
    description: '优先消耗 Pro 账号（依赖 plan_type）',
    evidence_hint: '依据 plan_type（Pro 账号优先调度）',
    providers: ['codex', 'kiro'],
    modes: null,
    default_mode: null,
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

export const FALLBACK_SCHEDULING_PRESET_ORDER = FALLBACK_SCHEDULING_PRESET_DEFS.map(def => def.name)

export function normalizeProviderType(value: string | undefined): string {
  return (value || '').trim().toLowerCase()
}

export function normalizePresetName(value: unknown): string {
  return String(value ?? '').trim().toLowerCase()
}

export function normalizeMode(value: unknown): string | null {
  const normalized = String(value ?? '').trim().toLowerCase()
  return normalized || null
}

export function normalizeMutexGroup(value: unknown): string | null {
  const normalized = String(value ?? '').trim().toLowerCase()
  return normalized || null
}

export function normalizePresetDefs(defs: readonly SchedulingPresetDefLike[]): SchedulingPresetDefLike[] {
  const ordered: SchedulingPresetDefLike[] = []
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
      ...raw,
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
  ordered.sort((a, b) => {
    const ia = FALLBACK_SCHEDULING_PRESET_ORDER.indexOf(a.name)
    const ib = FALLBACK_SCHEDULING_PRESET_ORDER.indexOf(b.name)
    return (ia === -1 ? 9999 : ia) - (ib === -1 ? 9999 : ib)
  })
  return ordered
}

function getModeOptions(def: SchedulingPresetDefLike): SchedulingPresetModeOptionLike[] {
  const modes = Array.isArray(def.modes) ? def.modes : []
  return modes
    .map(mode => ({
      value: normalizePresetName(mode.value),
      label: String(mode.label ?? '').trim() || String(mode.value ?? '').trim(),
    }))
    .filter(mode => Boolean(mode.value))
}

function defaultModeForPreset(def: SchedulingPresetDefLike): string | null {
  const options = getModeOptions(def)
  if (options.length === 0) return null
  const normalizedDefault = normalizeMode(def.default_mode)
  if (normalizedDefault && options.some(option => option.value === normalizedDefault)) {
    return normalizedDefault
  }
  return options[0].value
}

function resolveMode(def: SchedulingPresetDefLike, mode: unknown): string | null {
  const options = getModeOptions(def)
  if (options.length === 0) return null
  const normalized = normalizeMode(mode)
  if (normalized && options.some(option => option.value === normalized)) {
    return normalized
  }
  return defaultModeForPreset(def)
}

function buildPresetListItem(
  def: SchedulingPresetDefLike,
  enabled: boolean,
  applicable: boolean,
  mode?: unknown,
): HydratedSchedulingPresetItem {
  return {
    preset: def.name,
    label: String(def.label ?? '').trim() || def.name,
    desc: String(def.description ?? '').trim(),
    enabled,
    mode: mode !== undefined ? resolveMode(def, mode) : defaultModeForPreset(def),
    modeOptions: getModeOptions(def),
    applicable,
    mutexGroup: normalizeMutexGroup(def.mutex_group),
    evidenceHint: String(def.evidence_hint ?? '').trim(),
  }
}

function insertMissingByPreferredOrder(
  ordered: HydratedSchedulingPresetItem[],
  seen: Set<string>,
  defs: SchedulingPresetDefLike[],
  defsByName: Map<string, SchedulingPresetDefLike>,
  isApplicablePreset: (def: SchedulingPresetDefLike) => boolean,
) {
  for (const name of FALLBACK_SCHEDULING_PRESET_ORDER) {
    if (seen.has(name)) continue
    const def = defsByName.get(name)
    if (!def) continue
    seen.add(name)
    const item = buildPresetListItem(def, false, isApplicablePreset(def))
    const myIdx = FALLBACK_SCHEDULING_PRESET_ORDER.indexOf(name)
    let insertAt = ordered.length
    for (let i = ordered.length - 1; i >= 0; i--) {
      const peerIdx = FALLBACK_SCHEDULING_PRESET_ORDER.indexOf(ordered[i].preset)
      if (peerIdx !== -1 && peerIdx < myIdx) {
        insertAt = i + 1
        break
      }
      if (i === 0) insertAt = 0
    }
    ordered.splice(insertAt, 0, item)
  }

  for (const def of defs) {
    if (seen.has(def.name)) continue
    seen.add(def.name)
    ordered.push(buildPresetListItem(def, false, isApplicablePreset(def)))
  }
}

function reorderDistributionGroup(items: HydratedSchedulingPresetItem[]): HydratedSchedulingPresetItem[] {
  const distIndexes: number[] = []
  const distItems: HydratedSchedulingPresetItem[] = []
  items.forEach((item, i) => {
    if (item.mutexGroup === DISTRIBUTION_GROUP) {
      distIndexes.push(i)
      distItems.push(item)
    }
  })
  if (distItems.length <= 1) return items

  distItems.sort((a, b) => {
    const ia = FALLBACK_SCHEDULING_PRESET_ORDER.indexOf(a.preset)
    const ib = FALLBACK_SCHEDULING_PRESET_ORDER.indexOf(b.preset)
    return (ia === -1 ? 9999 : ia) - (ib === -1 ? 9999 : ib)
  })

  const result = [...items]
  distIndexes.forEach((origIdx, i) => {
    result[origIdx] = distItems[i]
  })
  return result
}

function isNewFormatPresetItem(item: unknown): item is SchedulingPresetConfigItemLike {
  return typeof item === 'object' && item !== null && 'preset' in item
}

export function hydrateSchedulingPresetList(
  cfg: PoolSchedulingConfigLike | null,
  defs: readonly SchedulingPresetDefLike[] = FALLBACK_SCHEDULING_PRESET_DEFS,
  isApplicablePreset: (def: SchedulingPresetDefLike) => boolean = () => true,
): HydratedSchedulingPresetItem[] {
  const normalizedDefs = normalizePresetDefs(defs)
  const defsByName = new Map(normalizedDefs.map(def => [def.name, def]))
  const defaults = normalizedDefs.map(def => (
    buildPresetListItem(def, DEFAULT_ENABLED_PRESETS.has(def.name), isApplicablePreset(def))
  ))
  if (!cfg) return normalizeMutexSelection(defaults)

  const rawPresets = cfg.scheduling_presets
  if (!Array.isArray(rawPresets) || rawPresets.length === 0) {
    if (cfg.scheduling_mode === 'lru' || cfg.lru_enabled === true) {
      return defaults.map(item => ({
        ...item,
        enabled: item.preset === 'lru',
      }))
    }
    return normalizeMutexSelection(defaults)
  }

  const first = rawPresets[0]
  if (isNewFormatPresetItem(first)) {
    const ordered: HydratedSchedulingPresetItem[] = []
    const seen = new Set<string>()

    for (const ci of rawPresets as SchedulingPresetConfigItemLike[]) {
      const presetName = normalizePresetName(ci.preset)
      const def = defsByName.get(presetName)
      if (!def || seen.has(presetName)) continue
      seen.add(presetName)
      ordered.push(buildPresetListItem(def, ci.enabled !== false, isApplicablePreset(def), ci.mode))
    }

    insertMissingByPreferredOrder(ordered, seen, normalizedDefs, defsByName, isApplicablePreset)
    return reorderDistributionGroup(normalizeMutexSelection(ordered))
  }

  const legacyPresets = rawPresets as unknown[]
  const legacyHasDistributionMode = legacySchedulingPresetIncludesDistributionMode(legacyPresets)
  const lruEnabled = cfg.lru_enabled ?? !legacyHasDistributionMode
  const ordered: HydratedSchedulingPresetItem[] = []
  const seen = new Set<string>()

  const lruDef = defsByName.get('lru')
  if (lruDef && lruEnabled) {
    ordered.push(buildPresetListItem(lruDef, lruEnabled, isApplicablePreset(lruDef)))
    seen.add('lru')
  } else if (!legacyHasDistributionMode) {
    const cacheAffinityDef = defsByName.get('cache_affinity')
    if (cacheAffinityDef) {
      ordered.push(buildPresetListItem(cacheAffinityDef, true, isApplicablePreset(cacheAffinityDef)))
      seen.add('cache_affinity')
    }
  }

  for (const name of legacyPresets) {
    const presetName = normalizePresetName(name)
    const def = defsByName.get(presetName)
    if (!def || seen.has(presetName)) continue
    seen.add(presetName)
    ordered.push(buildPresetListItem(def, true, isApplicablePreset(def), undefined))
  }

  insertMissingByPreferredOrder(ordered, seen, normalizedDefs, defsByName, isApplicablePreset)
  return reorderDistributionGroup(normalizeMutexSelection(ordered))
}

export function legacySchedulingPresetIncludesDistributionMode(presets: readonly unknown[]): boolean {
  const distributionPresets = new Set(['no_weight', 'lru', 'cache_affinity', 'load_balance', 'single_account'])
  return presets.some((preset) => {
    const normalized = String(preset ?? '').trim().toLowerCase()
    return distributionPresets.has(normalized)
  })
}

export function moveStrategyItem<T extends SchedulingDialogPresetLike>(
  items: readonly T[],
  itemIndex: number,
  direction: -1 | 1,
): T[] {
  const strategyIndexes: number[] = []

  items.forEach((item, index) => {
    if (!item.mutexGroup) {
      strategyIndexes.push(index)
    }
  })

  const currentPosition = strategyIndexes.indexOf(itemIndex)
  if (currentPosition === -1) {
    return [...items]
  }

  const targetPosition = currentPosition + direction
  if (targetPosition < 0 || targetPosition >= strategyIndexes.length) {
    return [...items]
  }

  const sourceIndex = strategyIndexes[currentPosition]
  const targetIndex = strategyIndexes[targetPosition]
  const nextItems = [...items]

  ;[nextItems[sourceIndex], nextItems[targetIndex]] = [nextItems[targetIndex], nextItems[sourceIndex]]

  return nextItems
}

export function normalizeMutexSelection<T extends SchedulingDialogPresetLike>(items: readonly T[]): T[] {
  const next = items.map(item => ({ ...item }))
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
      return item.enabled && (item.applicable ?? true)
    })
    const firstApplicable = indexes.find(index => next[index].applicable ?? true)
    const winner = enabledApplicable ?? firstApplicable ?? indexes[0]
    indexes.forEach((index) => {
      next[index].enabled = index === winner && (next[index].applicable ?? true)
    })
  }

  return next as T[]
}
