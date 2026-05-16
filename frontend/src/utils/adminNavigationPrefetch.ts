import { adminWalletApi } from '@/api/admin-wallets'
import { adminApi } from '@/api/admin'
import { adminBillingPlansApi, epayGatewayApi } from '@/api/billing'
import { getProvidersSummary } from '@/api/endpoints/providers'
import { getPoolOverview, getPoolSchedulingPresets, listPoolKeys } from '@/api/endpoints/pool'
import { listGlobalModels } from '@/api/global-models'
import { usersApi } from '@/api/users'
import { log } from '@/utils/logger'

const NAV_DATA_CACHE_TTL_MS = 10 * 1000
const NAV_SYSTEM_CONFIG_CACHE_TTL_MS = 30 * 1000
const NAV_POOL_PRESETS_CACHE_TTL_MS = 5 * 60 * 1000
const PREFETCH_COOLDOWN_MS = 5 * 1000

const lastPrefetchAt = new Map<string, number>()

const adminRouteWarmers: Record<string, () => Promise<void>> = {
  '/admin/users': async () => {
    await Promise.allSettled([
      import('@/views/admin/Users.vue'),
      usersApi.getAllUsers({ cacheTtlMs: NAV_DATA_CACHE_TTL_MS }),
      adminWalletApi.listAllWallets(
        { owner_type: 'user' },
        { cacheTtlMs: NAV_DATA_CACHE_TTL_MS },
      ),
    ])
  },
  '/admin/providers': async () => {
    await Promise.allSettled([
      import('@/views/admin/ProviderManagement.vue'),
      getProvidersSummary(
        { page: 1, page_size: 20 },
        { cacheTtlMs: NAV_DATA_CACHE_TTL_MS },
      ),
      adminApi.getSystemConfig('provider_priority_mode', {
        cacheTtlMs: NAV_SYSTEM_CONFIG_CACHE_TTL_MS,
      }),
      listGlobalModels(
        { is_active: true, limit: 1000 },
        { cacheTtlMs: NAV_DATA_CACHE_TTL_MS },
      ),
    ])
  },
  '/admin/models': async () => {
    await Promise.allSettled([
      import('@/views/admin/ModelManagement.vue'),
      listGlobalModels(
        { skip: 0, limit: 20 },
        { cacheTtlMs: NAV_DATA_CACHE_TTL_MS },
      ),
    ])
  },
  '/admin/pool': async () => {
    const [overviewResult] = await Promise.allSettled([
      getPoolOverview({ cacheTtlMs: NAV_DATA_CACHE_TTL_MS }),
      getPoolSchedulingPresets({ cacheTtlMs: NAV_POOL_PRESETS_CACHE_TTL_MS }),
      import('@/views/admin/PoolManagement.vue'),
    ])

    if (overviewResult.status !== 'fulfilled') {
      return
    }

    const firstProviderId = overviewResult.value.items.find(item => item.pool_enabled)?.provider_id
    if (!firstProviderId) {
      return
    }

    await listPoolKeys(
      firstProviderId,
      { page: 1, page_size: 50, status: 'all' },
      { cacheTtlMs: NAV_DATA_CACHE_TTL_MS },
    )
  },
  '/admin/payment-gateways': async () => {
    await Promise.allSettled([
      import('@/views/admin/PaymentGatewaySettings.vue'),
      epayGatewayApi.get(),
    ])
  },
  '/admin/billing-plans': async () => {
    await Promise.allSettled([
      import('@/views/admin/BillingPlansManagement.vue'),
      adminBillingPlansApi.list(),
    ])
  },
}

export function prefetchAdminNavigationTarget(href: string): void {
  const warmer = adminRouteWarmers[href]
  if (!warmer) return

  const now = Date.now()
  const lastRun = lastPrefetchAt.get(href) ?? 0
  if (now - lastRun < PREFETCH_COOLDOWN_MS) {
    return
  }
  lastPrefetchAt.set(href, now)

  void warmer().catch((err) => {
    log.debug('[adminNavigationPrefetch] ignore prefetch failure', err)
  })
}
