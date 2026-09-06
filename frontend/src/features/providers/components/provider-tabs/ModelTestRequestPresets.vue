<template>
  <div class="space-y-2 rounded-lg border border-border/60 bg-muted/10 p-3">
    <div class="flex items-center justify-between gap-3">
      <div class="text-sm font-medium">
        请求预设
      </div>
      <Button
        variant="ghost"
        size="sm"
        :disabled="!ready || loading"
        @click="openManager('body')"
      >
        <Settings2 class="mr-1 h-3.5 w-3.5" />
        管理全局模板
      </Button>
    </div>
    <div class="grid gap-3 lg:grid-cols-2">
      <div
        v-for="item in kinds"
        :key="item.kind"
        class="flex min-w-0 items-center gap-2"
      >
        <Select v-model="selected[item.kind]">
          <SelectTrigger
            class="h-8 min-w-0 flex-1 text-xs"
            :aria-label="`${item.label}预设`"
          >
            <SelectValue :placeholder="`${item.label}预设`" />
          </SelectTrigger>
          <SelectContent :disable-portal="false">
            <SelectItem :value="DEFAULT_PRESET">
              {{ item.kind === 'headers' ? '默认请求头（空）' : '当前端点默认请求体' }}
            </SelectItem>
            <SelectItem
              v-for="template in availableTemplates[item.kind]"
              :key="template.id"
              :value="template.id"
            >
              {{ template.name }}
            </SelectItem>
          </SelectContent>
        </Select>
        <Button
          variant="outline"
          size="sm"
          :aria-label="`应用${item.label}预设`"
          :disabled="selected[item.kind] !== DEFAULT_PRESET && !ready"
          @click="applyPreset(item.kind)"
        >
          应用
        </Button>
        <Button
          variant="ghost"
          size="icon"
          class="h-8 w-8 shrink-0"
          :title="`将当前${item.label}存为全局模板`"
          :aria-label="`将当前${item.label}存为全局模板`"
          :disabled="!ready || loading"
          @click="openManager(item.kind, true)"
        >
          <Plus class="h-4 w-4" />
        </Button>
      </div>
    </div>
    <p class="text-[11px] text-muted-foreground">
      {{ loading ? '正在加载全局模板…' : '应用仅替换对应编辑框，不会发起测试；模板在所有供应商和模型间共享，按端点格式筛选。' }}
    </p>
    <div
      v-if="loadError"
      class="flex items-center justify-between gap-2 text-xs text-destructive"
      role="alert"
    >
      <span>{{ loadError }}</span>
      <Button
        variant="ghost"
        size="sm"
        @click="load"
      >
        重试
      </Button>
    </div>
  </div>

  <Dialog
    :open="managerOpen"
    title="全局请求模板"
    description="保存到服务端，所有管理员共享。保存模板不会修改当前测试请求。"
    size="2xl"
    :z-index="80"
    :close-on-backdrop="false"
    @update:open="closeManager"
  >
    <div class="space-y-4">
      <div class="flex gap-2">
        <Button
          v-for="item in kinds"
          :key="item.kind"
          :variant="managerKind === item.kind ? 'default' : 'outline'"
          size="sm"
          :disabled="saving"
          :aria-pressed="managerKind === item.kind"
          @click="switchKind(item.kind)"
        >
          {{ item.label }}模板（{{ templates[item.kind].length }}）
        </Button>
      </div>
      <div class="flex items-center gap-2">
        <Select
          :model-value="editingId || NEW_TEMPLATE"
          :disabled="saving"
          @update:model-value="selectEditingTemplate"
        >
          <SelectTrigger
            class="min-w-0 flex-1"
            aria-label="选择要维护的模板"
          >
            <SelectValue placeholder="选择要维护的模板" />
          </SelectTrigger>
          <SelectContent :disable-portal="false">
            <SelectItem :value="NEW_TEMPLATE">
              新建模板
            </SelectItem>
            <SelectItem
              v-for="template in templates[managerKind]"
              :key="template.id"
              :value="template.id"
            >
              {{ template.name }} · {{ template.api_format ? formatApiFormat(template.api_format) : '所有端点格式' }}
            </SelectItem>
          </SelectContent>
        </Select>
        <Button
          variant="outline"
          size="sm"
          :disabled="saving"
          @click="startNew"
        >
          <Plus class="mr-1 h-4 w-4" />
          从当前请求新建
        </Button>
      </div>
      <div class="grid gap-3 sm:grid-cols-2">
        <div class="space-y-1.5">
          <label
            for="model-test-template-name"
            class="text-sm font-medium"
          >模板名称</label>
          <Input
            id="model-test-template-name"
            v-model="form.name"
            placeholder="例如：简短问答测试"
            :maxlength="100"
            :disabled="saving"
          />
        </div>
        <div class="space-y-1.5">
          <div class="text-sm font-medium">
            适用端点格式
          </div>
          <Select
            v-model="form.apiFormat"
            :disabled="saving"
          >
            <SelectTrigger aria-label="模板适用端点格式">
              <SelectValue />
            </SelectTrigger>
            <SelectContent :disable-portal="false">
              <SelectItem :value="ALL_FORMATS">
                所有端点格式
              </SelectItem>
              <SelectItem
                v-for="format in formatOptions"
                :key="format"
                :value="format"
              >
                {{ formatApiFormat(format) }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>
      <div class="space-y-2">
        <div class="flex items-center justify-between">
          <label
            for="model-test-template-content"
            class="text-sm font-medium"
          >{{ managerKind === 'body' ? '请求体' : '请求头' }} JSON</label>
          <Button
            variant="ghost"
            size="sm"
            :disabled="saving || !!parsedDraft.error"
            @click="formatDraft"
          >
            格式化
          </Button>
        </div>
        <Textarea
          id="model-test-template-content"
          v-model="form.draft"
          class="min-h-[240px] font-mono text-xs"
          :disabled="saving"
          spellcheck="false"
        />
        <p
          v-if="parsedDraft.error"
          class="text-xs text-destructive"
          role="alert"
        >
          {{ parsedDraft.error }}
        </p>
        <p
          v-if="managerKind === 'body'"
          class="text-xs text-muted-foreground"
        >
          <code v-pre>"model": "{{model}}"</code> 或省略 model，应用时使用当前测试模型（含已选映射）；固定模型名则原样保留。
        </p>
        <p class="text-xs text-muted-foreground">
          模板对所有管理员可见，请勿保存 API Key、Cookie 等敏感信息；鉴权信息由后端补齐。
        </p>
      </div>
      <div
        v-if="deletePending"
        class="flex flex-wrap items-center justify-between gap-2 rounded-md border border-destructive/30 bg-destructive/10 p-3 text-xs"
        role="alert"
      >
        <span>删除后所有管理员都将无法使用此模板，确认删除？</span>
        <div class="flex gap-2">
          <Button
            variant="outline"
            size="sm"
            :disabled="saving"
            @click="deletePending = false"
          >
            取消删除
          </Button>
          <Button
            variant="destructive"
            size="sm"
            :disabled="saving"
            @click="deleteTemplate"
          >
            确认删除
          </Button>
        </div>
      </div>
    </div>
    <template #footer>
      <Button
        :disabled="saving || !form.name.trim() || !!parsedDraft.error || deletePending"
        @click="saveTemplate"
      >
        {{ saving ? '保存中…' : '保存模板' }}
      </Button>
      <Button
        variant="outline"
        :disabled="saving"
        @click="closeManager"
      >
        关闭
      </Button>
      <Button
        v-if="editingId"
        variant="ghost"
        class="mr-auto text-destructive"
        :disabled="saving || deletePending"
        @click="deletePending = true"
      >
        删除模板
      </Button>
    </template>
  </Dialog>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from 'vue'
import { Plus, Settings2 } from 'lucide-vue-next'
import { Button, Dialog, Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Textarea } from '@/components/ui'
import { API_FORMAT_ORDER, formatApiFormat, normalizeApiFormatAlias } from '@/api/endpoints/types/api-format'
import { isModelTestableApiFormat } from './model-test-request'
import {
  applyModelTestTemplate,
  buildModelTestTemplateDraft,
  modelTestTemplateMatchesFormat,
  parseModelTestTemplateDraft,
  type ModelTestTemplateKind,
} from './model-test-templates'
import { useModelTestTemplates } from './useModelTestTemplates'

const props = defineProps<{
  apiFormat?: string | null
  modelName: string
  requestHeadersDraft: string
  requestBodyDraft: string
  requestHeadersResetValue: string
  requestBodyResetValue: string
}>()

const emit = defineEmits<{
  'update:requestHeadersDraft': [value: string]
  'update:requestBodyDraft': [value: string]
}>()

const DEFAULT_PRESET = '__default__'
const NEW_TEMPLATE = '__new__'
const ALL_FORMATS = '__all__'
const kinds = [
  { kind: 'headers', label: '请求头' },
  { kind: 'body', label: '请求体' },
] as const
const { templates, loading, saving, ready, loadError, load, save, remove } = useModelTestTemplates()
const selected = reactive({ headers: DEFAULT_PRESET, body: DEFAULT_PRESET })
const availableTemplates = computed(() => ({
  headers: templates.value.headers.filter(template => modelTestTemplateMatchesFormat(template, props.apiFormat)),
  body: templates.value.body.filter(template => modelTestTemplateMatchesFormat(template, props.apiFormat)),
}))

onMounted(load)
watch(availableTemplates, (available) => {
  for (const { kind } of kinds) {
    if (!available[kind].some(template => template.id === selected[kind])) selected[kind] = DEFAULT_PRESET
  }
})

function applyPreset(kind: ModelTestTemplateKind) {
  const template = availableTemplates.value[kind].find(item => item.id === selected[kind])
  if (selected[kind] !== DEFAULT_PRESET && (!ready.value || !template)) return
  const draft = template
    ? applyModelTestTemplate(template, kind, props.modelName)
    : kind === 'headers' ? props.requestHeadersResetValue : props.requestBodyResetValue
  if (kind === 'headers') emit('update:requestHeadersDraft', draft)
  else emit('update:requestBodyDraft', draft)
}

const managerOpen = ref(false)
const managerKind = ref<ModelTestTemplateKind>('body')
const editingId = ref('')
const form = reactive({ name: '', apiFormat: ALL_FORMATS, draft: '{}' })
const deletePending = ref(false)
const parsedDraft = computed(() => parseModelTestTemplateDraft(managerKind.value, form.draft))
const formatOptions = computed(() => [...new Set([
  ...API_FORMAT_ORDER.filter(isModelTestableApiFormat),
  normalizeApiFormatAlias(props.apiFormat ?? ''),
  form.apiFormat === ALL_FORMATS ? '' : form.apiFormat,
])].filter(Boolean))

function startNew() {
  editingId.value = ''
  deletePending.value = false
  form.name = ''
  form.apiFormat = managerKind.value === 'body' && props.apiFormat
    ? normalizeApiFormatAlias(props.apiFormat)
    : ALL_FORMATS
  const draft = managerKind.value === 'headers' ? props.requestHeadersDraft : props.requestBodyDraft
  form.draft = buildModelTestTemplateDraft(managerKind.value, draft, props.modelName)
}

function selectEditingTemplate(id: string) {
  const template = templates.value[managerKind.value].find(item => item.id === id)
  if (!template) {
    startNew()
    return
  }
  editingId.value = id
  deletePending.value = false
  form.name = template.name
  form.apiFormat = template.api_format || ALL_FORMATS
  form.draft = JSON.stringify(template.content, null, 2)
}

function switchKind(kind: ModelTestTemplateKind) {
  managerKind.value = kind
  selectEditingTemplate(templates.value[kind][0]?.id ?? NEW_TEMPLATE)
}

function openManager(kind: ModelTestTemplateKind, fromCurrent = false) {
  switchKind(kind)
  if (fromCurrent) startNew()
  managerOpen.value = true
}

function closeManager() {
  if (!saving.value) managerOpen.value = false
}

function formatDraft() {
  if (parsedDraft.value.value) form.draft = JSON.stringify(parsedDraft.value.value, null, 2)
}

async function saveTemplate() {
  if (!form.name.trim() || !parsedDraft.value.value || deletePending.value) return
  const id = editingId.value || (globalThis.crypto?.randomUUID?.() ?? `template-${Date.now()}-${Math.random().toString(36).slice(2)}`)
  const saved = await save(managerKind.value, {
    id,
    name: form.name.trim(),
    api_format: form.apiFormat === ALL_FORMATS ? null : form.apiFormat,
    content: parsedDraft.value.value,
  }, !editingId.value)
  if (saved) selectEditingTemplate(id)
}

async function deleteTemplate() {
  if (!editingId.value || !deletePending.value) return
  if (await remove(managerKind.value, editingId.value)) {
    selectEditingTemplate(templates.value[managerKind.value][0]?.id ?? NEW_TEMPLATE)
  }
}
</script>
