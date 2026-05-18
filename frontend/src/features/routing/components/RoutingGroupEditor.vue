<template>
  <section class="space-y-4">
    <div class="grid gap-3">
      <label class="space-y-1 text-sm">
        <span class="text-muted-foreground">允许模型</span>
        <input
          v-model="allowedModelsText"
          class="h-10 w-full rounded-md border border-border bg-background px-3 text-sm"
          placeholder="gpt-5, claude-sonnet-*"
        >
      </label>
    </div>

    <RoutingModelPolicyEditor
      :model-policies="config.model_policies"
      @update:model-policies="updateModelPolicies"
    />
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'

import RoutingModelPolicyEditor from './RoutingModelPolicyEditor.vue'
import { normalizeRoutingGroupConfig, type RoutingGroupConfig, type RoutingModelPolicy } from '../utils/routingPolicy'

const props = defineProps<{
  config: RoutingGroupConfig
}>()

const emit = defineEmits<{
  'update:config': [value: RoutingGroupConfig]
}>()

const config = computed(() => normalizeRoutingGroupConfig(props.config))

const allowedModelsText = computed({
  get: () => config.value.allowed_models.join(', '),
  set: value => {
    emit('update:config', {
      ...config.value,
      allowed_models: value.split(',').map(item => item.trim()).filter(Boolean),
    })
  },
})

function updateModelPolicies(modelPolicies: RoutingModelPolicy[]) {
  emit('update:config', {
    ...config.value,
    model_policies: modelPolicies,
  })
}
</script>
