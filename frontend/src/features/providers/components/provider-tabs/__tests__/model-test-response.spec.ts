import { describe, expect, it } from 'vitest'
import { extractModelTestResponseText, formatModelTestResponseBody } from '../model-test-response'

describe('full model test response', () => {
  const long = `第一行\n\n  保留缩进\n${'响应内容'.repeat(3000)}\n最后一行`

  it('preserves long Responses text, every output item and available reasoning summaries', () => {
    const body = { output: [
      { type: 'reasoning', summary: [{ type: 'summary_text', text: '已返回的推理摘要' }] },
      { type: 'message', content: [{ type: 'output_text', text: long }, { type: 'output_text', text: '第二段' }] },
      { type: 'message', content: [{ type: 'output_text', text: '第二条消息' }] },
    ] }
    expect(extractModelTestResponseText(body)).toBe(`已返回的推理摘要\n\n${long}\n\n第二段\n\n第二条消息`)
    expect(JSON.parse(formatModelTestResponseBody(body))).toEqual(body)
  })

  it('preserves every Chat choice and text part with reasoning and refusals', () => {
    expect(extractModelTestResponseText({ choices: [
      { message: { reasoning_content: 'reasoning', content: long } },
      { message: { content: [{ type: 'text', text: 'another' }, { type: 'text', text: 'part' }], refusal: 'refusal' } },
    ] })).toBe(`reasoning\n\n${long}\n\nanother\n\npart\n\nrefusal`)
  })

  it('preserves Claude and Gemini text without whitespace compression', () => {
    expect(extractModelTestResponseText({ content: [{ type: 'thinking', thinking: 'thought' }, { type: 'text', text: long }] })).toBe(`thought\n\n${long}`)
    expect(extractModelTestResponseText({ candidates: [
      { content: { parts: [{ text: long }, { text: 'end-1' }] } },
      { content: { parts: [{ text: 'end-2' }] } },
    ] })).toBe(`${long}\n\nend-1\n\nend-2`)
  })

  it('handles wrapped, serialized and plain-text responses', () => {
    expect(extractModelTestResponseText({ response: { body: { output_text: long } } })).toBe(long)
    expect(extractModelTestResponseText(JSON.stringify({ output_text: long }))).toBe(long)
    expect(extractModelTestResponseText(`<html>${long}</html>`)).toBe(`<html>${long}</html>`)
  })

  it('keeps all error, tool, image and usage fields in the raw JSON', () => {
    const body = { detail: long, output: [{ type: 'function_call', arguments: long }], data: [{ b64_json: long }], usage: { total_tokens: 321 } }
    expect(extractModelTestResponseText(body)).toBeNull()
    expect(JSON.parse(formatModelTestResponseBody(body))).toEqual(body)
    expect(formatModelTestResponseBody(false)).toBe('false')
    expect(formatModelTestResponseBody(0)).toBe('0')
  })
})
