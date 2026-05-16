<template>
  <!-- 聚合数据导入对话框 -->
  <Dialog
    :open="aggregateImportDialogOpen"
    title="导入聚合数据"
    description="选择冲突处理模式并确认导入"
    @update:open="$emit('update:aggregateImportDialogOpen', $event)"
  >
    <div class="space-y-4">
      <div
        v-if="aggregateImportPreview"
        class="text-sm"
      >
        <p class="font-medium mb-2">
          聚合数据预览
        </p>
        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4 text-muted-foreground">
          <div>
            <p class="font-medium text-foreground mb-1">
              配置数据
            </p>
            <ul class="space-y-1">
              <li>全局模型: {{ aggregateImportPreview.config_data.global_models?.length || 0 }} 个</li>
              <li>提供商: {{ aggregateImportPreview.config_data.providers?.length || 0 }} 个</li>
              <li>
                API Keys: {{ aggregateImportPreview.config_data.providers?.reduce((sum: number, p: { api_keys?: unknown[] }) => sum + (p.api_keys?.length || 0), 0) }} 个
              </li>
            </ul>
          </div>
          <div>
            <p class="font-medium text-foreground mb-1">
              用户数据
            </p>
            <ul class="space-y-1">
              <li v-if="aggregateImportPreview.user_data.user_groups?.length">
                用户组: {{ aggregateImportPreview.user_data.user_groups.length }} 个
              </li>
              <li>用户: {{ aggregateImportPreview.user_data.users?.length || 0 }} 个</li>
              <li>
                API Keys: {{ aggregateImportPreview.user_data.users?.reduce((sum: number, u: { api_keys?: unknown[] }) => sum + (u.api_keys?.length || 0), 0) }} 个
              </li>
              <li v-if="aggregateImportPreview.user_data.standalone_keys?.length">
                独立余额 Keys: {{ aggregateImportPreview.user_data.standalone_keys.length }} 个
              </li>
            </ul>
          </div>
        </div>
      </div>

      <div>
        <Label class="block text-sm font-medium mb-2">冲突处理模式</Label>
        <Select
          :model-value="aggregateMergeMode"
          :open="aggregateMergeModeSelectOpen"
          @update:model-value="$emit('update:aggregateMergeMode', $event)"
          @update:open="$emit('update:aggregateMergeModeSelectOpen', $event)"
        >
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="skip">
              跳过 - 保留现有数据
            </SelectItem>
            <SelectItem value="overwrite">
              覆盖 - 用导入数据替换
            </SelectItem>
            <SelectItem value="error">
              报错 - 遇到冲突时中止
            </SelectItem>
          </SelectContent>
        </Select>
        <p class="mt-1 text-xs text-muted-foreground">
          <template v-if="aggregateMergeMode === 'skip'">
            已存在的数据将被保留，仅导入新数据
          </template>
          <template v-else-if="aggregateMergeMode === 'overwrite'">
            已存在的数据将被导入内容覆盖
          </template>
          <template v-else>
            如果发现任何冲突，导入将中止
          </template>
        </p>
      </div>

      <p class="text-xs text-muted-foreground">
        注意：聚合数据会先导入配置数据，再导入用户数据；用户 API Keys 需要目标系统使用相同的 ENCRYPTION_KEY。
      </p>
    </div>

    <template #footer>
      <Button
        variant="outline"
        @click="$emit('update:aggregateImportDialogOpen', false); $emit('update:aggregateMergeModeSelectOpen', false)"
      >
        取消
      </Button>
      <Button
        :disabled="importAggregateLoading"
        @click="$emit('confirm')"
      >
        {{ importAggregateLoading ? '导入中...' : '确认导入' }}
      </Button>
    </template>
  </Dialog>

  <!-- 聚合数据导入结果对话框 -->
  <Dialog
    :open="aggregateImportResultDialogOpen"
    title="聚合数据导入完成"
    @update:open="$emit('update:aggregateImportResultDialogOpen', $event)"
  >
    <div
      v-if="aggregateImportResult"
      class="space-y-4"
    >
      <div class="grid grid-cols-1 sm:grid-cols-2 gap-4 text-sm">
        <div>
          <p class="font-medium">
            配置数据
          </p>
          <p class="text-muted-foreground">
            全局模型创建 {{ aggregateImportResult.config.stats.global_models.created }}，
            提供商创建 {{ aggregateImportResult.config.stats.providers.created }}，
            API Keys 创建 {{ aggregateImportResult.config.stats.keys.created }}
          </p>
        </div>
        <div>
          <p class="font-medium">
            用户数据
          </p>
          <p class="text-muted-foreground">
            用户创建 {{ aggregateImportResult.users.stats.users.created }}，
            API Keys 创建 {{ aggregateImportResult.users.stats.api_keys.created }}，
            跳过 {{ aggregateImportResult.users.stats.users.skipped }} 个用户
          </p>
        </div>
      </div>

      <div
        v-if="warningMessages.length > 0"
        class="p-3 bg-destructive/10 rounded-lg"
      >
        <p class="font-medium text-destructive mb-2">
          警告信息
        </p>
        <ul class="text-sm text-destructive space-y-1">
          <li
            v-for="(message, index) in warningMessages"
            :key="index"
          >
            {{ message }}
          </li>
        </ul>
      </div>
    </div>

    <template #footer>
      <Button @click="$emit('update:aggregateImportResultDialogOpen', false)">
        确定
      </Button>
    </template>
  </Dialog>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import Button from '@/components/ui/button.vue'
import Label from '@/components/ui/label.vue'
import Select from '@/components/ui/select.vue'
import SelectTrigger from '@/components/ui/select-trigger.vue'
import SelectValue from '@/components/ui/select-value.vue'
import SelectContent from '@/components/ui/select-content.vue'
import SelectItem from '@/components/ui/select-item.vue'
import { Dialog } from '@/components/ui'
import type { AggregateExportData, AggregateImportResponse } from '@/api/admin'

const props = defineProps<{
  aggregateImportDialogOpen: boolean
  aggregateImportResultDialogOpen: boolean
  aggregateImportPreview: AggregateExportData | null
  aggregateImportResult: AggregateImportResponse | null
  aggregateMergeMode: 'skip' | 'overwrite' | 'error'
  aggregateMergeModeSelectOpen: boolean
  importAggregateLoading: boolean
}>()

defineEmits<{
  confirm: []
  'update:aggregateImportDialogOpen': [value: boolean]
  'update:aggregateImportResultDialogOpen': [value: boolean]
  'update:aggregateMergeMode': [value: 'skip' | 'overwrite' | 'error']
  'update:aggregateMergeModeSelectOpen': [value: boolean]
}>()

const warningMessages = computed(() => {
  if (!props.aggregateImportResult) return []
  const configErrors = props.aggregateImportResult.config.stats.errors.map((message) => `配置数据: ${message}`)
  const userErrors = props.aggregateImportResult.users.stats.errors.map((message) => `用户数据: ${message}`)
  return [...configErrors, ...userErrors]
})
</script>
