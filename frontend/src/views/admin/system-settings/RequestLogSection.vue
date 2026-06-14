<template>
  <CardSection
    title="请求记录"
    description="控制请求/响应详情的入库方式和内容"
  >
    <template #actions>
      <Button
        size="sm"
        :disabled="loading || !hasChanges"
        @click="$emit('save')"
      >
        {{ loading ? '保存中...' : '保存' }}
      </Button>
    </template>
    <div class="grid grid-cols-1 md:grid-cols-2 gap-6">
      <div>
        <Label
          for="request-log-level"
          class="block text-sm font-medium mb-2"
        >
          记录详细程度
        </Label>
        <Select
          :model-value="requestRecordLevel"
          @update:model-value="$emit('update:requestRecordLevel', $event)"
        >
          <SelectTrigger
            id="request-log-level"
            class="mt-1"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="basic">
              BASIC - 基本信息 (~1KB/条)
            </SelectItem>
            <SelectItem value="headers">
              HEADERS - 含请求头 (~2-3KB/条)
            </SelectItem>
            <SelectItem value="full">
              FULL - 完整请求响应 (~50KB/条)
            </SelectItem>
          </SelectContent>
        </Select>
        <p class="mt-1 text-xs text-muted-foreground">
          敏感信息会自动脱敏
        </p>
      </div>

      <div>
        <Label
          for="max-request-body-size"
          class="block text-sm font-medium"
        >
          最大请求体大小 (KB)
        </Label>
        <Input
          id="max-request-body-size"
          :model-value="maxRequestBodySizeKB"
          type="number"
          placeholder="512"
          class="mt-1"
          @update:model-value="$emit('update:maxRequestBodySizeKB', Number($event))"
        />
        <p class="mt-1 text-xs text-muted-foreground">
          超过此大小的请求体将被截断记录
        </p>
      </div>

      <div>
        <Label
          for="max-response-body-size"
          class="block text-sm font-medium"
        >
          最大响应体大小 (KB)
        </Label>
        <Input
          id="max-response-body-size"
          :model-value="maxResponseBodySizeKB"
          type="number"
          placeholder="512"
          class="mt-1"
          @update:model-value="$emit('update:maxResponseBodySizeKB', Number($event))"
        />
        <p class="mt-1 text-xs text-muted-foreground">
          超过此大小的响应体将被截断记录
        </p>
      </div>

      <div>
        <Label
          for="sensitive-headers"
          class="block text-sm font-medium"
        >
          敏感请求头
        </Label>
        <Input
          id="sensitive-headers"
          :model-value="sensitiveHeadersStr"
          placeholder="authorization, x-api-key, cookie"
          class="mt-1"
          @update:model-value="$emit('update:sensitiveHeadersStr', $event)"
        />
        <p class="mt-1 text-xs text-muted-foreground">
          逗号分隔，这些请求头会被脱敏处理
        </p>
      </div>
    </div>

    <div class="mt-6 border-t border-border pt-6">
      <div class="grid grid-cols-1 gap-6 lg:grid-cols-2">
        <div class="space-y-4">
          <div>
            <h3 class="text-sm font-medium">
              提示词摘要
            </h3>
            <p class="mt-1 text-xs text-muted-foreground">
              只保存 hash、长度和短预览，用于排查异常请求。
            </p>
          </div>

          <label class="flex items-center gap-2 text-sm">
            <Checkbox
              :checked="requestCapturePolicy.prompt_capture.enabled"
              @update:checked="updatePromptCapture('enabled', $event)"
            />
            启用提示词摘要
          </label>

          <div class="grid grid-cols-2 gap-3">
            <label
              v-for="item in promptRoleOptions"
              :key="item.key"
              class="flex items-center gap-2 text-sm"
            >
              <Checkbox
                :checked="Boolean(requestCapturePolicy.prompt_capture[item.key])"
                :disabled="!requestCapturePolicy.prompt_capture.enabled"
                @update:checked="updatePromptCapture(item.key, $event)"
              />
              {{ item.label }}
            </label>
          </div>

          <div class="grid grid-cols-1 gap-4 sm:grid-cols-2">
            <div>
              <Label
                for="prompt-preview-chars"
                class="block text-sm font-medium"
              >
                预览字符数
              </Label>
              <Input
                id="prompt-preview-chars"
                :model-value="requestCapturePolicy.prompt_capture.preview_chars"
                type="number"
                min="0"
                max="8192"
                class="mt-1"
                :disabled="!requestCapturePolicy.prompt_capture.enabled"
                @update:model-value="updatePromptCaptureNumber('preview_chars', $event)"
              />
            </div>
            <div>
              <Label
                for="prompt-max-items"
                class="block text-sm font-medium"
              >
                单请求最大条目
              </Label>
              <Input
                id="prompt-max-items"
                :model-value="requestCapturePolicy.prompt_capture.max_items"
                type="number"
                min="0"
                max="256"
                class="mt-1"
                :disabled="!requestCapturePolicy.prompt_capture.enabled"
                @update:model-value="updatePromptCaptureNumber('max_items', $event)"
              />
            </div>
          </div>
        </div>

        <div class="space-y-4">
          <div>
            <h3 class="text-sm font-medium">
              记录范围
            </h3>
            <p class="mt-1 text-xs text-muted-foreground">
              可限制只对指定用户组记录完整 body 和提示词摘要。
            </p>
          </div>

          <div>
            <Label
              for="request-capture-scope"
              class="block text-sm font-medium mb-2"
            >
              生效范围
            </Label>
            <Select
              :model-value="requestCapturePolicy.scope.mode"
              @update:model-value="updateScopeMode"
            >
              <SelectTrigger
                id="request-capture-scope"
                class="mt-1"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">
                  所有用户
                </SelectItem>
                <SelectItem value="include_groups">
                  仅指定用户组
                </SelectItem>
                <SelectItem value="exclude_groups">
                  排除指定用户组
                </SelectItem>
              </SelectContent>
            </Select>
          </div>

          <div>
            <Label class="block text-sm font-medium mb-2">
              用户组
            </Label>
            <MultiSelect
              :model-value="requestCapturePolicy.scope.group_ids"
              :options="userGroupOptions"
              :disabled="requestCapturePolicy.scope.mode === 'all'"
              placeholder="选择用户组"
              empty-text="暂无用户组"
              @update:model-value="updateScopeGroupIds"
            />
            <p class="mt-1 text-xs text-muted-foreground">
              范围为“所有用户”时忽略用户组选择。
            </p>
          </div>
        </div>
      </div>
    </div>
  </CardSection>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import Button from '@/components/ui/button.vue'
