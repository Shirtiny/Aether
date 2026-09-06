import { ref } from 'vue'
import { isAxiosError } from 'axios'
import { adminApi } from '@/api/admin'
import { useToast } from '@/composables/useToast'
import { parseApiError } from '@/utils/errorParser'
import {
  emptyModelTestTemplates,
  MODEL_TEST_TEMPLATES_CONFIG_KEY,
  parseModelTestTemplates,
  type ModelTestRequestTemplate,
  type ModelTestRequestTemplates,
  type ModelTestTemplateKind,
} from './model-test-templates'

async function fetchTemplates(): Promise<ModelTestRequestTemplates> {
  try {
    const response = await adminApi.getSystemConfig(MODEL_TEST_TEMPLATES_CONFIG_KEY)
    return parseModelTestTemplates(response.value)
  } catch (error) {
    // Older installations do not yet return a default for this config key.
    if (isAxiosError(error) && error.response?.status === 404) return emptyModelTestTemplates()
    throw error
  }
}

export function useModelTestTemplates() {
  const templates = ref(emptyModelTestTemplates())
  const loading = ref(false)
  const saving = ref(false)
  const ready = ref(false)
  const loadError = ref('')
  const { success, error: showError } = useToast()

  async function load() {
    if (loading.value || saving.value) return
    loading.value = true
    ready.value = false
    loadError.value = ''
    try {
      templates.value = await fetchTemplates()
      ready.value = true
    } catch (error) {
      loadError.value = parseApiError(error, '全局请求模板加载失败')
    } finally {
      loading.value = false
    }
  }

  async function persist(change: (latest: ModelTestRequestTemplates) => void): Promise<boolean> {
    if (!ready.value || loading.value || saving.value) return false
    saving.value = true
    try {
      // Merge a single edit into the latest list, preserving other kinds and newly added templates.
      const latest = await fetchTemplates()
      change(latest)
      const response = await adminApi.updateSystemConfig(
        MODEL_TEST_TEMPLATES_CONFIG_KEY,
        latest,
        '模型测试全局请求模板（请求头、请求体）',
      )
      templates.value = parseModelTestTemplates(response.value)
      success('全局请求模板已保存')
      return true
    } catch (error) {
      showError(parseApiError(error, '全局请求模板保存失败'))
      return false
    } finally {
      saving.value = false
    }
  }

  async function save(kind: ModelTestTemplateKind, template: ModelTestRequestTemplate, isNew: boolean) {
    return persist((latest) => {
      const list = latest[kind]
      const index = list.findIndex(item => item.id === template.id)
      if (!isNew && index < 0) throw new Error('此模板已被删除，请刷新后重试')
      if (list.some(item => item.id !== template.id && item.name.toLowerCase() === template.name.toLowerCase())) {
        throw new Error('同类模板名称不能重复')
      }
      if (isNew) list.push(template)
      else list.splice(index, 1, template)
    })
  }

  async function remove(kind: ModelTestTemplateKind, id: string) {
    return persist((latest) => {
      latest[kind] = latest[kind].filter(item => item.id !== id)
    })
  }

  return { templates, loading, saving, ready, loadError, load, save, remove }
}
