import { describe, expect, it } from 'vitest'

import {
  getQuotaSegments,
  hasNoFiveHourLimit,
  hasNoWeeklyLimit,
  isDepletedQuotaSegment,
} from '../quota-selectors'

describe('quota selectors', () => {
  it('splits quota text into normalized segments', () => {
    expect(getQuotaSegments('周剩余 0.0% | 5H剩余 100.0％')).toEqual([
      '周剩余 0.0%',
      '5h剩余 100.0%',
    ])
  })

  it('treats exact 0 percent as depleted', () => {
    expect(isDepletedQuotaSegment('周剩余 0.0%（5天后重置）')).toBe(true)
    expect(isDepletedQuotaSegment('5h剩余 0%')).toBe(true)
  })

  it('does not treat non-zero decimal percentages as depleted', () => {
    expect(isDepletedQuotaSegment('周45.0% 5d20h')).toBe(false)
    expect(isDepletedQuotaSegment('周17.0% 5d20h')).toBe(false)
    expect(isDepletedQuotaSegment('5h93.0% 2h')).toBe(false)
  })

  it('detects only depleted weekly segments', () => {
    expect(hasNoWeeklyLimit('周剩余 0.0%（5天后重置） | 5H剩余 93.0%（2小时后重置）')).toBe(true)
    expect(hasNoWeeklyLimit('周剩余 45.0%（5天后重置） | 5H剩余 0.0%（2小时后重置）')).toBe(false)
  })

  it('detects only depleted 5h segments', () => {
    expect(hasNoFiveHourLimit('周剩余 45.0%（5天后重置） | 5H剩余 0.0%（2小时后重置）')).toBe(true)
    expect(hasNoFiveHourLimit('周剩余 0.0%（5天后重置） | 5H剩余 93.0%（2小时后重置）')).toBe(false)
  })
})
