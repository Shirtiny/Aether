<template>
  <section class="space-y-4">
    <div class="rounded-lg border border-border/60 p-3">
      <p
        v-for="line in summary"
        :key="line"
        class="text-sm text-muted-foreground"
      >
        {{ line }}
      </p>
    </div>

    <div class="space-y-2">
      <div
        v-for="candidate in candidates"
        :key="`${candidate.provider_id}-${candidate.endpoint_id}-${candidate.key_id ?? 'pool'}`"
        class="rounded-lg border border-border/60 px-3 py-2"
      >
        <div class="flex items-center justify-between gap-3">
          <p class="truncate text-sm font-medium">
            {{ candidateTraceLabel(candidate) }}
          </p>
          <span class="text-xs text-muted-foreground">
            {{ candidate.skip_reason || `#${candidate.selected_order ?? '-'}` }}
          </span>
        </div>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'

import { candidateTraceLabel, sortCandidateTraces, summarizeRoutingTrace, type RoutingDecisionTrace } from '../utils/routingTrace'

const props = defineProps<{
  trace: RoutingDecisionTrace
}>()

const summary = computed(() => summarizeRoutingTrace(props.trace))
const candidates = computed(() => sortCandidateTraces(props.trace.global_candidates))
</script>
