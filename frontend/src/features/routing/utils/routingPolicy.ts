export type RoutingPriorityMode = 'provider' | 'global_key'
export type RoutingSchedulingMode = 'fixed_order' | 'cache_affinity' | 'load_balance'
export type RoutingRulePhase = 'client_request' | 'provider_request'

export interface RoutingDefaultPolicy {
  priority_mode: RoutingPriorityMode
  scheduling_mode: RoutingSchedulingMode
  keep_priority_on_conversion: boolean
}

export interface RoutingPoolSchedulingPreset {
  preset: string
  enabled: boolean
  mode?: string | null
}

export interface RoutingPoolPolicyOverride {
  scheduling_presets: RoutingPoolSchedulingPreset[]
}

export interface RoutingModelPolicy {
  model: string
  allowed_providers: string[]
  allowed_keys: string[]
  provider_priority_overrides: Record<string, number>
  key_priority_overrides: Record<string, number>
  pool_priority_overrides: Record<string, number>
  pool_policy_overrides: Record<string, RoutingPoolPolicyOverride>
}

export interface RoutingRule {
  id: string
  priority: number
  enabled: boolean
  phase: RoutingRulePhase
  conditions: unknown
  actions: unknown[]
  stop_processing: boolean
}

export interface RoutingPredicateCondition {
  field: string
  op: 'eq' | 'prefix'
  value: string
}

export interface RoutingSetSchedulingAction {
  type: 'set_scheduling'
  priority_mode: RoutingPriorityMode
  scheduling_mode: RoutingSchedulingMode
}

export interface RoutingGroupConfig {
  allowed_models: string[]
  default_policy: RoutingDefaultPolicy
  model_policies: RoutingModelPolicy[]
  rules: RoutingRule[]
}

export const DEFAULT_ROUTING_POLICY_MODEL = '*'
export const MODEL_SCHEDULING_RULE_PREFIX = 'ui_model_scheduling:'

export function createEmptyRoutingGroupConfig(): RoutingGroupConfig {
  return {
    allowed_models: [],
    default_policy: {
      priority_mode: 'provider',
      scheduling_mode: 'cache_affinity',
      keep_priority_on_conversion: false,
    },
    model_policies: [],
    rules: [],
  }
}

export function createEmptyModelPolicy(model = ''): RoutingModelPolicy {
  return {
    model,
    allowed_providers: [],
    allowed_keys: [],
    provider_priority_overrides: {},
    key_priority_overrides: {},
    pool_priority_overrides: {},
    pool_policy_overrides: {},
  }
}

export function normalizeRoutingGroupConfig(value: Partial<RoutingGroupConfig> | null | undefined): RoutingGroupConfig {
  const base = createEmptyRoutingGroupConfig()

  return {
    allowed_models: Array.isArray(value?.allowed_models) ? [...value.allowed_models] : base.allowed_models,
    default_policy: {
      ...base.default_policy,
      ...(value?.default_policy ?? {}),
    },
    model_policies: Array.isArray(value?.model_policies)
      ? value.model_policies.map(policy => ({
          ...createEmptyModelPolicy(policy.model),
          ...policy,
          allowed_providers: Array.isArray(policy.allowed_providers) ? [...policy.allowed_providers] : [],
          allowed_keys: Array.isArray(policy.allowed_keys) ? [...policy.allowed_keys] : [],
          provider_priority_overrides: { ...(policy.provider_priority_overrides ?? {}) },
          key_priority_overrides: { ...(policy.key_priority_overrides ?? {}) },
          pool_priority_overrides: { ...(policy.pool_priority_overrides ?? {}) },
          pool_policy_overrides: { ...(policy.pool_policy_overrides ?? {}) },
        }))
      : base.model_policies,
    rules: Array.isArray(value?.rules) ? value.rules.map(rule => ({ ...rule })) : base.rules,
  }
}

export function upsertModelPolicy(config: RoutingGroupConfig, policy: RoutingModelPolicy): RoutingGroupConfig {
  const model = policy.model.trim()
  if (!model) {
    return normalizeRoutingGroupConfig(config)
  }

  const next = normalizeRoutingGroupConfig(config)
  const index = next.model_policies.findIndex(item => item.model === model)
  const normalizedPolicy = { ...createEmptyModelPolicy(model), ...policy, model }

  if (index >= 0) {
    next.model_policies[index] = normalizedPolicy
  } else {
    next.model_policies.push(normalizedPolicy)
  }

  return next
}

