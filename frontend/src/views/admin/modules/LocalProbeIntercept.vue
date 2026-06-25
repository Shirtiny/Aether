<template>
  <PageContainer>
    <PageHeader
      title="测活拦截"
      description="拦截已配置的短测活提示词并本地返回对应回复；算术数字测活由后端内置处理。"
      :icon="Activity"
    >
      <template #actions>
        <Button
          variant="outline"
          :disabled="loading || saving"
          @click="loadConfig"
        >
          <RefreshCw
            class="mr-2 h-4 w-4"
            :class="{ 'animate-spin': loading }"
          />
          刷新
        </Button>
        <Button
          :disabled="loading || saving || !hasChanges"
          @click="saveConfig"
        >
          {{ saving ? '保存中...' : '保存配置' }}
        </Button>
      </template>
    </PageHeader>

    <div class="mt-6 space-y-6">
      <section class="rounded-lg border border-border bg-card p-5">
        <div class="flex flex-col gap-4 md:flex-row md:items-center md:justify-between">
          <div class="space-y-1">
            <div
              v-if="statusLabel"
              class="flex items-center gap-2"
            >
              <span class="h-2.5 w-2.5 rounded-full bg-primary ring-2 ring-primary/30 ring-offset-2 ring-offset-background" />
              <p class="text-sm font-semibold text-foreground">
                {{ statusLabel }}
              </p>
            </div>
            <p class="max-w-3xl text-sm text-muted-foreground">
              提示词使用精确匹配：忽略大小写、空白和常见标点，但不会因为出现在长提示词中而命中。
            </p>
            <p class="max-w-3xl text-xs text-muted-foreground">
              计费和 usage 记录保持启用，拦截结果会标记为 local provider 和对应 ping_kind。
            </p>
          </div>
          <div class="flex items-center gap-3 rounded-lg border border-border bg-muted/40 px-4 py-3">
            <div class="text-right">
              <p class="text-sm font-medium text-foreground">
                启用测活拦截
              </p>
            </div>
            <Switch
              :model-value="config.enabled"
              @update:model-value="(value: boolean) => config.enabled = value"
            />
          </div>
        </div>
        <div class="mt-5 grid gap-4 sm:grid-cols-2 lg:max-w-xl">
          <label class="space-y-2">
            <span class="text-sm font-medium text-foreground">最小延迟（ms）</span>
            <Input
              :model-value="config.delay_min_ms"
              type="number"
              min="0"
              max="60000"
              step="1"
              class="h-9"
              @update:model-value="(value) => updateDelay('delay_min_ms', value)"
            />
          </label>
          <label class="space-y-2">
            <span class="text-sm font-medium text-foreground">最大延迟（ms）</span>
            <Input
              :model-value="config.delay_max_ms"
              type="number"
              min="0"
              max="60000"
              step="1"
              class="h-9"
              @update:model-value="(value) => updateDelay('delay_max_ms', value)"
            />
          </label>
        </div>
      </section>

      <CardSection
        title="提示词与回复"
        description="每条规则配置一个短测活提示词和对应的本地回复。"
      >
        <div class="space-y-4">
          <div class="flex items-center justify-between gap-3">
            <div class="text-sm text-muted-foreground">
              系统预置规则可恢复默认，自定义规则可删除。
            </div>
            <Button
              variant="outline"
              size="sm"
              @click="addRule"
            >
              <Plus class="mr-2 h-4 w-4" />
              新增规则
            </Button>
          </div>

          <div class="overflow-x-auto rounded-lg border border-border">
            <table class="min-w-[980px] w-full text-sm">
              <thead class="bg-muted/50 text-left text-xs font-medium text-muted-foreground">
                <tr>
                  <th class="w-[180px] px-4 py-3">
                    名称
                  </th>
                  <th class="w-[280px] px-4 py-3">
                    提示词
                  </th>
                  <th class="px-4 py-3">
                    回复
                  </th>
                  <th class="w-[130px] px-4 py-3">
                    类型
                  </th>
                  <th class="w-[100px] px-4 py-3">
                    启用
                  </th>
                  <th class="w-[130px] px-4 py-3 text-right">
                    操作
                  </th>
                </tr>
              </thead>
              <tbody>
                <tr
                  v-for="(rule, index) in config.rules"
                  :key="rule.id"
                  class="border-t border-border align-top"
                >
                  <td class="px-4 py-3">
                    <Input
                      :model-value="rule.name"
                      class="h-9"
                      @update:model-value="(value) => updateRule(index, { name: String(value) })"
                    />
                    <div
                      v-if="rule.system"
                      class="mt-1 text-[11px] text-muted-foreground"
                    >
                      系统预置
                    </div>
                  </td>
                  <td class="px-4 py-3">
                    <Textarea
                      :model-value="rule.prompt"
                      class="min-h-[66px] font-mono text-xs"
                      maxlength="512"
                      @update:model-value="(value) => updateRule(index, { prompt: String(value) })"
                    />
                    <p class="mt-1 text-[11px] text-muted-foreground">
                      匹配键：{{ normalizedPromptPreview(rule.prompt) || '-' }}
                    </p>
                  </td>
                  <td class="px-4 py-3">
                    <Textarea
                      :model-value="rule.response"
                      class="min-h-[66px] font-mono text-xs"
                      maxlength="8000"
                      @update:model-value="(value) => updateRule(index, { response: String(value) })"
                    />
                  </td>
                  <td class="px-4 py-3">
                    <Select
                      :model-value="rule.kind"
                      @update:model-value="(value) => updateRule(index, { kind: value as LocalProbeInterceptKind })"
                    >
                      <SelectTrigger class="h-9 rounded-lg">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="health">
                          健康检查
                        </SelectItem>
                        <SelectItem value="ping">
                          Ping
                        </SelectItem>
                      </SelectContent>
                    </Select>
                  </td>
                  <td class="px-4 py-3">
                    <Switch
                      :model-value="rule.enabled"
                      @update:model-value="(value: boolean) => updateRule(index, { enabled: value })"
                    />
                  </td>
                  <td class="px-4 py-3">
                    <div class="flex justify-end gap-1">
                      <Button
                        v-if="rule.system"
                        variant="ghost"
                        size="icon"
                        class="h-8 w-8"
                        title="恢复默认"
                        @click="resetSystemRule(index)"
                      >
                        <RotateCcw class="h-4 w-4" />
                      </Button>
                      <Button
                        v-if="!rule.system"
                        variant="ghost"
                        size="icon"
                        class="h-8 w-8 text-destructive"
                        title="删除"
                        @click="removeRule(index)"
                      >
                        <Trash2 class="h-4 w-4" />
                      </Button>
                    </div>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>
        </div>
      </CardSection>
    </div>
  </PageContainer>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Activity, Plus, RefreshCw, RotateCcw, Trash2 } from 'lucide-vue-next'
