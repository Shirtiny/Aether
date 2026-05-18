<template>
  <div class="space-y-2">
    <div
      v-for="rule in rules"
      :key="rule.id"
      class="rounded-lg border border-border/60 px-3 py-2"
    >
      <div class="flex items-center justify-between gap-3">
        <p class="truncate text-sm font-medium">
          {{ rule.id }}
        </p>
        <span class="rounded-md bg-muted px-2 py-1 text-xs text-muted-foreground">
          P{{ rule.priority }} / {{ rule.phase }}
        </span>
      </div>
      <p class="mt-1 text-xs text-muted-foreground">
        {{ summarizeRule(rule) }}
      </p>
    </div>
  </div>
</template>

<script setup lang="ts">
import { summarizeRoutingCondition } from '../utils/routingConditions'
import type { RoutingRule } from '../utils/routingPolicy'

defineProps<{
  rules: RoutingRule[]
}>()

function summarizeRule(rule: RoutingRule): string {
  return summarizeRoutingCondition(rule.conditions as never)
}
</script>