export function removeModelPolicy(config: RoutingGroupConfig, model: string): RoutingGroupConfig {
  const next = normalizeRoutingGroupConfig(config)
  next.model_policies = next.model_policies.filter(policy => policy.model !== model)
  return next
}

export function getDefaultModelPolicy(config: RoutingGroupConfig): RoutingModelPolicy {
  const normalized = normalizeRoutingGroupConfig(config)
  return normalized.model_policies.find(policy => policy.model === DEFAULT_ROUTING_POLICY_MODEL)
    ?? createEmptyModelPolicy(DEFAULT_ROUTING_POLICY_MODEL)
}

export function getModelPolicy(config: RoutingGroupConfig, model: string): RoutingModelPolicy {
  const normalizedModel = model.trim() || DEFAULT_ROUTING_POLICY_MODEL
  if (normalizedModel === DEFAULT_ROUTING_POLICY_MODEL) {
    return getDefaultModelPolicy(config)
  }
  const normalized = normalizeRoutingGroupConfig(config)
  return normalized.model_policies.find(policy => policy.model === normalizedModel)
    ?? createEmptyModelPolicy(normalizedModel)
}

export function upsertDefaultModelPolicy(
  config: RoutingGroupConfig,
  patch: Partial<Omit<RoutingModelPolicy, 'model'>>,
): RoutingGroupConfig {
  const current = getDefaultModelPolicy(config)
  const next = upsertModelPolicy(config, {
    ...current,
    ...patch,
    model: DEFAULT_ROUTING_POLICY_MODEL,
  })
  next.model_policies = [
    ...next.model_policies.filter(policy => policy.model === DEFAULT_ROUTING_POLICY_MODEL),
    ...next.model_policies.filter(policy => policy.model !== DEFAULT_ROUTING_POLICY_MODEL),
  ]
  return next
}

export function setDefaultProviderPriorityOverrides(
  config: RoutingGroupConfig,
  overrides: Record<string, number>,
): RoutingGroupConfig {
  return upsertDefaultModelPolicy(config, {
    provider_priority_overrides: normalizePriorityOverrides(overrides),
  })
}

export function setDefaultKeyPriorityOverrides(
  config: RoutingGroupConfig,
  overrides: Record<string, number>,
): RoutingGroupConfig {
  return upsertDefaultModelPolicy(config, {
    key_priority_overrides: normalizePriorityOverrides(overrides),
  })
}

export function setDefaultPoolPriorityOverrides(
  config: RoutingGroupConfig,
  overrides: Record<string, number>,
): RoutingGroupConfig {
  return upsertDefaultModelPolicy(config, {
    pool_priority_overrides: normalizePriorityOverrides(overrides),
  })
}

export function setModelProviderPriorityOverrides(
  config: RoutingGroupConfig,
  model: string,
  overrides: Record<string, number>,
): RoutingGroupConfig {
  const normalizedModel = model.trim() || DEFAULT_ROUTING_POLICY_MODEL
  if (normalizedModel === DEFAULT_ROUTING_POLICY_MODEL) {
    return setDefaultProviderPriorityOverrides(config, overrides)
  }
  return upsertModelPolicy(config, {
    ...getModelPolicy(config, normalizedModel),
    model: normalizedModel,
    provider_priority_overrides: normalizePriorityOverrides(overrides),
  })
}

export function setModelKeyPriorityOverrides(
  config: RoutingGroupConfig,
  model: string,
  overrides: Record<string, number>,
): RoutingGroupConfig {
  const normalizedModel = model.trim() || DEFAULT_ROUTING_POLICY_MODEL
  if (normalizedModel === DEFAULT_ROUTING_POLICY_MODEL) {
    return setDefaultKeyPriorityOverrides(config, overrides)
  }
  return upsertModelPolicy(config, {
    ...getModelPolicy(config, normalizedModel),
    model: normalizedModel,
    key_priority_overrides: normalizePriorityOverrides(overrides),
  })
}

