import { normalizeApiFormatAlias } from '@/api/endpoints/types/api-format'
import {
  parseModelTestRequestBodyDraft,
  parseModelTestRequestHeadersDraft,
} from './model-test-request'

export const MODEL_TEST_TEMPLATES_CONFIG_KEY = 'model_test_request_templates'
export const MODEL_TEST_MODEL_PLACEHOLDER = '{{model}}'

export type ModelTestTemplateKind = 'headers' | 'body'

export interface ModelTestRequestTemplate {
  id: string
  name: string
  api_format: string | null
  content: Record<string, unknown>
}

export type ModelTestRequestTemplates = Record<ModelTestTemplateKind, ModelTestRequestTemplate[]>

export function emptyModelTestTemplates(): ModelTestRequestTemplates {
  return { headers: [], body: [] }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

// A failed/malformed read must not turn into an empty list that can overwrite saved templates.
export function parseModelTestTemplates(value: unknown): ModelTestRequestTemplates {
  if (!isObject(value)) throw new Error('全局请求模板格式无效')
  for (const kind of ['headers', 'body'] as const) {
    const templates = value[kind]
    if (!Array.isArray(templates) || templates.some(template => (
      !isObject(template)
      || typeof template.id !== 'string' || !template.id.trim()
      || typeof template.name !== 'string' || !template.name.trim()
      || !(template.api_format === null || typeof template.api_format === 'string')
      || !isObject(template.content)
    ))) {
      throw new Error('全局请求模板格式无效')
    }
  }
  return value as ModelTestRequestTemplates
}

export function modelTestTemplateMatchesFormat(template: ModelTestRequestTemplate, apiFormat?: string | null): boolean {
  return !template.api_format
    || normalizeApiFormatAlias(template.api_format) === normalizeApiFormatAlias(apiFormat ?? '')
}

export function parseModelTestTemplateDraft(kind: ModelTestTemplateKind, draft: string) {
  const parsed = kind === 'headers'
    ? parseModelTestRequestHeadersDraft(draft)
    : parseModelTestRequestBodyDraft(draft)
  if (kind === 'headers' && parsed.value) {
    for (const [name, value] of Object.entries(parsed.value)) {
      if (!/^[!#$%&'*+.^_`|~0-9a-z-]+$/i.test(name)
        || !['string', 'number', 'boolean'].includes(typeof value)
        || /[\r\n\0]/.test(String(value))) {
        return { value: null, error: '请求头名称必须合法，值必须是无换行的字符串、数字或布尔值' }
      }
    }
  }
  return parsed
}

export function buildModelTestTemplateDraft(kind: ModelTestTemplateKind, draft: string, modelName: string): string {
  const parsed = parseModelTestTemplateDraft(kind, draft)
  if (!parsed.value) return draft
  const content = { ...parsed.value }
  if (kind === 'body' && content.model === modelName) {
    content.model = MODEL_TEST_MODEL_PLACEHOLDER
  }
  return JSON.stringify(content, null, 2)
}

export function applyModelTestTemplate(template: ModelTestRequestTemplate, kind: ModelTestTemplateKind, modelName: string): string {
  const content = { ...template.content }
  if (kind === 'body' && (!content.model || content.model === MODEL_TEST_MODEL_PLACEHOLDER)) {
    content.model = modelName
  }
  return JSON.stringify(content, null, 2)
}
