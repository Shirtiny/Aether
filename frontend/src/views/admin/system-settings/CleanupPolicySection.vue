<template>
  <CardSection
    title="请求记录清理策略"
    description="配置请求记录的分级保留和自动清理"
  >
    <template #actions>
      <div class="flex items-center gap-4">
        <div class="flex items-center gap-2">
          <Switch
            id="enable-auto-cleanup"
            :model-value="enableAutoCleanup"
            @update:model-value="$emit('toggleAutoCleanup', $event)"
          />
          <div>
            <Label
              for="enable-auto-cleanup"
              class="text-sm cursor-pointer"
            >
              启用自动清理
            </Label>
            <p class="text-xs text-muted-foreground">
              每天凌晨执行
            </p>
          </div>
        </div>
        <Button
          size="sm"
          :disabled="loading || !hasChanges"
          @click="$emit('save')"
        >
          {{ loading ? '保存中...' : '保存' }}
        </Button>
      </div>
    </template>
    <div class="grid grid-cols-1 md:grid-cols-2 gap-6">
      <div>
        <Label
          for="detail-log-retention-days"
          class="block text-sm font-medium"
        >
          详细记录保留天数
        </Label>
        <Input
          id="detail-log-retention-days"
          :model-value="detailLogRetentionDays"
          type="number"
          placeholder="7"
          class="mt-1"
          @update:model-value="$emit('update:detailLogRetentionDays', Number($event))"
        />
        <p class="mt-1 text-xs text-muted-foreground">
          超过后压缩 body 字段
        </p>
      </div>

      <div>
        <Label
          for="compressed-log-retention-days"
          class="block text-sm font-medium"
        >
          压缩记录保留天数
        </Label>
        <Input
          id="compressed-log-retention-days"
          :model-value="compressedLogRetentionDays"
          type="number"
          placeholder="30"
          class="mt-1"
          @update:model-value="$emit('update:compressedLogRetentionDays', Number($event))"
        />
        <p class="mt-1 text-xs text-muted-foreground">
          超过后删除 body 字段
        </p>
      </div>

      <div>
        <Label
          for="header-retention-days"
          class="block text-sm font-medium"
        >
          请求头保留天数
        </Label>
        <Input
          id="header-retention-days"
          :model-value="headerRetentionDays"
          type="number"
          placeholder="90"
          class="mt-1"
          @update:model-value="$emit('update:headerRetentionDays', Number($event))"
        />
        <p class="mt-1 text-xs text-muted-foreground">
          超过后清空 headers 字段
        </p>
      </div>

      <div>
        <Label
          for="log-retention-days"
          class="block text-sm font-medium"
        >
          请求记录保存天数
        </Label>
        <Input
          id="log-retention-days"
          :model-value="logRetentionDays"
          type="number"
          placeholder="365"
          class="mt-1"
          @update:model-value="$emit('update:logRetentionDays', Number($event))"
        />
        <p class="mt-1 text-xs text-muted-foreground">
          超过后删除整条记录
        </p>
      </div>

      <div>
        <Label
          for="cleanup-batch-size"
          class="block text-sm font-medium"
        >
          每批次清理记录数
        </Label>
        <Input
          id="cleanup-batch-size"
          :model-value="cleanupBatchSize"
          type="number"
          placeholder="1000"
          class="mt-1"
          @update:model-value="$emit('update:cleanupBatchSize', Number($event))"
        />
        <p class="mt-1 text-xs text-muted-foreground">
          避免单次操作过大影响性能
        </p>
      </div>

      <div>
        <Label
          for="audit-log-retention-days"
          class="block text-sm font-medium"
        >
          审计日志保留天数
        </Label>
        <Input
          id="audit-log-retention-days"
          :model-value="auditLogRetentionDays"
          type="number"
          placeholder="30"
          class="mt-1"
          @update:model-value="$emit('update:auditLogRetentionDays', Number($event))"
        />
        <p class="mt-1 text-xs text-muted-foreground">
          超过后删除审计日志记录
        </p>
      </div>
    </div>

    <!-- 清理策略说明 -->
    <div class="mt-4 p-4 bg-muted/50 rounded-lg">
      <h4 class="text-sm font-medium mb-2">
        清理策略说明
      </h4>
      <div class="text-xs text-muted-foreground space-y-1">
        <p>1. <strong>详细日志阶段</strong>: 保留完整的 request_body 和 response_body</p>
        <p>2. <strong>压缩日志阶段</strong>: body 字段被压缩存储，节省空间</p>
        <p>3. <strong>统计阶段</strong>: 仅保留 tokens、成本等统计信息</p>
        <p>4. <strong>归档删除</strong>: 超过保留期限后完全删除记录</p>
        <p>5. <strong>候选记录</strong>: 与详细记录同步清理请求候选记录（request_candidates）</p>
        <p>6. <strong>审计日志</strong>: 独立清理，记录用户登录、操作等安全事件</p>
      </div>
    </div>
  </CardSection>
</template>

<script setup lang="ts">
import Button from '@/components/ui/button.vue'
import Input from '@/components/ui/input.vue'
import Label from '@/components/ui/label.vue'
import Switch from '@/components/ui/switch.vue'
import { CardSection } from '@/components/layout'

defineProps<{
  enableAutoCleanup: boolean
  detailLogRetentionDays: number
  compressedLogRetentionDays: number
  headerRetentionDays: number
  logRetentionDays: number
  cleanupBatchSize: number
  auditLogRetentionDays: number
  loading: boolean
  hasChanges: boolean
}>()

defineEmits<{
  save: []
  toggleAutoCleanup: [enabled: boolean]
  'update:detailLogRetentionDays': [value: number]
  'update:compressedLogRetentionDays': [value: number]
  'update:headerRetentionDays': [value: number]
  'update:logRetentionDays': [value: number]
  'update:cleanupBatchSize': [value: number]
  'update:auditLogRetentionDays': [value: number]
}>()
</script>