export function setModelPoolPriorityOverrides(
  config: RoutingGroupConfig,
  model: string,
  overrides: Record<string, number>,
): RoutingGroupConfig {
  const normalizedModel = model.trim() || DEFAULT_ROUTING_POLICY_MODEL
  if (normalizedModel === DEFAULT_ROUTING_POLICY_MODEL) {
    return setDefaultPoolPriorityOverrides(config, overrides)
  }
  return upsertModelPolicy(config, {
    ...getModelPolicy(config, normalizedModel),
    model: normalizedModel,
    pool_priority_overrides: normalizePriorityOverrides(overrides),
  })
}

export function modelSchedulingRuleId(model: string): string {
  return `${MODEL_SCHEDULING_RULE_PREFIX}${encodeURIComponent(model.trim())}`
}

export function isGeneratedModelSchedulingRule(rule: RoutingRule): boolean {
  return rule.id.startsWith(MODEL_SCHEDULING_RULE_PREFIX)
}

export function modelPatternCondition(model: string): RoutingPredicateCondition {
  const normalizedModel = model.trim()
  if (normalizedModel.endsWith('*')) {
    return {
      field: 'model',
      op: 'prefix',
      value: normalizedModel.slice(0, -1),
    }
  }
  return {
    field: 'model',
    op: 'eq',
    value: normalizedModel,
  }
}

export function getModelScheduling(
  config: RoutingGroupConfig,
  model: string,
): RoutingDefaultPolicy {
  const normalized = normalizeRoutingGroupConfig(config)
  const rule = normalized.rules.find(rule => rule.id === modelSchedulingRuleId(model))
  const action = rule?.actions.find(isSetSchedulingAction)
  return {
    priority_mode: action?.priority_mode ?? normalized.default_policy.priority_mode,
    scheduling_mode: action?.scheduling_mode ?? normalized.default_policy.scheduling_mode,
    keep_priority_on_conversion: normalized.default_policy.keep_priority_on_conversion,
  }
}

export function upsertModelSchedulingRule(
  config: RoutingGroupConfig,
  model: string,
  scheduling: Pick<RoutingDefaultPolicy, 'priority_mode' | 'scheduling_mode'>,
): RoutingGroupConfig {
  const normalizedModel = model.trim()
  if (!normalizedModel || normalizedModel === DEFAULT_ROUTING_POLICY_MODEL) {
    return normalizeRoutingGroupConfig(config)
  }

  const next = normalizeRoutingGroupConfig(config)
  const rule: RoutingRule = {
    id: modelSchedulingRuleId(normalizedModel),
    priority: 10_000 + next.rules.filter(isGeneratedModelSchedulingRule).length,
    enabled: true,
    phase: 'client_request',
    conditions: modelPatternCondition(normalizedModel),
    actions: [{
      type: 'set_scheduling',
      priority_mode: scheduling.priority_mode,
      scheduling_mode: scheduling.scheduling_mode,
    } satisfies RoutingSetSchedulingAction],
    stop_processing: false,
  }

  const index = next.rules.findIndex(item => item.id === rule.id)
  if (index >= 0) {
    next.rules[index] = {
      ...next.rules[index],
      ...rule,
      priority: next.rules[index].priority,
    }
  } else {
    next.rules.push(rule)
  }
  return next
}

export function removeModelSchedulingRule(config: RoutingGroupConfig, model: string): RoutingGroupConfig {
  const ruleId = modelSchedulingRuleId(model)
  const next = normalizeRoutingGroupConfig(config)
  next.rules = next.rules.filter(rule => rule.id !== ruleId)
  return next
}

export function removeGeneratedModelSchedulingRules(config: RoutingGroupConfig): RoutingGroupConfig {
  const next = normalizeRoutingGroupConfig(config)
  next.rules = next.rules.filter(rule => !isGeneratedModelSchedulingRule(rule))
  return next
}

export function normalizePriorityOverrides(overrides: Record<string, number>): Record<string, number> {
  const normalized: Record<string, number> = {}
  for (const [rawId, rawPriority] of Object.entries(overrides)) {
    const id = rawId.trim()
    const priority = Math.max(0, Math.trunc(Number(rawPriority)))
    if (!id || !Number.isFinite(priority)) continue
    normalized[id] = priority
  }
  return normalized
}

function isSetSchedulingAction(action: unknown): action is RoutingSetSchedulingAction {
  if (!action || typeof action !== 'object') return false
  const candidate = action as Partial<RoutingSetSchedulingAction>
  return candidate.type === 'set_scheduling'
}
