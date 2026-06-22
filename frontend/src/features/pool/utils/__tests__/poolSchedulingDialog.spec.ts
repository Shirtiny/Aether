import { describe, expect, it } from 'vitest'

import {
  hydrateSchedulingPresetList,
  legacySchedulingPresetIncludesDistributionMode,
  moveStrategyItem,
  normalizeMutexSelection,
} from '@/features/pool/utils/poolSchedulingDialog'

interface TestPresetItem {
  preset: string
  mutexGroup: string | null
  enabled: boolean
}

function buildItems(): TestPresetItem[] {
  return [
    { preset: 'no_weight', mutexGroup: 'distribution_mode', enabled: false },
    { preset: 'cache_affinity', mutexGroup: 'distribution_mode', enabled: false },
    { preset: 'lru', mutexGroup: 'distribution_mode', enabled: true },
    { preset: 'single_account', mutexGroup: 'distribution_mode', enabled: false },
    { preset: 'load_balance', mutexGroup: 'distribution_mode', enabled: false },
    { preset: 'recent_refresh', mutexGroup: null, enabled: true },
    { preset: 'quota_balanced', mutexGroup: null, enabled: false },
    { preset: 'priority_first', mutexGroup: null, enabled: true },
  ]
}

describe('poolSchedulingDialog', () => {
  it('moves only strategy items upward without disturbing distribution presets', () => {
    const moved = moveStrategyItem(buildItems(), 7, -1)

    expect(moved.map(item => item.preset)).toEqual([
      'no_weight',
      'cache_affinity',
      'lru',
      'single_account',
      'load_balance',
      'recent_refresh',
      'priority_first',
      'quota_balanced',
    ])
  })

  it('keeps the original order when a strategy item is already at the top boundary', () => {
    const original = buildItems()
    const moved = moveStrategyItem(original, 5, -1)

    expect(moved.map(item => item.preset)).toEqual(original.map(item => item.preset))
  })

  it('moves a strategy item downward within the strategy group', () => {
    const moved = moveStrategyItem(buildItems(), 5, 1)

    expect(moved.map(item => item.preset)).toEqual([
      'no_weight',
      'cache_affinity',
      'lru',
      'single_account',
      'load_balance',
      'quota_balanced',
      'recent_refresh',
      'priority_first',
    ])
  })

  it('keeps the original order when a strategy item is already at the bottom boundary', () => {
    const original = buildItems()
    const moved = moveStrategyItem(original, 7, 1)

    expect(moved.map(item => item.preset)).toEqual(original.map(item => item.preset))
  })

  it('keeps the original order when the target item is not a strategy preset', () => {
    const original = buildItems()
    const moved = moveStrategyItem(original, 1, 1)

    expect(moved.map(item => item.preset)).toEqual(original.map(item => item.preset))
  })

  it('keeps the first enabled mutex item from loaded config order', () => {
    const normalized = normalizeMutexSelection([
      { preset: 'cache_affinity', mutexGroup: 'distribution_mode', enabled: true },
      { preset: 'no_weight', mutexGroup: 'distribution_mode', enabled: true },
      { preset: 'lru', mutexGroup: 'distribution_mode', enabled: false },
    ])

    expect(normalized.map(item => [item.preset, item.enabled])).toEqual([
      ['cache_affinity', true],
      ['no_weight', false],
      ['lru', false],
    ])
  })

  it('skips non-applicable mutex items when choosing a winner', () => {
    const normalized = normalizeMutexSelection([
      { preset: 'cache_affinity', mutexGroup: 'distribution_mode', enabled: true, applicable: false },
      { preset: 'no_weight', mutexGroup: 'distribution_mode', enabled: true, applicable: true },
      { preset: 'lru', mutexGroup: 'distribution_mode', enabled: false, applicable: true },
    ])

    expect(normalized.map(item => [item.preset, item.enabled])).toEqual([
      ['cache_affinity', false],
      ['no_weight', true],
      ['lru', false],
    ])
  })

  it('detects explicit legacy distribution presets', () => {
    expect(legacySchedulingPresetIncludesDistributionMode(['cache_affinity', 'priority_first'])).toBe(true)
    expect(legacySchedulingPresetIncludesDistributionMode([' no_weight '])).toBe(true)
    expect(legacySchedulingPresetIncludesDistributionMode(['recent_refresh', 'priority_first'])).toBe(false)
    expect(legacySchedulingPresetIncludesDistributionMode([null, ''])).toBe(false)
  })

  it('hydrates legacy non-LRU configs as cache affinity rather than no weight', () => {
    const hydrated = hydrateSchedulingPresetList({ lru_enabled: false })

    expect(hydrated.find(item => item.preset === 'cache_affinity')?.enabled).toBe(true)
    expect(hydrated.find(item => item.preset === 'no_weight')?.enabled).toBe(false)
    expect(hydrated.find(item => item.preset === 'lru')?.enabled).toBe(false)
  })

  it('hydrates explicit legacy distribution presets without overriding them', () => {
    for (const preset of ['cache_affinity', 'load_balance', 'single_account'] as const) {
      const hydrated = hydrateSchedulingPresetList({
        lru_enabled: false,
        scheduling_presets: [preset, 'priority_first'],
      })

      expect(hydrated.find(item => item.preset === preset)?.enabled).toBe(true)
      expect(hydrated.find(item => item.preset === 'no_weight')?.enabled).toBe(false)
      expect(hydrated.find(item => item.preset === 'lru')?.enabled).toBe(false)
      expect(hydrated.find(item => item.preset === 'priority_first')?.enabled).toBe(true)
    }
  })

  it('hydrates explicit legacy no-weight presets as no weight', () => {
    const hydrated = hydrateSchedulingPresetList({
      lru_enabled: false,
      scheduling_presets: ['no_weight', 'priority_first'],
    })

    expect(hydrated.find(item => item.preset === 'no_weight')?.enabled).toBe(true)
    expect(hydrated.find(item => item.preset === 'cache_affinity')?.enabled).toBe(false)
    expect(hydrated.find(item => item.preset === 'priority_first')?.enabled).toBe(true)
  })
})
