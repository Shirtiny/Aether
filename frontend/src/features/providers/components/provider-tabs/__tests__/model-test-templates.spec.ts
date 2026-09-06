import { describe, expect, it } from 'vitest'
import {
  applyModelTestTemplate,
  buildModelTestTemplateDraft,
  emptyModelTestTemplates,
  modelTestTemplateMatchesFormat,
  parseModelTestTemplateDraft,
  parseModelTestTemplates,
  type ModelTestRequestTemplate,
} from '../model-test-templates'

const template: ModelTestRequestTemplate = {
  id: 'body-1',
  name: '问答测试',
  api_format: 'openai:responses',
  content: { model: '{{model}}', input: 'Hello', stream: true },
}

describe('model test request templates', () => {
  it('accepts empty or saved lists but rejects malformed reads instead of silently clearing them', () => {
    expect(parseModelTestTemplates(emptyModelTestTemplates())).toEqual({ body: [], headers: [] })
    expect(parseModelTestTemplates({ body: [template], headers: [] }).body).toEqual([template])
    for (const value of [null, [], {}, { headers: [] }, { headers: [], body: [null] }, {
      headers: [], body: [{ ...template, content: [] }],
    }]) {
      expect(() => parseModelTestTemplates(value)).toThrow('全局请求模板格式无效')
    }
  })

  it('filters by normalized endpoint format while keeping global templates available', () => {
    expect(modelTestTemplateMatchesFormat(template, 'OPENAI_RESPONSES')).toBe(true)
    expect(modelTestTemplateMatchesFormat(template, 'claude:messages')).toBe(false)
    expect(modelTestTemplateMatchesFormat(template, undefined)).toBe(false)
    expect(modelTestTemplateMatchesFormat({ ...template, api_format: null }, 'claude:messages')).toBe(true)
  })

  it('resolves the current model safely without mutating the reusable template or unrelated strings', () => {
    const source = { ...template, content: { ...template.content, input: 'Keep {{model}} literally' } }
    const model = 'mapped-"model\\name'
    expect(JSON.parse(applyModelTestTemplate(source, 'body', model))).toEqual({
      model, input: 'Keep {{model}} literally', stream: true,
    })
    expect(source.content).toHaveProperty('model', '{{model}}')
    expect(JSON.parse(applyModelTestTemplate(source, 'body', 'another-model')).model).toBe('another-model')
  })

  it('fills omitted models but preserves an explicitly pinned model and header content', () => {
    expect(JSON.parse(applyModelTestTemplate({ ...template, content: { input: 'test' } }, 'body', 'current')).model).toBe('current')
    expect(JSON.parse(applyModelTestTemplate({ ...template, content: { model: 'fixed' } }, 'body', 'current')).model).toBe('fixed')
    expect(JSON.parse(applyModelTestTemplate({ ...template, content: { 'x-model': '{{model}}' } }, 'headers', 'current'))).toEqual({ 'x-model': '{{model}}' })
  })

  it('prepares reusable drafts from the current request without changing a manual model override', () => {
    expect(JSON.parse(buildModelTestTemplateDraft('body', '{"model":"current","input":"hello"}', 'current'))).toEqual({
      model: '{{model}}', input: 'hello',
    })
    expect(JSON.parse(buildModelTestTemplateDraft('body', '{"model":"fixed"}', 'current'))).toEqual({ model: 'fixed' })
    expect(buildModelTestTemplateDraft('body', '{bad', 'current')).toBe('{bad')
    expect(buildModelTestTemplateDraft('headers', '', 'current')).toBe('{}')
  })

  it('requires object JSON and valid scalar header values', () => {
    for (const draft of ['', '[]', 'null', '"text"', '{bad']) {
      expect(parseModelTestTemplateDraft('body', draft).error).toBeTruthy()
    }
    expect(parseModelTestTemplateDraft('headers', '').value).toEqual({})
    expect(parseModelTestTemplateDraft('headers', '{"x-test":true,"x-number":2}').error).toBeNull()
    for (const content of [{ 'bad name': 'value' }, { 'x-test': [] }, { 'x-test': null }, { 'x-test': 'bad\r\nvalue' }]) {
      expect(parseModelTestTemplateDraft('headers', JSON.stringify(content)).error).toBeTruthy()
    }
  })
})
