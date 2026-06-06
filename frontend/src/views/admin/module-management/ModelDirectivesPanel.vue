<template>
  <div class="space-y-4">
    <div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
      <div>
        <h3 class="text-base font-semibold">
          推理参数
        </h3>
        <p class="mt-1 text-sm text-muted-foreground">
          各端点可以分别启用推理参数，并配置推理程度到实际请求参数和值的映射。
        </p>
      </div>
      <div class="flex items-center">
        <Switch
          :model-value="config.reasoning_effort.enabled"
          :disabled="loading"
          @update:model-value="onReasoningEnabledChange"
        />
      </div>
    </div>

    <div class="overflow-hidden rounded-lg border">
      <div class="grid grid-cols-1 gap-2 border-b bg-muted/40 px-4 py-3 text-xs font-medium text-muted-foreground lg:grid-cols-[minmax(0,1fr)_minmax(0,0.7fr)_minmax(0,1.8fr)_auto]">
        <div>API 端点</div>
        <div>推理程度</div>
        <div>映射参数</div>
        <div class="md:text-right">
          状态
        </div>
      </div>
      <div class="divide-y">
        <div
          v-for="format in MODEL_DIRECTIVE_API_FORMATS"
          :key="format.key"
          class="grid grid-cols-1 items-center gap-3 px-4 py-3 lg:grid-cols-[minmax(0,1fr)_minmax(0,0.7fr)_minmax(0,1.8fr)_auto]"
        >
          <div>
            <div class="text-sm font-medium">
              {{ format.label }}
            </div>
          </div>
          <div>
            <Select
              :model-value="selectedEfforts[format.key] ?? 'low'"
              :disabled="loading || !config.reasoning_effort.enabled || !formatConfig(format.key).enabled"
              @update:model-value="value => selectEffort(format.key, value)"
            >
              <SelectTrigger class="h-9 w-28 rounded-lg">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem
                  v-for="effort in DEFAULT_REASONING_SUFFIXES"
                  :key="effort"
                  :value="effort"
                >
                  {{ effort }}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div class="flex items-center gap-2">
            <Input
              v-model="localMappingParams[mappingKey(format.key)]"
              class="h-9 font-mono text-xs"
              :disabled="loading || !config.reasoning_effort.enabled || !formatConfig(format.key).enabled"
              placeholder="{&quot;reasoning_effort&quot;:&quot;low&quot;}"
            />
            <Button
              size="icon"
              variant="ghost"
              class="h-7 w-7 shrink-0 text-muted-foreground"
              :disabled="loading || !config.reasoning_effort.enabled || !formatConfig(format.key).enabled || !hasMappingParamChanges(format.key)"
              @click="saveMappingParam(format.key)"
            >
              <Save class="w-3.5 h-3.5" />
            </Button>
          </div>
          <div class="flex items-center md:justify-end">
            <Switch
              :model-value="formatConfig(format.key).enabled"
              :disabled="loading || !config.reasoning_effort.enabled"
              @update:model-value="value => onApiFormatEnabledChange(format.key, value)"
            />
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { reactive, watch } from 'vue'
import { Save } from 'lucide-vue-next'
import Button from '@/components/ui/button.vue'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui'
import Switch from '@/components/ui/switch.vue'
import Input from '@/components/ui/input.vue'
import {
  DEFAULT_REASONING_SUFFIXES,
  MODEL_DIRECTIVE_API_FORMATS,
  type ReasoningApiFormatConfig,
  type ModelDirectivesConfig,
} from './modelDirectivesConfig'

const props = defineProps<{
  config: ModelDirectivesConfig
  loading: boolean
}>()

const emit = defineEmits<{
  save: []
  'update:config': [value: ModelDirectivesConfig]
}>()

const selectedEfforts = reactive<Record<string, string>>({})
const localMappingParams = reactive<Record<string, string>>({})

watch(() => props.config.reasoning_effort.api_formats, (newFormats) => {
  for (const format of MODEL_DIRECTIVE_API_FORMATS) {
    const fc = newFormats[format.key]
    const selectedEffort = selectedEfforts[format.key] ?? firstMappingEffort(fc?.mappings) ?? 'low'
    selectedEfforts[format.key] = selectedEffort
    const key = mappingKey(format.key, selectedEffort)
    if (localMappingParams[key] === undefined || !hasMappingParamChanges(format.key)) {
      localMappingParams[key] = JSON.stringify(fc?.mappings?.[selectedEffort] ?? {}, null, 2)
    }
  }
}, { immediate: true })

function formatConfig(apiFormat: string): ReasoningApiFormatConfig {
  return props.config.reasoning_effort.api_formats[apiFormat] ?? {
    enabled: true,
    mappings: {},
  }
}

function firstMappingEffort(mappings: Record<string, unknown> | undefined): string | undefined {
  return DEFAULT_REASONING_SUFFIXES.find((effort) => mappings?.[effort] !== undefined)
}

function mappingKey(apiFormat: string, effort = selectedEfforts[apiFormat] ?? 'low'): string {
  return `${apiFormat}:${effort}`
}

function selectEffort(apiFormat: string, effort: string) {
  selectedEfforts[apiFormat] = effort
  const key = mappingKey(apiFormat, effort)
  localMappingParams[key] = JSON.stringify(formatConfig(apiFormat).mappings[effort] ?? {}, null, 2)
}

function hasMappingParamChanges(apiFormat: string): boolean {
  const effort = selectedEfforts[apiFormat] ?? 'low'
  return (localMappingParams[mappingKey(apiFormat, effort)] ?? '') !== JSON.stringify(formatConfig(apiFormat).mappings[effort] ?? {}, null, 2)
}

function onReasoningEnabledChange(value: boolean) {
  emit('update:config', {
    ...props.config,
    reasoning_effort: { ...props.config.reasoning_effort, enabled: Boolean(value) },
  })
  emit('save')
}

function onApiFormatEnabledChange(apiFormat: string, value: boolean) {
  const current = formatConfig(apiFormat)
  emit('update:config', {
    ...props.config,
    reasoning_effort: {
      ...props.config.reasoning_effort,
      api_formats: {
        ...props.config.reasoning_effort.api_formats,
        [apiFormat]: { ...current, enabled: Boolean(value) },
      },
    },
  })
  emit('save')
}

function saveMappingParam(apiFormat: string) {
  const current = formatConfig(apiFormat)
  const effort = selectedEfforts[apiFormat] ?? 'low'
  let mapping: unknown
  try {
    const parsed = JSON.parse(localMappingParams[mappingKey(apiFormat, effort)] || '{}')
    mapping = parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {}
  } catch {
    localMappingParams[mappingKey(apiFormat, effort)] = JSON.stringify(current.mappings[effort] ?? {}, null, 2)
    return
  }
  localMappingParams[mappingKey(apiFormat, effort)] = JSON.stringify(mapping, null, 2)
  emit('update:config', {
    ...props.config,
    reasoning_effort: {
      ...props.config.reasoning_effort,
      api_formats: {
        ...props.config.reasoning_effort.api_formats,
        [apiFormat]: {
          ...current,
          mappings: {
            ...current.mappings,
            [effort]: mapping,
          },
        },
      },
    },
  })
  emit('save')
}
</script>
