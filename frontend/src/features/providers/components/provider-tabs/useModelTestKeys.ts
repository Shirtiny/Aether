import { computed, onScopeDispose, ref, watch } from 'vue'
import { getProviderKeys, type EndpointAPIKey } from '@/api/endpoints/keys'
import { useToast } from '@/composables/useToast'
import { parseApiError } from '@/utils/errorParser'
import { modelTestKeySupportsEndpoint } from './model-test-request'

export function useModelTestKeys(options: {
  providerId: () => string
  providerType: () => string | null | undefined
  endpoint: () => { id: string; api_format: string } | null
  fallbackKeys: () => EndpointAPIKey[]
}) {
  const keys = ref<EndpointAPIKey[]>([])
  const loadedProviderId = ref<string | null>(null)
  const loading = ref(false)
  const loadError = ref('')
  const selectedIds = ref<string[]>([])
  const { error: showError } = useToast()
  let generation = 0

  const keyOptions = computed(() => {
    const endpoint = options.endpoint()
    if (!endpoint) return []
    const seen = new Set<string>()
    const available = loadedProviderId.value === options.providerId() ? keys.value : options.fallbackKeys()
    return [...available]
      .filter(key => {
        if (seen.has(key.id)) return false
        seen.add(key.id)
        return modelTestKeySupportsEndpoint(key, endpoint, options.providerType())
      })
      .sort((left, right) => left.internal_priority - right.internal_priority || label(left).localeCompare(label(right)))
      .map(key => ({ value: key.id, label: label(key) }))
  })

  function label(key: EndpointAPIKey): string {
    const primary = key.name?.trim() || key.api_key_masked?.trim() || key.id
    const suffix = [key.api_key_masked?.trim() !== primary ? key.api_key_masked?.trim() : '', key.auth_type?.trim()].filter(Boolean)
    return suffix.length ? `${primary} · ${suffix.join(' · ')}` : primary
  }

  function select(ids: string[]) {
    const allowed = new Set(keyOptions.value.map(option => option.value))
    selectedIds.value = [...new Set(ids.map(id => id.trim()).filter(id => allowed.has(id)))]
  }

  async function load() {
    const providerId = options.providerId()
    if (loading.value || loadedProviderId.value === providerId) return
    const token = ++generation
    loading.value = true
    loadError.value = ''
    try {
      const fetched = await getProviderKeys(providerId)
      if (generation !== token || options.providerId() !== providerId) return
      keys.value = fetched
      loadedProviderId.value = providerId
    } catch (error) {
      if (generation === token) {
        loadError.value = parseApiError(error, '加载测试 Key 失败')
        showError(loadError.value)
      }
    } finally {
      if (generation === token) loading.value = false
    }
  }

  watch(keyOptions, () => select(selectedIds.value))
  watch(options.providerId, () => {
    generation += 1
    keys.value = []
    loadedProviderId.value = null
    loading.value = false
    loadError.value = ''
    selectedIds.value = []
  })
  onScopeDispose(() => { generation += 1 })

  return { selectedIds, keyOptions, loading, loadError, select, load }
}
