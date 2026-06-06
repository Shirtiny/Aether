<script setup lang="ts">
import { PopoverTrigger } from 'radix-vue'
import { useAttrs } from 'vue'

withDefaults(defineProps<{
  asChild?: boolean
  as?: string
}>(), {
  asChild: false,
  as: 'button',
})

defineOptions({
  inheritAttrs: false,
})

const attrs = useAttrs()
</script>

<template>
  <PopoverTrigger
    v-bind="attrs"
    :as-child="asChild"
    :as="as"
  >
    <slot />
  </PopoverTrigger>
</template>