import { PageContainer, PageHeader, CardSection } from '@/components/layout'
import Button from '@/components/ui/button.vue'
import Input from '@/components/ui/input.vue'
import Switch from '@/components/ui/switch.vue'
import Textarea from '@/components/ui/textarea.vue'
import Select from '@/components/ui/select.vue'
import SelectContent from '@/components/ui/select-content.vue'
import SelectItem from '@/components/ui/select-item.vue'
import SelectTrigger from '@/components/ui/select-trigger.vue'
import SelectValue from '@/components/ui/select-value.vue'
import {
  LOCAL_PROBE_INTERCEPT_DEFAULT_DELAY_MAX_MS,
  LOCAL_PROBE_INTERCEPT_DEFAULT_DELAY_MIN_MS,
  LOCAL_PROBE_INTERCEPT_MAX_DELAY_MS,
  LOCAL_PROBE_INTERCEPT_DEFAULT_RULES,
  modulesApi,
  type LocalProbeInterceptConfig,
  type LocalProbeInterceptKind,
  type LocalProbeInterceptRule,
} from '@/api/modules'
import { parseNumberInput } from '@/utils/form'
import { useModuleStore } from '@/stores/modules'
import { useToast } from '@/composables/useToast'
import { parseApiError } from '@/utils/errorParser'
import { log } from '@/utils/logger'

const defaultConfig: LocalProbeInterceptConfig = {
  enabled: true,
  rules: LOCAL_PROBE_INTERCEPT_DEFAULT_RULES.map(rule => ({ ...rule })),
  delay_min_ms: LOCAL_PROBE_INTERCEPT_DEFAULT_DELAY_MIN_MS,
  delay_max_ms: LOCAL_PROBE_INTERCEPT_DEFAULT_DELAY_MAX_MS,
}

const moduleStore = useModuleStore()
const { success, error } = useToast()

const loading = ref(false)
const saving = ref(false)
const config = ref<LocalProbeInterceptConfig>(cloneConfig(defaultConfig))
const originalConfig = ref<LocalProbeInterceptConfig>(cloneConfig(defaultConfig))

const hasChanges = computed(() => JSON.stringify(config.value) !== JSON.stringify(originalConfig.value))

const statusLabel = computed(() => {
  const moduleStatus = moduleStore.modules.local_probe_intercept
  if (moduleStatus && !moduleStatus.config_validated) return '配置异常'
  return config.value.enabled ? '已开启' : ''
})

function cloneConfig(value: LocalProbeInterceptConfig): LocalProbeInterceptConfig {
  return {
    enabled: value.enabled,
    rules: value.rules.map(rule => ({ ...rule })),
    delay_min_ms: value.delay_min_ms,
    delay_max_ms: value.delay_max_ms,
  }
}

type LocalProbeDelayKey = 'delay_min_ms' | 'delay_max_ms'

function updateDelay(field: LocalProbeDelayKey, value: string | number) {
  config.value[field] = Math.floor(parseNumberInput(value, {
    min: 0,
    max: LOCAL_PROBE_INTERCEPT_MAX_DELAY_MS,
  }) ?? 0)
}

