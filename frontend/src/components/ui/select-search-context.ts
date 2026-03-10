import type { InjectionKey, Ref } from 'vue'

export interface RegisteredSelectItem {
  value: string
  text: string
}

export interface SelectSearchContext {
  searchQuery: Ref<string>
  hiddenValues: Ref<Set<string>>
  registerItem: (id: string, item: RegisteredSelectItem) => void
  updateItem: (id: string, item: Partial<RegisteredSelectItem>) => void
  unregisterItem: (id: string) => void
}

export const SELECT_SEARCH_CONTEXT_KEY: InjectionKey<SelectSearchContext> =
  Symbol('select-search-context')
