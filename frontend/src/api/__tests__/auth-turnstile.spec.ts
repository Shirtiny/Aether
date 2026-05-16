import { beforeEach, describe, expect, it, vi } from 'vitest'

const { postMock } = vi.hoisted(() => ({
  postMock: vi.fn(),
}))

vi.mock('@/api/client', () => ({
  default: {
    post: postMock,
  },
}))

import { authApi } from '@/api/auth'

describe('authApi turnstile payloads', () => {
  beforeEach(() => {
    postMock.mockReset()
    postMock.mockResolvedValue({ data: {} })
  })

  it('includes turnstile token when sending email verification code', async () => {
    await authApi.sendVerificationCode('alice@example.com', 'turnstile-token')

    expect(postMock).toHaveBeenCalledWith('/api/auth/send-verification-code', {
      email: 'alice@example.com',
      turnstile_token: 'turnstile-token',
    })
  })

  it('includes turnstile token when registering', async () => {
    await authApi.register({
      email: 'alice@example.com',
      username: 'alice',
      password: 'secret123',
      turnstile_token: 'turnstile-token',
    })

    expect(postMock).toHaveBeenCalledWith('/api/auth/register', {
      email: 'alice@example.com',
      username: 'alice',
      password: 'secret123',
      turnstile_token: 'turnstile-token',
    })
  })
})
