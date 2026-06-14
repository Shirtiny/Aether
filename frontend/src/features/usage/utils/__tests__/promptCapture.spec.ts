import { describe, expect, it } from 'vitest'

import { extractPromptCaptureMetadata } from '../promptCapture'

describe('prompt capture metadata', () => {
  it('extracts prompt capture from top-level request metadata', () => {
    const capture = extractPromptCaptureMetadata({
      prompt_capture: {
        item_count: 2,
        role_counts: { system: 1, user: '1' },
        items: [
          {
            source: 'messages[0].content',
            role: 'system',
            sha256: 'abc',
            chars: 120,
            preview: 'system prompt preview',
            truncated: false,
          },
          {
            source: 'messages[1].content',
            role: 'user',
            sha256: 'def',
            chars: '42',
            preview: 'user prompt preview',
            truncated: true,
          },
        ],
      },
    })

    expect(capture).toEqual({
      itemCount: 2,
      roleCounts: { system: 1, user: 1 },
      items: [
        {
          source: 'messages[0].content',
          role: 'system',
          sha256: 'abc',
          chars: 120,
          preview: 'system prompt preview',
          truncated: false,
        },
        {
          source: 'messages[1].content',
          role: 'user',
          sha256: 'def',
          chars: 42,
          preview: 'user prompt preview',
          truncated: true,
        },
      ],
    })
  })

  it('extracts prompt capture from nested request_metadata', () => {
    const capture = extractPromptCaptureMetadata({
      request_metadata: {
        prompt_capture: {
          items: [
            {
              role: 'user',
              preview: 'nested prompt',
            },
          ],
        },
      },
    })

    expect(capture?.items).toHaveLength(1)
    expect(capture?.items[0]?.preview).toBe('nested prompt')
  })

  it('returns null when there are no prompt capture items', () => {
    expect(extractPromptCaptureMetadata({ prompt_capture: { items: [] } })).toBeNull()
    expect(extractPromptCaptureMetadata({})).toBeNull()
  })
})
