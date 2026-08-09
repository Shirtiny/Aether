import { describe, expect, it } from 'vitest'

import {
  canRefreshOAuthCredential,
  getProviderMaskedSecretLabel,
  shouldShowOAuthRefreshControl,
} from '@/utils/providerKeyAuth'

describe('providerKeyAuth', () => {
  it('renders refreshable Grok OAuth credentials as OAuth tokens with refresh controls', () => {
    const key = {
      auth_type: 'oauth',
      oauth_managed: true,
      can_refresh_oauth: true,
    }

    expect(getProviderMaskedSecretLabel(key, 'grok')).toBe('[OAuth Token]')
    expect(shouldShowOAuthRefreshControl(key, 'grok')).toBe(true)
  })

  it('keeps legacy non-refreshable Grok session cookies labeled as sessions', () => {
    const key = {
      auth_type: 'oauth',
      oauth_managed: true,
      oauth_temporary: false,
      can_refresh_oauth: false,
    }

    expect(getProviderMaskedSecretLabel(key, 'grok')).toBe('[Session Cookie]')
    expect(shouldShowOAuthRefreshControl(key, 'grok')).toBe(false)
  })

  it('does not mislabel temporary access-token imports as Session Cookies', () => {
    const key = {
      auth_type: 'oauth',
      oauth_managed: true,
      oauth_temporary: true,
      can_refresh_oauth: false,
    }

    expect(getProviderMaskedSecretLabel(key, 'grok')).toBe('[OAuth Token]')
    expect(shouldShowOAuthRefreshControl(key, 'grok')).toBe(false)
  })

  it('hides oauth refresh control when backend marks a provider as non-refreshable', () => {
    const input = {
      auth_type: 'oauth',
      oauth_managed: true,
      can_refresh_oauth: false,
    }

    expect(canRefreshOAuthCredential(input)).toBe(false)
    expect(shouldShowOAuthRefreshControl(input)).toBe(false)
  })

  it('keeps legacy oauth refresh control visible when backend capability is absent', () => {
    const input = {
      auth_type: 'oauth',
      oauth_managed: true,
    }

    expect(canRefreshOAuthCredential(input)).toBe(true)
    expect(shouldShowOAuthRefreshControl(input)).toBe(true)
    expect(getProviderMaskedSecretLabel(input, 'codex')).toBe('[OAuth Token]')
  })
})