function updateRule(index: number, patch: Partial<LocalProbeInterceptRule>) {
  const rules = [...config.value.rules]
  rules[index] = { ...rules[index], ...patch }
  config.value.rules = rules
}

function addRule() {
  config.value.rules = [
    ...config.value.rules,
    {
      id: `custom_${Date.now().toString(36)}`,
      name: '自定义测活',
      prompt: '',
      response: '',
      kind: 'health',
      enabled: true,
      system: false,
    },
  ]
}

function removeRule(index: number) {
  config.value.rules = config.value.rules.filter((_, itemIndex) => itemIndex !== index)
}

function resetSystemRule(index: number) {
  const rule = config.value.rules[index]
  const defaultRule = LOCAL_PROBE_INTERCEPT_DEFAULT_RULES.find(item => item.id === rule.id)
  if (!defaultRule) return
  updateRule(index, { ...defaultRule })
}

function normalizedPromptPreview(prompt: string): string {
  return normalizedPromptKey(prompt).slice(0, 80)
}

function normalizedPromptKey(prompt: string): string {
  return prompt
    .split(/\s+/)
    .filter(Boolean)
    .join(' ')
    .replace(/[A-Z]/g, value => value.toLowerCase())
    .replace(/[\s.,!:;"'?？。，！：；“”‘’、]/g, '')
}

function sanitizeRules(): LocalProbeInterceptRule[] | null {
  const seenIds = new Set<string>()
  const seenPrompts = new Set<string>()
  const rules: LocalProbeInterceptRule[] = []
  for (const [index, rule] of config.value.rules.entries()) {
    const id = normalizeRuleId(rule.id || `custom_${index + 1}`, index)
    const name = rule.name.trim()
    const prompt = rule.prompt.trim()
    const response = rule.response.trim()
    const promptKey = normalizedPromptPreview(prompt)
    if (!name || !prompt || !response) {
      error('规则名称、提示词和回复不能为空')
      return null
    }
    if (!promptKey) {
      error('提示词不能只包含空白或标点')
      return null
    }
    if (seenPrompts.has(promptKey)) {
      error(`提示词重复：${prompt}`)
      return null
    }
    seenPrompts.add(promptKey)
    const uniqueId = seenIds.has(id) ? `${id}_${index + 1}` : id
    seenIds.add(uniqueId)
    rules.push({
      id: uniqueId,
      name,
      prompt,
      response,
      kind: rule.kind === 'ping' ? 'ping' : 'health',
      enabled: rule.enabled,
      system: rule.system === true,
    })
  }
  return rules
}

function sanitizeDelayRange(): Pick<LocalProbeInterceptConfig, 'delay_min_ms' | 'delay_max_ms'> | null {
  const delayMinMs = Math.floor(Number(config.value.delay_min_ms))
  const delayMaxMs = Math.floor(Number(config.value.delay_max_ms))
  if (
    !Number.isFinite(delayMinMs)
    || !Number.isFinite(delayMaxMs)
    || delayMinMs < 0
    || delayMaxMs < 0
    || delayMinMs > LOCAL_PROBE_INTERCEPT_MAX_DELAY_MS
    || delayMaxMs > LOCAL_PROBE_INTERCEPT_MAX_DELAY_MS
  ) {
    error(`随机延迟必须是 0 到 ${LOCAL_PROBE_INTERCEPT_MAX_DELAY_MS} ms 之间的整数`)
    return null
  }
  if (delayMinMs > delayMaxMs) {
    error('随机延迟最小值不能大于最大值')
    return null
  }
  return {
    delay_min_ms: delayMinMs,
    delay_max_ms: delayMaxMs,
  }
}

function normalizeRuleId(raw: string, index: number): string {
  const normalized = raw.trim().replace(/[^A-Za-z0-9_.-]/g, '_').replace(/^_+|_+$/g, '')
  return normalized ? normalized.slice(0, 64) : `custom_${index + 1}`
}

async function loadConfig() {
  loading.value = true
  try {
    const [savedConfig] = await Promise.all([
      modulesApi.getLocalProbeInterceptConfig(),
      moduleStore.fetchModules(),
    ])
    config.value = cloneConfig(savedConfig)
    originalConfig.value = cloneConfig(savedConfig)
  } catch (err) {
    error(parseApiError(err, '加载测活拦截配置失败'))
    log.error('加载测活拦截配置失败:', err)
  } finally {
    loading.value = false
  }
}

async function saveConfig() {
  const rules = sanitizeRules()
  if (!rules) return
  const delayRange = sanitizeDelayRange()
  if (!delayRange) return
  saving.value = true
  try {
    const saved = await modulesApi.updateLocalProbeInterceptConfig({
      enabled: config.value.enabled,
      rules,
      ...delayRange,
    })
    config.value = cloneConfig(saved)
    originalConfig.value = cloneConfig(saved)
    await moduleStore.fetchModules()
    success('测活拦截配置已保存')
  } catch (err) {
    error(parseApiError(err, '保存测活拦截配置失败'))
    log.error('保存测活拦截配置失败:', err)
  } finally {
    saving.value = false
  }
}

onMounted(loadConfig)
</script>
