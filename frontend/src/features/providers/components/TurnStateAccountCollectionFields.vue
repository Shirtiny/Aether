<script setup lang="ts">
import { Label, Switch, Textarea } from '@/components/ui'
import TurnStateCollectionStatusPanel from './TurnStateCollectionStatusPanel.vue'

defineProps<{ enabled: boolean, models: string, providerId: string, keyId?: string }>()
defineEmits<{ 'update:enabled': [value: boolean], 'update:models': [value: string] }>()
</script>

<template>
  <div class="space-y-3 rounded-md border border-border/60 bg-muted/30 p-3">
    <div class="flex items-center justify-between gap-3">
      <Label>账号 Turn-State 票据定时采集</Label>
      <Switch
        :model-value="enabled"
        data-testid="account-turn-state-switch"
        @update:model-value="$emit('update:enabled', $event)"
      />
    </div>
    <p class="text-xs leading-5 text-muted-foreground">
      只使用此账号及其已有认证和代理，每模型 40 分钟采集一次，会产生少量上游用量；需要提供商启用 Responses 端点并返回票据头。
      与账号是否参与号池请求独立：账号停用后仍会采集，关闭本开关或停用提供商才会停止。失败不会换其他账号。
    </p>
    <div
      v-if="enabled"
      class="space-y-1"
    >
      <Label>采集模型（每行一个）</Label>
      <Textarea
        :model-value="models"
        placeholder="gpt-5.6-sol&#10;gpt-6-astra"
        rows="3"
        data-testid="account-turn-state-models"
        @update:model-value="$emit('update:models', String($event))"
      />
    </div>
    <TurnStateCollectionStatusPanel
      v-if="keyId"
      :provider-id="providerId"
      :key-id="keyId"
    />
  </div>
</template>
