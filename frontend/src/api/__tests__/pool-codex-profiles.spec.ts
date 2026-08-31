import { beforeEach, describe, expect, it, vi } from 'vitest'

const { postMock } = vi.hoisted(() => ({
  postMock: vi.fn(),
}))

vi.mock('@/api/client', () => ({
  default: {
    post: postMock,
  },
}))

import { refreshCodexPoolClientProfiles } from '@/api/endpoints/pool'

describe('Codex pool client profile refresh', () => {
  beforeEach(() => {
    postMock.mockReset()
    postMock.mockResolvedValue({
      data: { affected: 2, message: '2 keys Codex client profiles refreshed' },
    })
  })

  it('uses the pool batch action for the selected keys', async () => {
    const clientHeaders = {
      enabled: true,
      profiles: [{ user_agent: 'codex-tui/0.151.0', originator: 'codex-tui' }],
    }
    const result = await refreshCodexPoolClientProfiles(
      'provider-codex',
      ['key-a', 'key-b'],
      clientHeaders,
    )

    expect(postMock).toHaveBeenCalledWith(
      '/api/admin/pool/provider-codex/keys/batch-action',
      {
        key_ids: ['key-a', 'key-b'],
        action: 'refresh_codex_client_profiles',
        payload: { codex_client_headers: clientHeaders },
      },
      { timeout: 5 * 60 * 1000 },
    )
    expect(result.affected).toBe(2)
  })
})
