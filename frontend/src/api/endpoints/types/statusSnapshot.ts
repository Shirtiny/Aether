export interface OAuthStatusSnapshot {
  code: 'none' | 'valid' | 'expiring' | 'expired' | 'invalid' | 'reauth_required' | 'check_failed'
  label?: string | null
  reason?: string | null
  expires_at?: number | null
  invalid_at?: number | null
  source?: string | null
  requires_reauth?: boolean
  usable_until_expiry?: boolean
  expiring_soon?: boolean
}

export interface AccountStatusSnapshot {
  code: string
  label?: string | null
  reason?: string | null
  blocked: boolean
  source?: string | null
  recoverable?: boolean
}

export interface QuotaWindowUsageSnapshot {
  source?: string | null
  request_count?: number | null
  total_tokens?: number | null
  total_cost_usd?: number | string | null
}

export interface QuotaWindowSnapshot {
  code: string
  label?: string | null
  scope?: 'account' | 'workspace' | 'model' | string
  unit?: 'percent' | 'count' | 'usd' | 'tokens' | string
  model?: string | null
  used_ratio?: number | null
  remaining_ratio?: number | null
  used_value?: number | null
  remaining_value?: number | null
  limit_value?: number | null
  /** Provider-native aliases retained by xAI quota observations. */
  remaining?: number | null
  limit?: number | null
  /** Indicates that the upstream value is a static ceiling, not a decrementing balance. */
  remaining_source?: string | null
  /** Locally settled lifetime usage shown separately from upstream quota semantics. */
  local_used_value?: number | null
  reset_at?: number | null
  reset_seconds?: number | null
  /** Start of the counted period, for windows that publish their own bounds. */
  window_start_at?: number | null
  window_minutes?: number | null
  is_exhausted?: boolean | null
  usage?: QuotaWindowUsageSnapshot | null
}

export interface QuotaCreditsSnapshot {
  has_credits?: boolean | null
  balance?: number | null
  available_count?: number | null
  remaining?: number | null
  consumed?: number | null
  total?: number | null
  unlimited?: boolean | null
  trace_id?: string | null
  updated_at?: number | null
}

/**
 * xAI billing view. Unlike the rate-limit windows — which a paid Grok account
 * reports as a static ceiling — this describes the subscription allowance that
 * actually runs out.
 */
export interface QuotaBillingSnapshot {
  period_type?: 'weekly' | 'monthly' | 'unknown' | string | null
  /** Weekly credit utilisation, 0-100. */
  usage_percent?: number | null
  /** Monthly subscription utilisation, 0-100. */
  used_percent?: number | null
  period_start?: string | null
  period_end?: string | null
  period_end_unix?: number | null
  monthly_limit_cents?: number | null
  used_cents?: number | null
  included_used_cents?: number | null
  on_demand_cap_cents?: number | null
  on_demand_used_cents?: number | null
  prepaid_balance_cents?: number | null
  billing_period_start?: string | null
  billing_period_end?: string | null
  plan?: string | null
  product_usage?: { product?: string | null; usage_percent?: number | null }[] | null
  weekly_status_code?: number | null
  monthly_status_code?: number | null
  /** True when either window could not be refreshed; the other keeps its last value. */
  partial?: boolean | null
  failed_windows?: string[] | null
  observed_at?: number | null
  source?: string | null
}

export interface QuotaStatusSnapshot {
  version?: number | null
  provider_type?: string | null
  code: 'unknown' | 'ok' | 'exhausted' | 'cooldown' | 'forbidden' | 'banned' | string
  label?: string | null
  reason?: string | null
  freshness?: 'fresh' | 'stale' | 'unknown' | 'error' | string | null
  source?: string | null
  usage_source?: string | null
  observed_at?: number | null
  exhausted: boolean
  usage_ratio?: number | null
  updated_at?: number | null
  reset_at?: number | null
  reset_seconds?: number | null
  plan_type?: string | null
  pool_tier?: string | null
  credits?: QuotaCreditsSnapshot | null
  allowed_models_count?: number | null
  rate_limit?: Record<string, unknown> | null
  billing?: QuotaBillingSnapshot | null
  windows?: QuotaWindowSnapshot[] | null
}

export interface ProviderKeyStatusSnapshot {
  oauth: OAuthStatusSnapshot
  account: AccountStatusSnapshot
  quota: QuotaStatusSnapshot
}
