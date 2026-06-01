<template>
  <div class="px-6 py-3.5 border-b border-border/60">
    <div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
      <div>
        <h3 class="text-base font-semibold">
          {{ title }}
        </h3>
        <p class="mt-1 text-xs text-muted-foreground">
          {{ description }}
        </p>
      </div>
      <div class="flex items-center gap-3">
        <Label class="text-xs text-muted-foreground">回溯时间：</Label>
        <Select v-model="selectedLookbackHours">
          <SelectTrigger class="w-28 h-8 text-xs border-border/60">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="1">
              1 小时
            </SelectItem>
            <SelectItem value="6">
              6 小时
            </SelectItem>
            <SelectItem value="12">
              12 小时
            </SelectItem>
            <SelectItem value="24">
              24 小时
            </SelectItem>
            <SelectItem value="48">
              48 小时
            </SelectItem>
          </SelectContent>
        </Select>
        <RefreshButton
          :loading="loading"
          @click="$emit('refresh')"
        />
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import Label from '@/components/ui/label.vue'
import Select from '@/components/ui/select.vue'
import SelectTrigger from '@/components/ui/select-trigger.vue'
import SelectValue from '@/components/ui/select-value.vue'
import SelectContent from '@/components/ui/select-content.vue'
import SelectItem from '@/components/ui/select-item.vue'
import RefreshButton from '@/components/ui/refresh-button.vue'

const props = defineProps<{
  title: string
  description: string
  lookbackHours: string
  loading: boolean
}>()

const emit = defineEmits<{
  'update:lookbackHours': [value: string]
  refresh: []
}>()

const selectedLookbackHours = computed({
  get: () => props.lookbackHours,
  set: value => emit('update:lookbackHours', value)
})
</script>
