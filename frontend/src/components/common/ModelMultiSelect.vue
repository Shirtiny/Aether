<template>
  <div class="space-y-2">
    <Label class="text-sm font-medium">允许的模型</Label>
    <div class="relative">
      <button
        type="button"
        class="flex h-10 w-full items-center justify-between rounded-lg border bg-background px-3 text-left transition-colors hover:bg-muted/50"
        @click="isOpen = !isOpen"
      >
        <span
          class="truncate text-sm"
          :class="
            modelValue.length ? 'text-foreground' : 'text-muted-foreground'
          "
        >
          {{
            modelValue.length ? `已选择 ${modelValue.length} 个` : '全部可用'
          }}
          <span
            v-if="invalidModels.length"
            class="text-destructive"
          >({{ invalidModels.length }} 个已失效)</span>
        </span>
        <ChevronDown
          class="h-4 w-4 text-muted-foreground transition-transform"
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
        class="absolute z-[90] mt-1 w-full rounded-lg border bg-popover shadow-lg"
      >
        <div
          v-if="showSearch"
          class="sticky top-0 z-10 border-b bg-popover/95 p-1 backdrop-blur supports-[backdrop-filter]:bg-popover/85"
        >
          <div class="relative">
            <Search
              class="pointer-events-none absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
            />
            <Input
              v-model="searchQuery"
              :placeholder="searchPlaceholder"
              class="h-8 rounded-md border-border/60 bg-popover pl-8 text-xs"
              @keydown.stop
            />
          </div>
        </div>

        <div class="max-h-48 overflow-y-auto">
          <div
            v-for="modelName in filteredInvalidModels"
            :key="modelName"
            class="flex cursor-pointer items-center gap-2 bg-destructive/5 px-3 py-2 hover:bg-muted/50"
            @click="removeModel(modelName)"
          >
            <input
              type="checkbox"
              :checked="true"
              class="h-4 w-4 shrink-0 cursor-pointer rounded border-gray-300"
              @click.stop
              @change="removeModel(modelName)"
            >
            <span class="min-w-0 truncate text-sm text-destructive">{{ modelName }}</span>
            <span class="shrink-0 text-xs text-destructive/70">(已失效)</span>
          </div>

          <div
            v-for="model in filteredModels"
            :key="model.name"
            class="flex cursor-pointer items-center gap-2 px-3 py-2 hover:bg-muted/50"
            @click="toggleModel(model.name)"
          >
            <input
              type="checkbox"
              :checked="modelValue.includes(model.name)"
              class="h-4 w-4 shrink-0 cursor-pointer rounded border-gray-300"
              @click.stop
              @change="toggleModel(model.name)"
            >
            <span class="min-w-0 truncate text-sm">{{ model.name }}</span>
          </div>

          <div
            v-if="
              filteredModels.length === 0 && filteredInvalidModels.length === 0
            "
            class="px-3 py-2 text-sm text-muted-foreground"
          >
            {{ searchQuery.trim() ? '未找到匹配项' : '暂无可用模型' }}
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { Input, Label } from '@/components/ui'
import { ChevronDown, Search } from 'lucide-vue-next'
import { useInvalidModels } from '@/composables/useInvalidModels'
import { matchesSearchQuery } from '@/utils/search'

export interface ModelWithName {
  name: string
}

const props = withDefaults(
  defineProps<{
    modelValue: string[]
    models: ModelWithName[]
    searchThreshold?: number
    searchPlaceholder?: string
  }>(),
  {
    searchThreshold: 8,
    searchPlaceholder: '输入模型名搜索...',
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: string[]]
}>()

const isOpen = ref(false)
const searchQuery = ref('')

const { invalidModels } = useInvalidModels(
  computed(() => props.modelValue),
  computed(() => props.models),
)

const showSearch = computed(
  () =>
    props.models.length + invalidModels.value.length >= props.searchThreshold,
)
const filteredInvalidModels = computed(() => {
  if (!showSearch.value || !searchQuery.value.trim()) {
    return invalidModels.value
  }

  return invalidModels.value.filter((modelName) =>
    matchesSearchQuery(searchQuery.value, modelName),
  )
})
const filteredModels = computed(() => {
  if (!showSearch.value || !searchQuery.value.trim()) {
    return props.models
  }

  return props.models.filter((model) =>
    matchesSearchQuery(searchQuery.value, model.name),
  )
})

watch(isOpen, (open) => {
  if (!open) {
    searchQuery.value = ''
  }
})

function toggleModel(name: string) {
  const newValue = [...props.modelValue]
  const index = newValue.indexOf(name)
  if (index === -1) {
    newValue.push(name)
  } else {
    newValue.splice(index, 1)
  }
  emit('update:modelValue', newValue)
}

function removeModel(name: string) {
  const newValue = props.modelValue.filter((m) => m !== name)
  emit('update:modelValue', newValue)
}
</script>
