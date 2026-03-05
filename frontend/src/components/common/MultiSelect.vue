<template>
  <div class="relative">
    <button
      type="button"
      :class="cn(
        'h-9 px-3 border rounded-lg bg-background text-left flex items-center justify-between hover:bg-muted/50 transition-colors gap-1',
        triggerClass,
      )"
      :disabled="disabled"
      @click="isOpen = !isOpen"
    >
      <span
        :class="modelValue.length ? 'text-foreground' : 'text-muted-foreground'"
        class="text-xs truncate"
      >
        {{ displayText }}
      </span>
      <ChevronDown
        class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
        :class="isOpen ? 'rotate-180' : ''"
      />
    </button>
    <div
      v-if="isOpen"
      class="fixed inset-0 z-[80]"
      @click.stop="isOpen = false"
    />
    <div
      v-if="isOpen"
      class="absolute z-[90] w-full mt-1 bg-popover border rounded-lg shadow-lg max-h-48 overflow-y-auto"
      :style="dropdownMinWidth ? { minWidth: dropdownMinWidth } : undefined"
    >
      <div
        v-for="item in options"
        :key="item.value"
        class="flex items-center gap-2 px-3 py-1.5 hover:bg-muted/50 cursor-pointer text-xs"
        @click="toggle(item.value)"
      >
        <input
          type="checkbox"
          :checked="modelValue.includes(item.value)"
          class="h-4 w-4 rounded border-border/60 bg-card/80 text-primary shadow-sm accent-primary cursor-pointer"
          @click.stop
          @change="toggle(item.value)"
        >
        <span class="text-sm">{{ item.label }}</span>
      </div>
      <div
        v-if="options.length === 0"
        class="px-3 py-2 text-sm text-muted-foreground"
      >
        {{ emptyText }}
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { ChevronDown } from 'lucide-vue-next'
import { cn } from '@/lib/utils'

export interface MultiSelectOption {
  value: string
  label: string
}

const props = withDefaults(defineProps<{
  modelValue: string[]
  options: MultiSelectOption[]
  placeholder?: string
  emptyText?: string
  triggerClass?: string
  dropdownMinWidth?: string
  disabled?: boolean
}>(), {
  placeholder: '请选择',
  emptyText: '暂无选项',
  triggerClass: '',
  dropdownMinWidth: undefined,
  disabled: false,
})

const emit = defineEmits<{
  'update:modelValue': [value: string[]]
}>()

const isOpen = ref(false)

const displayText = computed(() => {
  if (props.modelValue.length === 0) return props.placeholder
  if (props.modelValue.length <= 2) {
    return props.modelValue
      .map(v => props.options.find(o => o.value === v)?.label ?? v)
      .join(', ')
  }
  return `已选择 ${props.modelValue.length} 项`
})

function toggle(value: string) {
  const newValue = [...props.modelValue]
  const index = newValue.indexOf(value)
  if (index === -1) {
    newValue.push(value)
  } else {
    newValue.splice(index, 1)
  }
  emit('update:modelValue', newValue)
}
</script>
