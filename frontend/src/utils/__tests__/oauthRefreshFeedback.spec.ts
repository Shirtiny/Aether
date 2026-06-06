import { describe, expect, it } from 'vitest'

import {
  getOAuthRefreshFeedback,
  resolveOAuthAccountBlockDisplay,
} from '@/utils/oauthRefreshFeedback'

describe('oauthRefreshFeedback', () => {
  it('prefers explicit blocked account status from pool data', () => {
    expect(
      resolveOAuthAccountBlockDisplay({
        status_snapshot: {
          oauth: { code: 'valid' },
          account: {
            code: 'account_disabled',
            label: '账号停用',
            reason: 'account has been deactivated',
            blocked: true,
          },
          quota: { code: 'unknown', exhausted: false },
        },
      }),
    ).toEqual({
      label: '账号停用',
      reason: 'account has been deactivated',
    })
  })

  it('classifies account-level OAuth invalid reasons', () => {
    expect(
      resolveOAuthAccountBlockDisplay({
        oauth_invalid_reason: '[ACCOUNT_BLOCK] account has been deactivated',
      }),
    ).toEqual({
      label: '账号停用',
      reason: 'account has been deactivated',
    })
  })

  it('ignores recoverable refresh failures', () => {
    expect(
      resolveOAuthAccountBlockDisplay({
        oauth_invalid_reason: '[REFRESH_FAILED] Token 续期失败',
      }),
    ).toEqual({
      label: null,
      reason: null,
    })
  })

  it('reports blocked result after successful recheck', () => {
    expect(
      getOAuthRefreshFeedback({
        accountStateRecheckAttempted: true,
        snapshot: {
          status_snapshot: {
            oauth: { code: 'valid' },
            account: {
              code: 'account_disabled',
              label: '账号停用',
              reason: 'account has been deactivated',
              blocked: true,
            },
            quota: { code: 'unknown', exhausted: false },
          },
        },
      }),
    ).toEqual({
      tone: 'warning',
      message: 'Token 刷新成功，已重新检查额度/账号状态；当前状态仍是账号停用',
    })
  })

  it('reports plain success when no blocked state remains', () => {
    expect(
      getOAuthRefreshFeedback({
        accountStateRecheckAttempted: true,
        accountStateRecheckError: null,
      }),
    ).toEqual({
      tone: 'success',
      message: 'Token 刷新成功，已重新检查额度/账号状态',
    })
  })

  it('includes the recheck failure detail after a successful token refresh', () => {
    expect(
      getOAuthRefreshFeedback({
        accountStateRecheckAttempted: true,
        accountStateRecheckError: 'wham/usage API 返回状态码 401',
      }),
    ).toEqual({
      tone: 'warning',
      message: 'Token 刷新成功，但额度/账号状态复检失败：wham/usage API 返回状态码 401',
    })
  })
})
