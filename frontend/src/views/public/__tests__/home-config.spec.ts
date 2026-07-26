import { describe, expect, it } from 'vitest'
import { ref } from 'vue'

import { useCliConfigs } from '../home-config'

describe('useCliConfigs', () => {
  it('opts the custom Codex provider into standalone web search', () => {
    const { codexConfig } = useCliConfigs(ref('https://aether.example.com'))

    expect(codexConfig.value).toContain('base_url = "https://aether.example.com/v1"')
    expect(codexConfig.value).toContain('supports_standalone_web_search = true')
    expect(codexConfig.value).toContain('[features]\nstandalone_web_search = true')
  })
})
