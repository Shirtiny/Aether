import type { ModelCreate, ModelUpdate, TieredPricingConfig } from '@/api/endpoints'

interface EmbeddingMetadataCarrier {
  supported_capabilities?: string[] | null
  supports_embedding?: boolean | null
  effective_supports_embedding?: boolean | null
  config?: Record<string, unknown> | null
}

function isEmbeddingApiFormat(format: unknown): boolean {
  const value = String(format).trim().toLowerCase()
  return value.endsWith(':embedding') || value === 'aliyun:multimodal_embedding'
}

export interface ProviderModelCreatePayloadInput {
  globalModelId: string
  providerModelName: string
  finalTieredPricing: TieredPricingConfig | null
  tieredPricingModified: boolean
  pricePerRequest?: number
  cleanConfig?: Record<string, unknown>
  configTouched: boolean
  supportsVision?: boolean
  supportsFunctionCalling?: boolean
  supportsStreaming?: boolean
  supportsExtendedThinking?: boolean
  supportsImageGeneration?: boolean
  isActive: boolean
}

export interface ProviderModelUpdatePayloadInput {
  finalTieredPricing: TieredPricingConfig | null
  pricePerRequest?: number
  cleanConfig?: Record<string, unknown>
  supportsVision?: boolean
  supportsFunctionCalling?: boolean
  supportsStreaming?: boolean
  supportsExtendedThinking?: boolean
  supportsImageGeneration?: boolean
  isActive: boolean
}

export function modelSupportsEmbedding(model: EmbeddingMetadataCarrier | null | undefined): boolean {
  if (!model) return false
  if ('effective_supports_embedding' in model && model.effective_supports_embedding === true) return true
  if ('supports_embedding' in model && model.supports_embedding === true) return true

  const supportedCapabilities = 'supported_capabilities' in model ? model.supported_capabilities : null
  const config = model.config || {}
  return supportedCapabilities?.includes('embedding') === true
    || config.embedding === true
    || config.model_type === 'embedding'
    || (Array.isArray(config.api_formats) && config.api_formats.some(isEmbeddingApiFormat))
}

export function buildProviderModelCreatePayload(input: ProviderModelCreatePayloadInput): ModelCreate {
  return {
    global_model_id: input.globalModelId,
    provider_model_name: input.providerModelName,
    tiered_pricing: input.tieredPricingModified && input.finalTieredPricing ? input.finalTieredPricing : undefined,
    price_per_request: input.pricePerRequest,
    config: input.configTouched ? input.cleanConfig : undefined,
    supports_vision: input.supportsVision,
    supports_function_calling: input.supportsFunctionCalling,
    supports_streaming: input.supportsStreaming,
    supports_extended_thinking: input.supportsExtendedThinking,
    supports_image_generation: input.supportsImageGeneration,
    is_active: input.isActive,
  }
}

export function buildProviderModelUpdatePayload(input: ProviderModelUpdatePayloadInput): ModelUpdate {
  return {
    tiered_pricing: input.finalTieredPricing,
    price_per_request: input.pricePerRequest ?? null,
    config: input.cleanConfig || null,
    supports_vision: input.supportsVision,
    supports_function_calling: input.supportsFunctionCalling,
    supports_streaming: input.supportsStreaming,
    supports_extended_thinking: input.supportsExtendedThinking,
    supports_image_generation: input.supportsImageGeneration,
    is_active: input.isActive,
  }
}
