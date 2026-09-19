import type { PoolAdvancedConfig, ProviderWithEndpointsSummary, TurnStateCollectionConfig } from '@/api/endpoints'

export const TURN_STATE_DISABLED = '__disabled__'
export interface TurnStateSourceOption {
  id: string
  name: string
}

export function turnStateSourceOptions(providers: ProviderWithEndpointsSummary[]): TurnStateSourceOption[] {
  return providers.filter(provider => provider.is_active).flatMap(provider => [
    ...(provider.turn_state_collection?.enabled
      ? [{ id: provider.id, name: `提供商 · ${provider.name}` }]
      : []),
    ...(provider.turn_state_account_sources ?? []).map(key => ({
      id: `account:${provider.id}:${key.key_id}`,
      name: `账号 · ${provider.name} / ${key.name}`,
    })),
  ])
}

export function selectedTurnStateSource(config?: PoolAdvancedConfig | null): string {
  if (!config?.turn_state_source_provider_id) return TURN_STATE_DISABLED
  return config.turn_state_source_key_id
    ? `account:${config.turn_state_source_provider_id}:${config.turn_state_source_key_id}`
    : config.turn_state_source_provider_id
}

export function turnStateSourceConfig(value: string) {
  if (value === TURN_STATE_DISABLED) return { turn_state_source_provider_id: null, turn_state_source_key_id: null }
  if (value.startsWith('account:')) {
    const [, providerId, keyId] = value.split(':')
    return { turn_state_source_provider_id: providerId, turn_state_source_key_id: keyId }
  }
  return { turn_state_source_provider_id: value, turn_state_source_key_id: null }
}

export function turnStateCollectionConfig(enabled: boolean, modelsText: string): TurnStateCollectionConfig {
  const models = [...new Set(modelsText.split(/[\n,，]+/).map(model => model.trim()).filter(Boolean))]
  if (enabled && models.length === 0) throw new Error('启用 Turn-State 采集时请填写模型列表')
  if (models.length > 64 || models.some(model => new TextEncoder().encode(model).length > 200 || /[\p{Cc}]/u.test(model))) {
    throw new Error('采集模型最多 64 个，每个名称不超过 200 字节且不能包含控制字符')
  }
  return { enabled, models }
}
