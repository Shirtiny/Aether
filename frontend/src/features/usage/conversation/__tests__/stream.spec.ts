import { describe, expect, it } from 'vitest'
import { parseResponse, renderResponse } from '../registry'

describe('Conversation stream compatibility', () => {
  it('parses raw OpenAI chat SSE text from stored usage records', () => {
    const requestBody = {
      model: 'gpt-5.4',
      stream: true,
      messages: [
        { role: 'user', content: 'Hello' },
      ],
    }
    const rawSse = [
      'data: {"id":"chatcmpl_123","object":"chat.completion.chunk","model":"gpt-5.4","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}',
      '',
      'data: {"id":"chatcmpl_123","object":"chat.completion.chunk","model":"gpt-5.4","choices":[{"index":0,"delta":{"content":"Hello from stream"},"finish_reason":null}]}',
      '',
      'data: {"id":"chatcmpl_123","object":"chat.completion.chunk","model":"gpt-5.4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}',
      '',
      'data: [DONE]',
      '',
    ].join('\n')

    const parsed = parseResponse(rawSse, requestBody, 'openai:chat')
    expect(parsed.apiFormat).toBe('openai')
    expect(parsed.isStream).toBe(true)
    expect(parsed.messages).toHaveLength(1)
    expect(parsed.messages[0]?.content[0]).toMatchObject({
      type: 'text',
      text: 'Hello from stream',
    })
  })

  it('renders raw OpenAI CLI SSE text from stored usage records', () => {
    const requestBody = {
      model: 'gpt-5.4',
      stream: true,
      input: 'Hello',
    }
    const rawSse = [
      'event: response.created',
      'data: {"type":"response.created","response":{"id":"resp_123","object":"response","model":"gpt-5.4","status":"in_progress"}}',
      '',
      'event: response.output_text.delta',
      'data: {"type":"response.output_text.delta","delta":"Hello from CLI stream"}',
      '',
      'event: response.completed',
      'data: {"type":"response.completed","response":{"id":"resp_123","object":"response","model":"gpt-5.4","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello from CLI stream"}]}]}}',
      '',
      'data: [DONE]',
      '',
    ].join('\n')

    const rendered = renderResponse(rawSse, requestBody, 'openai:responses')
    expect(rendered.error).toBeUndefined()
    expect(rendered.isStream).toBe(true)
    expect(rendered.blocks).toHaveLength(1)
    expect(rendered.blocks[0]).toMatchObject({
      type: 'message',
      role: 'assistant',
    })

    const firstBlock = rendered.blocks[0]
    if (!firstBlock || firstBlock.type !== 'message') {
      throw new Error('expected first render block to be message')
    }

    expect(firstBlock.content[0]).toMatchObject({
      type: 'text',
      content: 'Hello from CLI stream',
    })
  })

  it('renders legacy OpenAI CLI outtext delta alias from stored usage records', () => {
    const requestBody = {
      model: 'gpt-5.4',
      stream: true,
      input: 'Hello',
    }
    const rawSse = [
      'event: response.created',
      'data: {"type":"response.created","response":{"id":"resp_legacy_123","object":"response","model":"gpt-5.4","status":"in_progress"}}',
      '',
      'event: response.outtext.delta',
      'data: {"type":"response.outtext.delta","delta":"Hello from legacy alias"}',
      '',
      'event: response.completed',
      'data: {"type":"response.completed","response":{"id":"resp_legacy_123","object":"response","model":"gpt-5.4","status":"completed","output":[]}}',
      '',
      'data: [DONE]',
      '',
    ].join('\n')

    const rendered = renderResponse(rawSse, requestBody, 'openai:responses')
    expect(rendered.error).toBeUndefined()
    expect(rendered.isStream).toBe(true)
    expect(rendered.blocks[0]).toMatchObject({
      type: 'message',
      role: 'assistant',
    })

    const firstBlock = rendered.blocks[0]
    if (!firstBlock || firstBlock.type !== 'message') {
      throw new Error('expected first render block to be message')
    }

    expect(firstBlock.content[0]).toMatchObject({
      type: 'text',
      content: 'Hello from legacy alias',
    })
  })

  it('renders OpenAI Responses custom tool calls without text output', () => {
    const requestBody = {
      model: 'gpt-5.5',
      stream: true,
      input: 'Patch a file',
    }
    const toolInput = '*** Begin Patch\n*** Update File: demo.rs\n*** End Patch\n'
    const rawSse = [
      'event: response.created',
      'data: {"type":"response.created","response":{"id":"resp_custom_123","object":"response","model":"gpt-5.5","status":"in_progress"}}',
      '',
      'event: response.output_item.added',
      'data: {"type":"response.output_item.added","output_index":0,"item":{"id":"ctc_123","type":"custom_tool_call","status":"in_progress","call_id":"call_123","input":"","name":"apply_patch"}}',
      '',
      'event: response.custom_tool_call_input.delta',
      'data: {"type":"response.custom_tool_call_input.delta","output_index":0,"item_id":"ctc_123","delta":"*** Begin Patch\\n"}',
      '',
      'event: response.custom_tool_call_input.delta',
      'data: {"type":"response.custom_tool_call_input.delta","output_index":0,"item_id":"ctc_123","delta":"*** Update File: demo.rs\\n*** End Patch\\n"}',
      '',
      'event: response.custom_tool_call_input.done',
      `data: ${JSON.stringify({ type: 'response.custom_tool_call_input.done', output_index: 0, item_id: 'ctc_123', input: toolInput })}`,
      '',
      'event: response.output_item.done',
      `data: ${JSON.stringify({ type: 'response.output_item.done', output_index: 0, item: { id: 'ctc_123', type: 'custom_tool_call', status: 'completed', call_id: 'call_123', input: toolInput, name: 'apply_patch' } })}`,
      '',
      'event: response.completed',
      'data: {"type":"response.completed","response":{"id":"resp_custom_123","object":"response","model":"gpt-5.5","status":"completed","output":[]}}',
      '',
      'data: [DONE]',
      '',
    ].join('\n')

    const parsed = parseResponse(rawSse, requestBody, 'openai:responses')
    expect(parsed.messages[0]?.content[0]).toMatchObject({
      type: 'tool_use',
      toolName: 'apply_patch',
      toolId: 'call_123',
      input: toolInput,
    })

    const rendered = renderResponse(rawSse, requestBody, 'openai:responses')
    expect(rendered.error).toBeUndefined()
    expect(rendered.isStream).toBe(true)
    expect(rendered.blocks).toHaveLength(1)

    const firstBlock = rendered.blocks[0]
    if (!firstBlock || firstBlock.type !== 'message') {
      throw new Error('expected first render block to be message')
    }

    expect(firstBlock.content[0]).toMatchObject({
      type: 'tool_use',
      toolName: 'apply_patch',
      toolId: 'call_123',
      input: toolInput,
    })
  })

  it('keeps OpenAI Responses custom tool calls when text output is present', () => {
    const requestBody = {
      model: 'gpt-5.5',
      stream: true,
      input: 'Explain and patch',
    }
    const rawSse = [
      'event: response.output_text.delta',
      'data: {"type":"response.output_text.delta","delta":"I will patch it."}',
      '',
      'event: response.output_item.added',
      'data: {"type":"response.output_item.added","output_index":1,"item":{"id":"ctc_456","type":"custom_tool_call","status":"in_progress","call_id":"call_456","input":"","name":"apply_patch"}}',
      '',
      'event: response.custom_tool_call_input.delta',
      'data: {"type":"response.custom_tool_call_input.delta","output_index":1,"item_id":"ctc_456","delta":"patch text"}',
      '',
      'event: response.output_item.done',
      'data: {"type":"response.output_item.done","output_index":1,"item":{"id":"ctc_456","type":"custom_tool_call","status":"completed","call_id":"call_456","input":"patch text","name":"apply_patch"}}',
      '',
    ].join('\n')

    const rendered = renderResponse(rawSse, requestBody, 'openai:responses')
    const firstBlock = rendered.blocks[0]
    if (!firstBlock || firstBlock.type !== 'message') {
      throw new Error('expected first render block to be message')
    }

    expect(firstBlock.content.map(block => block.type)).toEqual(['text', 'tool_use'])
    expect(firstBlock.content[1]).toMatchObject({
      type: 'tool_use',
      toolName: 'apply_patch',
      input: 'patch text',
    })
  })

  it('renders future OpenAI Responses call items through the generic call fallback', () => {
    const requestBody = {
      model: 'gpt-5.5',
      stream: true,
      input: 'Run a command',
    }
    const action = { command: 'npm test', timeout_ms: 1000 }
    const expectedInput = JSON.stringify(action, null, 2)
    const rawSse = [
      'event: response.output_item.added',
      'data: {"type":"response.output_item.added","output_index":0,"item":{"id":"shell_123","type":"shell_call","status":"in_progress"}}',
      '',
      'event: response.output_item.done',
      `data: ${JSON.stringify({ type: 'response.output_item.done', output_index: 0, item: { id: 'shell_123', type: 'shell_call', status: 'completed', action } })}`,
      '',
    ].join('\n')

    const parsed = parseResponse(rawSse, requestBody, 'openai:responses')
    expect(parsed.messages[0]?.content[0]).toMatchObject({
      type: 'tool_use',
      toolName: 'shell_call',
      toolId: 'shell_123',
      input: expectedInput,
    })

    const rendered = renderResponse(rawSse, requestBody, 'openai:responses')
    const firstBlock = rendered.blocks[0]
    if (!firstBlock || firstBlock.type !== 'message') {
      throw new Error('expected first render block to be message')
    }

    expect(firstBlock.content[0]).toMatchObject({
      type: 'tool_use',
      toolName: 'shell_call',
      toolId: 'shell_123',
      input: expectedInput,
    })
  })

  it('keeps streamed function_call arguments when response.completed omits them', () => {
    const requestBody = {
      model: 'gpt-5.5',
      stream: true,
      input: 'What is the weather?',
    }
    const rawSse = [
      'event: response.created',
      `data: ${JSON.stringify({ type: 'response.created', response: { id: 'resp_fc_1', object: 'response', model: 'gpt-5.5', status: 'in_progress' } })}`,
      '',
      'event: response.output_item.added',
      `data: ${JSON.stringify({ type: 'response.output_item.added', output_index: 0, item: { id: 'fc_1', type: 'function_call', status: 'in_progress', call_id: 'call_1', name: 'get_weather', arguments: '' } })}`,
      '',
      'event: response.function_call_arguments.delta',
      `data: ${JSON.stringify({ type: 'response.function_call_arguments.delta', output_index: 0, item_id: 'fc_1', delta: '{"city":' })}`,
      '',
      'event: response.function_call_arguments.delta',
      `data: ${JSON.stringify({ type: 'response.function_call_arguments.delta', output_index: 0, item_id: 'fc_1', delta: '"SF"}' })}`,
      '',
      // 最终项故意不带 arguments：解析器不应用 '{}' 冲掉已收集的增量参数
      'event: response.completed',
      `data: ${JSON.stringify({ type: 'response.completed', response: { id: 'resp_fc_1', object: 'response', model: 'gpt-5.5', status: 'completed', output: [{ id: 'fc_1', type: 'function_call', status: 'completed', call_id: 'call_1', name: 'get_weather' }] } })}`,
      '',
      'data: [DONE]',
      '',
    ].join('\n')

    const parsed = parseResponse(rawSse, requestBody, 'openai:responses')
    // 命中同一 key，不重复渲染
    expect(parsed.messages).toHaveLength(1)
    expect(parsed.messages[0]?.content).toHaveLength(1)
    expect(parsed.messages[0]?.content[0]).toMatchObject({
      type: 'tool_use',
      toolName: 'get_weather',
      toolId: 'call_1',
      input: '{"city":"SF"}',
    })
  })

  it('renders HTML-entity encoded OpenAI tool arguments as formatted JSON', () => {
    const requestBody = {
      model: 'gpt-5.4',
      messages: [
        { role: 'user', content: 'Call a tool' },
      ],
    }
    const responseBody = {
      id: 'chatcmpl_tool_123',
      object: 'chat.completion',
      model: 'gpt-5.4',
      choices: [
        {
          index: 0,
          message: {
            role: 'assistant',
            content: null,
            tool_calls: [
              {
                id: 'call_123',
                type: 'function',
                function: {
                  name: 'skill',
                  arguments: '{&quot;name&quot;:&quot;hai-ai&quot;,&quot;user_message&quot;:&quot;A &amp; B &lt; C &gt; D &#39;ok&#39;&quot;}',
                },
              },
            ],
          },
          finish_reason: 'tool_calls',
        },
      ],
    }

    const rendered = renderResponse(responseBody, requestBody, 'openai:chat')
    expect(rendered.error).toBeUndefined()

    const firstBlock = rendered.blocks[0]
    if (!firstBlock || firstBlock.type !== 'message') {
      throw new Error('expected first render block to be message')
    }

    expect(firstBlock.content[0]).toMatchObject({
      type: 'tool_use',
      toolName: 'skill',
      input: JSON.stringify({
        name: 'hai-ai',
        user_message: "A & B < C > D 'ok'",
      }, null, 2),
    })
  })
})
