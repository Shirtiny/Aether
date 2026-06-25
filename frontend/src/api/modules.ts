import apiClient from './client'

const MODULE_MANAGEMENT_ORDER_CONFIG_KEY = 'module_management.extension_order'

export interface ModuleStatus {
  name: string
  available: boolean
  enabled: boolean
  active: boolean
  config_validated: boolean
  config_error: string | null
  display_name: string
  description: string
  category: 'auth' | 'monitoring' | 'security' | 'integration'
  admin_route: string | null
  admin_menu_icon: string | null
  admin_menu_group: string | null
  admin_menu_order: number
  health: 'healthy' | 'degraded' | 'unhealthy' | 'unknown'
}

export interface AuthModuleInfo {
  name: string
  display_name: string
  active: boolean
}

export type ChatPiiRedactionTtlSeconds = 300 | 3600

export interface ChatPiiRedactionRuleFeatures {
  validator?: string | null
  [key: string]: unknown
}

export interface ChatPiiRedactionRule {
  id: string
  name: string
  pattern: string
  enabled: boolean
  system?: boolean
  features?: ChatPiiRedactionRuleFeatures | null
}

export interface ChatPiiRedactionConfig {
  enabled: boolean
  rules: ChatPiiRedactionRule[]
  cache_ttl_seconds: ChatPiiRedactionTtlSeconds
  placeholder_prefix: string
}

export type LocalProbeInterceptKind = 'ping' | 'health'

export interface LocalProbeInterceptRule {
  id: string
  name: string
  prompt: string
  response: string
  kind: LocalProbeInterceptKind
  enabled: boolean
  system?: boolean
}

export interface LocalProbeInterceptConfig {
  enabled: boolean
  rules: LocalProbeInterceptRule[]
  delay_min_ms: number
  delay_max_ms: number
}

export const CHAT_PII_REDACTION_DEFAULT_RULES: ChatPiiRedactionRule[] = [
  { id: 'email', name: '邮箱', pattern: '(?i)[A-Z0-9._%+-]{1,64}@[A-Z0-9.-]{1,253}\\.[A-Z]{2,63}', enabled: true, features: { validator: 'email' }, system: true },
  { id: 'cn_phone', name: '手机号', pattern: '(?:\\+?86[- ]?)?(?:1[3-9]\\d[- ]?\\d{4}[- ]?\\d{4}|0\\d{2,3}[- ]\\d{7,8}(?:-\\d{1,6})?)', enabled: true, features: { validator: 'cn_phone' }, system: true },
  { id: 'global_phone', name: '国际号码', pattern: '\\+[1-9]\\d(?:[ -]?\\d){6,13}\\d', enabled: true, features: { validator: 'global_phone' }, system: true },
  { id: 'cn_id', name: '身份证号', pattern: '(?i)\\b\\d{17}[\\dX]\\b', enabled: true, features: { validator: 'cn_id' }, system: true },
  { id: 'payment_card', name: '银行卡号', pattern: '\\b(?:\\d[ -]?){12,18}\\d\\b', enabled: true, features: { validator: 'payment_card' }, system: true },
  { id: 'ipv4', name: 'IPv4', pattern: '\\b(?:\\d{1,3}\\.){3}\\d{1,3}\\b', enabled: true, features: { validator: 'ipv4' }, system: true },
  { id: 'ipv6', name: 'IPv6', pattern: '\\b(?:[0-9A-Fa-f]{1,4}:){2,7}[0-9A-Fa-f:.]{1,39}\\b', enabled: true, features: { validator: 'ipv6' }, system: true },
  { id: 'api_key', name: 'API Key', pattern: '\\b(?:sk-(?:proj-)?[A-Za-z0-9_-]{20,}|sk-ant-[A-Za-z0-9_-]{20,}|(?:gh[pousr]_[A-Za-z0-9_]{30,}|github_pat_[A-Za-z0-9_]{30,})|xox[baprs]-[A-Za-z0-9-]{20,}|(?:AKIA|ASIA)[0-9A-Z]{16})\\b', enabled: true, features: { validator: 'api_key' }, system: true },
  { id: 'access_token', name: 'Access Token', pattern: "(?i)\\baccess[_-]?token\\s*[:=]\\s*[\"']?[A-Za-z0-9._~+/=-]{20,}", enabled: true, features: { validator: 'access_token' }, system: true },
  { id: 'secret_key', name: 'Secret Key', pattern: "(?i)\\bsecret[_-]?key\\s*[:=]\\s*[\"']?[A-Za-z0-9._~+/=-]{20,}", enabled: true, features: { validator: 'secret_key' }, system: true },
  { id: 'bearer_token', name: 'Bearer Token', pattern: '(?i)\\bBearer\\s+[A-Za-z0-9._~+/=-]{20,}', enabled: true, features: { validator: 'bearer_token' }, system: true },
  { id: 'jwt', name: 'JWT', pattern: '\\b[A-Za-z0-9_-]{10,}\\.[A-Za-z0-9_-]{10,}\\.[A-Za-z0-9_-]{10,}\\b', enabled: true, features: { validator: 'jwt' }, system: true },
]

