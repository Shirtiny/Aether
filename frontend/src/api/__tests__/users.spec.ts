import { beforeEach, describe, expect, it, vi } from 'vitest'

const { getMock, cachedRequestMock } = vi.hoisted(() => ({
  getMock: vi.fn(),
  cachedRequestMock: vi.fn(async (_key: string, fn: () => Promise<unknown>) => fn()),
}))

vi.mock('@/api/client', () => ({
  default: {
    get: getMock,
  },
}))

vi.mock('@/utils/cache', () => ({
  cachedRequest: cachedRequestMock,
}))

import { usersApi } from '@/api/users'

describe('usersApi admin list query', () => {
  beforeEach(() => {
    getMock.mockReset()
    cachedRequestMock.mockClear()
    getMock.mockResolvedValue({
      data: {
        items: [],
        total: 0,
        skip: 0,
        limit: 20,
        has_more: false,
      },
    })
  })

  it('passes creation-time sort parameters to the admin users endpoint', async () => {
    await usersApi.getAllUsersPage({
      skip: 20,
      limit: 10,
      sort_by: 'created_at',
      sort_order: 'desc',
    })

    expect(getMock).toHaveBeenCalledWith('/api/admin/users', {
      params: {
        skip: 20,
        limit: 10,
        sort_by: 'created_at',
        sort_order: 'desc',
      },
    })
  })
})
