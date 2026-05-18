<template>
  <div class="space-y-3">
    <div class="flex items-center justify-between gap-3">
      <h3 class="text-sm font-medium">
        模型策略
      </h3>
      <button
        type="button"
        class="rounded-md border border-border px-3 py-1.5 text-xs"
        @click="addPolicy"
      >
        添加
      </button>
    </div>

    <div
      v-for="(policy, index) in draftPolicies"
      :key="`${policy.model}-${index}`"
      class="grid gap-3 rounded-lg border border-border/60 p-3 sm:grid-cols-[1fr_1fr_auto]"
    >
      <input
        v-model="policy.model"
        class="h-9 rounded-md border border-border bg-background px-3 text-sm"
        placeholder="模型"
        @change="commit"
      >
      <input
        :value="policy.allowed_providers.join(', ')"
        class="h-9 rounded-md border border-border bg-background px-3 text-sm"
        placeholder="允许 Provider"
        @change="event => updateProviders(index, event)"
      >
      <button
        type="button"
        class="rounded-md border border-border px-3 text-xs text-muted-foreground"
        @click="removePolicy(index)"
      >
        删除
      </button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, watch } from 'vue'

import { createEmptyModelPolicy, type RoutingModelPolicy } from '../utils/routingPolicy'

const props = defineProps<{
  modelPolicies: RoutingModelPolicy[]
}>()

const emit = defineEmits<{
  'update:model-policies': [value: RoutingModelPolicy[]]
}>()

const draftPolicies = ref<RoutingModelPolicy[]>(props.modelPolicies.map(policy => ({ ...policy })))

watch(() => props.modelPolicies, value => {
  draftPolicies.value = value.map(policy => ({ ...policy }))
})

function addPolicy() {
  draftPolicies.value.push(createEmptyModelPolicy())
  commit()
}

function removePolicy(index: number) {
  draftPolicies.value.splice(index, 1)
  commit()
}

function updateProviders(index: number, event: Event) {
  const target = event.target as HTMLInputElement
  draftPolicies.value[index].allowed_providers = target.value.split(',').map(item => item.trim()).filter(Boolean)
  commit()
}

function commit() {
  emit('update:model-policies', draftPolicies.value.map(policy => ({ ...policy })))
}
</script>