export const LOCAL_PROBE_INTERCEPT_DEFAULT_RULES: LocalProbeInterceptRule[] = [
  { id: 'ping', name: 'Ping', prompt: 'ping', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'reply_pong', name: '回复 pong', prompt: '只回复 pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'reply_pong_plain', name: 'Reply pong', prompt: 'reply pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'reply_exactly_pong', name: 'Reply exactly PONG', prompt: 'Reply exactly: PONG', response: 'PONG', kind: 'ping', enabled: true, system: true },
  { id: 'respond_pong', name: 'Respond pong', prompt: 'respond pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'respond_exactly_pong', name: 'Respond exactly pong', prompt: 'respond exactly pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'reply_with_pong', name: 'Reply with pong', prompt: 'reply with pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'respond_with_pong', name: 'Respond with pong', prompt: 'respond with pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'say_pong', name: 'Say pong', prompt: 'say pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'only_pong', name: 'Only pong', prompt: 'only pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'just_pong', name: 'Just pong', prompt: 'just pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'cn_reply_pong', name: '回复 pong', prompt: '回复 pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'cn_return_pong', name: '返回 pong', prompt: '返回 pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'cn_only_reply_pong', name: '仅回复 pong', prompt: '仅回复 pong', response: 'pong', kind: 'ping', enabled: true, system: true },
  { id: 'reply_exactly_ok', name: 'Reply exactly OK', prompt: 'Reply exactly: OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'ok', name: 'OK', prompt: 'OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'say_ok', name: 'Say OK', prompt: 'Say OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'only_ok', name: 'Only OK', prompt: 'only OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'just_ok', name: 'Just OK', prompt: 'just OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'reply_ok', name: 'Reply OK', prompt: 'reply OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'reply_with_ok', name: 'Reply with OK', prompt: 'reply with OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'respond_ok', name: 'Respond OK', prompt: 'respond OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'respond_exactly_ok', name: 'Respond exactly OK', prompt: 'respond exactly OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'respond_with_ok', name: 'Respond with OK', prompt: 'respond with OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'return_ok', name: 'Return OK', prompt: 'return OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_reply_ok', name: '请回复 OK', prompt: '请回复 OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_reply_ok_plain', name: '回复 OK', prompt: '回复 OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_return_ok_plain', name: '返回 OK', prompt: '返回 OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_only_reply_ok_plain', name: '仅回复 OK', prompt: '仅回复 OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_only_return_ok_plain', name: '仅返回 OK', prompt: '仅返回 OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_return_ok', name: '请返回 OK', prompt: '请返回 OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_only_reply_ok', name: '只回复 OK', prompt: '只回复 OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_only_return_ok', name: '只返回 OK', prompt: '只返回 OK', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'are_you_alive', name: 'Are you alive', prompt: 'Are you alive?', response: 'Yes.', kind: 'health', enabled: true, system: true },
  { id: 'alive', name: 'Alive', prompt: 'alive?', response: 'Yes.', kind: 'health', enabled: true, system: true },
  { id: 'online', name: 'Online', prompt: 'online?', response: 'Yes.', kind: 'health', enabled: true, system: true },
  { id: 'are_you_online', name: 'Are you online', prompt: 'Are you online?', response: 'Yes.', kind: 'health', enabled: true, system: true },
  { id: 'are_you_working', name: 'Are you working', prompt: 'Are you working?', response: 'Yes.', kind: 'health', enabled: true, system: true },
  { id: 'hello', name: 'Hello', prompt: 'hello', response: 'Hello!', kind: 'health', enabled: true, system: true },
  { id: 'hi', name: 'Hi', prompt: 'hi', response: 'Hello!', kind: 'health', enabled: true, system: true },
  { id: 'cn_hello_polite', name: '您好', prompt: '您好', response: '你好！', kind: 'health', enabled: true, system: true },
  { id: 'cn_hello', name: '你好', prompt: '你好', response: '你好！', kind: 'health', enabled: true, system: true },
  { id: 'who_are_you', name: 'Who are you', prompt: 'who are you', response: "I'm ChatGPT.", kind: 'health', enabled: true, system: true },
  { id: 'who_are_u', name: 'Who are u', prompt: 'who are u', response: "I'm ChatGPT.", kind: 'health', enabled: true, system: true },
  { id: 'cn_who_are_you', name: '你是谁', prompt: '你是谁', response: '我是 ChatGPT。', kind: 'health', enabled: true, system: true },
  { id: 'test', name: 'Test', prompt: 'test', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_test', name: '测试', prompt: '测试', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_health_probe', name: '测活', prompt: '测活', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_connectivity_test', name: '联通测试', prompt: '联通测试', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_connection_test', name: '连接测试', prompt: '连接测试', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_api_test', name: '接口测试', prompt: '接口测试', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'cn_health_check', name: '健康检查', prompt: '健康检查', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'healthcheck', name: 'Health check', prompt: 'healthcheck', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'health', name: 'Health', prompt: 'health', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'connection_test', name: 'Connection test', prompt: 'connection test', response: 'OK', kind: 'health', enabled: true, system: true },
  { id: 'connectivity_test', name: 'Connectivity test', prompt: 'connectivity test', response: 'OK', kind: 'health', enabled: true, system: true },
]

const CHAT_PII_REDACTION_CONFIG_KEYS = {
  enabled: 'module.chat_pii_redaction.enabled',
  rules: 'module.chat_pii_redaction.rules',
  cache_ttl_seconds: 'module.chat_pii_redaction.cache_ttl_seconds',
  placeholder_prefix: 'module.chat_pii_redaction.placeholder_prefix',
} as const

const LOCAL_PROBE_INTERCEPT_CONFIG_KEYS = {
  enabled: 'module.local_probe_intercept.enabled',
  rules: 'module.local_probe_intercept.rules',
  delay_min_ms: 'module.local_probe_intercept.delay_min_ms',
  delay_max_ms: 'module.local_probe_intercept.delay_max_ms',
} as const

export const LOCAL_PROBE_INTERCEPT_DEFAULT_DELAY_MIN_MS = 900
export const LOCAL_PROBE_INTERCEPT_DEFAULT_DELAY_MAX_MS = 2000
export const LOCAL_PROBE_INTERCEPT_MAX_DELAY_MS = 60000

const CHAT_PII_REDACTION_DEFAULT_CONFIG: ChatPiiRedactionConfig = {
  enabled: false,
  rules: CHAT_PII_REDACTION_DEFAULT_RULES.map(rule => ({ ...rule })),
  cache_ttl_seconds: 300,
  placeholder_prefix: 'AETHER',
}

export function normalizeModuleManagementOrder(value: unknown): string[] {
  if (!Array.isArray(value)) return []
  const seen = new Set<string>()
  const order: string[] = []
  for (const item of value) {
    if (typeof item !== 'string') continue
    const name = item.trim()
    if (!name || seen.has(name)) continue
    seen.add(name)
    order.push(name)
  }
  return order
}

function cloneDefaultChatPiiRedactionRules(): ChatPiiRedactionRule[] {
  return CHAT_PII_REDACTION_DEFAULT_RULES.map(rule => ({ ...rule }))
}

function cloneDefaultLocalProbeInterceptRules(): LocalProbeInterceptRule[] {
  return LOCAL_PROBE_INTERCEPT_DEFAULT_RULES.map(rule => ({ ...rule }))
}

function normalizeChatPiiRedactionRule(value: unknown, index: number): ChatPiiRedactionRule | null {
  if (!value || typeof value !== 'object') return null
  const item = value as Record<string, unknown>
  const id = typeof item.id === 'string' && item.id.trim()
    ? item.id.trim()
    : `custom_${index + 1}`
  const name = typeof item.name === 'string' && item.name.trim()
    ? item.name.trim()
    : id
  const pattern = typeof item.pattern === 'string' ? item.pattern : ''
  if (!pattern.trim()) return null
  const rawFeatures = item.features && typeof item.features === 'object' && !Array.isArray(item.features)
    ? { ...(item.features as Record<string, unknown>) }
    : {}
  const legacyValidator = typeof item.kind === 'string' && item.kind.trim()
    ? item.kind.trim()
    : null
  const validator = typeof rawFeatures.validator === 'string' && rawFeatures.validator.trim()
    ? rawFeatures.validator.trim()
    : legacyValidator
  if (validator) {
    rawFeatures.validator = validator
  } else {
    delete rawFeatures.validator
  }
  const features = Object.keys(rawFeatures).length > 0 ? rawFeatures : null
  return {
    id,
    name,
    pattern,
    enabled: item.enabled !== false,
    system: item.system === true,
    features,
  }
}

function normalizeChatPiiRedactionRules(value: unknown): ChatPiiRedactionRule[] {
  if (!Array.isArray(value)) return cloneDefaultChatPiiRedactionRules()
  return value
    .map((item, index) => normalizeChatPiiRedactionRule(item, index))
    .filter((item): item is ChatPiiRedactionRule => item !== null)
}

function normalizeChatPiiRedactionConfig(values: {
  enabled: unknown
  rules: unknown
  cache_ttl_seconds: unknown
  placeholder_prefix: unknown
}): ChatPiiRedactionConfig {
  return {
    enabled: values.enabled === true,
    rules: normalizeChatPiiRedactionRules(values.rules),
    cache_ttl_seconds: values.cache_ttl_seconds === 3600 ? 3600 : 300,
    placeholder_prefix: normalizePlaceholderPrefix(values.placeholder_prefix),
  }
}

function normalizePlaceholderPrefix(value: unknown): string {
  if (typeof value !== 'string') return CHAT_PII_REDACTION_DEFAULT_CONFIG.placeholder_prefix
  const normalized = value.trim().toUpperCase()
  return /^[A-Z0-9_]{1,32}$/.test(normalized)
    ? normalized
    : CHAT_PII_REDACTION_DEFAULT_CONFIG.placeholder_prefix
}

function normalizeLocalProbeInterceptRule(value: unknown, index: number): LocalProbeInterceptRule | null {
  if (!value || typeof value !== 'object') return null
  const item = value as Record<string, unknown>
  const id = typeof item.id === 'string' && item.id.trim()
    ? item.id.trim()
    : `custom_${index + 1}`
  const name = typeof item.name === 'string' && item.name.trim()
    ? item.name.trim()
    : id
  const prompt = typeof item.prompt === 'string' ? item.prompt : ''
  const response = typeof item.response === 'string' ? item.response : ''
  if (!prompt.trim() || !response.trim()) return null
  const kind: LocalProbeInterceptKind = item.kind === 'ping' ? 'ping' : 'health'
  return {
    id,
    name,
    prompt,
    response,
    kind,
    enabled: item.enabled !== false,
    system: item.system === true,
  }
}

function normalizeLocalProbeInterceptRules(value: unknown): LocalProbeInterceptRule[] {
  if (!Array.isArray(value)) return cloneDefaultLocalProbeInterceptRules()
  return value
    .map((item, index) => normalizeLocalProbeInterceptRule(item, index))
    .filter((item): item is LocalProbeInterceptRule => item !== null)
}

function normalizeLocalProbeInterceptDelayMs(value: unknown, fallback: number): number {
  if (value === null || value === undefined || value === '') return fallback
  const parsed = typeof value === 'number' ? value : Number(value)
  if (!Number.isInteger(parsed) || parsed < 0 || parsed > LOCAL_PROBE_INTERCEPT_MAX_DELAY_MS) {
    return fallback
  }
  return parsed
}

function normalizeLocalProbeInterceptConfig(values: {
  enabled: unknown
  rules: unknown
  delay_min_ms: unknown
  delay_max_ms: unknown
}): LocalProbeInterceptConfig {
  const delayMinMs = normalizeLocalProbeInterceptDelayMs(
    values.delay_min_ms,
    LOCAL_PROBE_INTERCEPT_DEFAULT_DELAY_MIN_MS,
  )
  const delayMaxMs = normalizeLocalProbeInterceptDelayMs(
    values.delay_max_ms,
    LOCAL_PROBE_INTERCEPT_DEFAULT_DELAY_MAX_MS,
  )
  return {
    enabled: values.enabled !== false,
    rules: normalizeLocalProbeInterceptRules(values.rules),
    delay_min_ms: Math.min(delayMinMs, delayMaxMs),
    delay_max_ms: Math.max(delayMinMs, delayMaxMs),
  }
}

async function getSystemConfigValue(key: string): Promise<unknown> {
  const response = await apiClient.get<{ key: string; value: unknown }>(`/api/admin/system/configs/${key}`)
  return response.data.value
}

async function updateSystemConfigValue(key: string, value: unknown, description: string) {
  const response = await apiClient.put<{ key: string; value: unknown; description?: string }>(
    `/api/admin/system/configs/${key}`,
    { value, description },
  )
  return response.data.value
}

export const modulesApi = {
  /**
   * 获取所有模块状态（管理员）
   */
  async getAllStatus(): Promise<Record<string, ModuleStatus>> {
    const response = await apiClient.get<Record<string, ModuleStatus>>(
      '/api/admin/modules/status'
    )
    return response.data
  },

  /**
   * 获取单个模块状态（管理员）
   */
  async getStatus(moduleName: string): Promise<ModuleStatus> {
    const response = await apiClient.get<ModuleStatus>(
      `/api/admin/modules/status/${moduleName}`
    )
    return response.data
  },

  /**
   * 设置模块启用状态（管理员）
   */
  async setEnabled(moduleName: string, enabled: boolean): Promise<ModuleStatus> {
    const response = await apiClient.put<ModuleStatus>(
      `/api/admin/modules/status/${moduleName}/enabled`,
      { enabled }
    )
    return response.data
  },

  async getModuleManagementOrder(): Promise<string[]> {
    try {
      const response = await apiClient.get<{ key: string; value: unknown }>(
        `/api/admin/system/configs/${MODULE_MANAGEMENT_ORDER_CONFIG_KEY}`
      )
      return normalizeModuleManagementOrder(response.data.value)
    } catch (err) {
      const status = (err as { response?: { status?: number } }).response?.status
      if (status === 404) return []
      throw err
    }
  },

  async updateModuleManagementOrder(order: string[]): Promise<string[]> {
    const normalized = normalizeModuleManagementOrder(order)
    const response = await apiClient.put<{ key: string; value: unknown }>(
      `/api/admin/system/configs/${MODULE_MANAGEMENT_ORDER_CONFIG_KEY}`,
      {
        value: normalized,
        description: '模块管理扩展模块展示顺序',
      },
    )
    return normalizeModuleManagementOrder(response.data.value)
  },

  async getChatPiiRedactionConfig(): Promise<ChatPiiRedactionConfig> {
    const [enabled, rules, cacheTtlSeconds, placeholderPrefix] = await Promise.all([
      getSystemConfigValue(CHAT_PII_REDACTION_CONFIG_KEYS.enabled),
      getSystemConfigValue(CHAT_PII_REDACTION_CONFIG_KEYS.rules),
      getSystemConfigValue(CHAT_PII_REDACTION_CONFIG_KEYS.cache_ttl_seconds),
      getSystemConfigValue(CHAT_PII_REDACTION_CONFIG_KEYS.placeholder_prefix),
    ])

    return normalizeChatPiiRedactionConfig({
      enabled,
      rules,
      cache_ttl_seconds: cacheTtlSeconds,
      placeholder_prefix: placeholderPrefix,
    })
  },

  async updateChatPiiRedactionConfig(config: ChatPiiRedactionConfig): Promise<ChatPiiRedactionConfig> {
    const [enabled, rules, cacheTtlSeconds, placeholderPrefix] = await Promise.all([
      updateSystemConfigValue(CHAT_PII_REDACTION_CONFIG_KEYS.enabled, config.enabled, '敏感信息保护总开关'),
      updateSystemConfigValue(CHAT_PII_REDACTION_CONFIG_KEYS.rules, config.rules, '敏感信息保护替换规则'),
      updateSystemConfigValue(CHAT_PII_REDACTION_CONFIG_KEYS.cache_ttl_seconds, config.cache_ttl_seconds, '敏感信息保护缓存 TTL'),
      updateSystemConfigValue(CHAT_PII_REDACTION_CONFIG_KEYS.placeholder_prefix, config.placeholder_prefix, '敏感信息保护占位符前缀'),
    ])

    return normalizeChatPiiRedactionConfig({
      enabled,
      rules,
      cache_ttl_seconds: cacheTtlSeconds,
      placeholder_prefix: placeholderPrefix,
    })
  },

  async getLocalProbeInterceptConfig(): Promise<LocalProbeInterceptConfig> {
    const [enabled, rules, delayMinMs, delayMaxMs] = await Promise.all([
      getSystemConfigValue(LOCAL_PROBE_INTERCEPT_CONFIG_KEYS.enabled),
      getSystemConfigValue(LOCAL_PROBE_INTERCEPT_CONFIG_KEYS.rules),
      getSystemConfigValue(LOCAL_PROBE_INTERCEPT_CONFIG_KEYS.delay_min_ms),
      getSystemConfigValue(LOCAL_PROBE_INTERCEPT_CONFIG_KEYS.delay_max_ms),
    ])

    return normalizeLocalProbeInterceptConfig({
      enabled,
      rules,
      delay_min_ms: delayMinMs,
      delay_max_ms: delayMaxMs,
    })
  },

  async updateLocalProbeInterceptConfig(config: LocalProbeInterceptConfig): Promise<LocalProbeInterceptConfig> {
    const [enabled, rules, delayMinMs, delayMaxMs] = await Promise.all([
      updateSystemConfigValue(LOCAL_PROBE_INTERCEPT_CONFIG_KEYS.enabled, config.enabled, '测活拦截总开关'),
      updateSystemConfigValue(LOCAL_PROBE_INTERCEPT_CONFIG_KEYS.rules, config.rules, '测活拦截提示词与回复规则'),
      updateSystemConfigValue(LOCAL_PROBE_INTERCEPT_CONFIG_KEYS.delay_min_ms, config.delay_min_ms, '测活拦截随机延迟最小毫秒数'),
      updateSystemConfigValue(LOCAL_PROBE_INTERCEPT_CONFIG_KEYS.delay_max_ms, config.delay_max_ms, '测活拦截随机延迟最大毫秒数'),
    ])

    return normalizeLocalProbeInterceptConfig({
      enabled,
      rules,
      delay_min_ms: delayMinMs,
      delay_max_ms: delayMaxMs,
    })
  },

  /**
   * 获取认证模块状态（公开接口，供登录页使用）
   */
  async getAuthModulesStatus(): Promise<AuthModuleInfo[]> {
    const response = await apiClient.get<AuthModuleInfo[]>('/api/modules/auth-status')
    return response.data
  },
}