import Checkbox from '@/components/ui/checkbox.vue'
import Input from '@/components/ui/input.vue'
import Label from '@/components/ui/label.vue'
import Select from '@/components/ui/select.vue'
import SelectTrigger from '@/components/ui/select-trigger.vue'
import SelectValue from '@/components/ui/select-value.vue'
import SelectContent from '@/components/ui/select-content.vue'
import SelectItem from '@/components/ui/select-item.vue'
import { CardSection } from '@/components/layout'
import MultiSelect, { type MultiSelectOption } from '@/components/common/MultiSelect.vue'
import type { UserGroup } from '@/api/users'
import type {
  RequestCapturePolicy,
  RequestCaptureScopeMode,
} from './composables/useSystemConfig'

const props = defineProps<{
  requestRecordLevel: string
  maxRequestBodySizeKB: number
  maxResponseBodySizeKB: number
  sensitiveHeadersStr: string
  requestCapturePolicy: RequestCapturePolicy
  userGroups: UserGroup[]
  loading: boolean
  hasChanges: boolean
}>()

const emit = defineEmits<{
  save: []
  'update:requestRecordLevel': [value: string]
  'update:maxRequestBodySizeKB': [value: number]
  'update:maxResponseBodySizeKB': [value: number]
  'update:sensitiveHeadersStr': [value: string]
  'update:requestCapturePolicy': [value: RequestCapturePolicy]
}>()

type PromptCaptureBooleanKey =
  | 'enabled'
  | 'include_system'
  | 'include_developer'
  | 'include_user'
  | 'include_tools'

type PromptCaptureNumberKey = 'preview_chars' | 'max_items'

const promptRoleOptions: Array<{ key: Exclude<PromptCaptureBooleanKey, 'enabled'>, label: string }> = [
  { key: 'include_system', label: 'System' },
  { key: 'include_developer', label: 'Developer' },
  { key: 'include_user', label: 'User' },
  { key: 'include_tools', label: 'Tool' },
]

const userGroupOptions = computed<MultiSelectOption[]>(() =>
  props.userGroups.map(group => ({
    value: group.id,
    label: group.name,
  }))
)

function emitPolicy(next: RequestCapturePolicy) {
  emit('update:requestCapturePolicy', next)
}

function clonePolicy(): RequestCapturePolicy {
  return {
    request_record_level: props.requestRecordLevel,
    max_request_body_bytes: props.maxRequestBodySizeKB * 1024,
    max_response_body_bytes: props.maxResponseBodySizeKB * 1024,
    scope: {
      mode: props.requestCapturePolicy.scope.mode,
      group_ids: [...props.requestCapturePolicy.scope.group_ids],
    },
    prompt_capture: {
      ...props.requestCapturePolicy.prompt_capture,
    },
  }
}

function updatePromptCapture(key: PromptCaptureBooleanKey, value: boolean) {
  const next = clonePolicy()
  next.prompt_capture[key] = value
  emitPolicy(next)
}

function updatePromptCaptureNumber(key: PromptCaptureNumberKey, value: string | number) {
  const next = clonePolicy()
  const numeric = typeof value === 'number' ? value : Number(value)
  next.prompt_capture[key] = Number.isFinite(numeric) ? numeric : 0
  emitPolicy(next)
}

function updateScopeMode(value: string) {
  const next = clonePolicy()
  next.scope.mode = value as RequestCaptureScopeMode
  emitPolicy(next)
}

function updateScopeGroupIds(value: string[]) {
  const next = clonePolicy()
  next.scope.group_ids = value
  emitPolicy(next)
}
</script>
