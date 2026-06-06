<template>
  <main class="min-h-screen bg-[#faf9f5] text-[#3d3929] dark:bg-[#191714] dark:text-[#e3e0d3]">
    <header class="border-b border-[#3d3929]/10 dark:border-white/10">
      <div class="mx-auto flex max-w-4xl items-center justify-between px-5 py-4">
        <RouterLink
          to="/"
          class="flex items-center gap-3"
        >
          <HeaderLogo
            size="h-9 w-9"
            class-name="text-[#191919] dark:text-white"
          />
          <div>
            <div class="text-sm font-semibold">
              {{ siteName }}
            </div>
            <div class="text-xs text-muted-foreground">
              隐私政策
            </div>
          </div>
        </RouterLink>
        <RouterLink
          to="/"
          class="rounded-lg border border-border px-3 py-1.5 text-sm text-muted-foreground transition hover:text-foreground"
        >
          返回首页
        </RouterLink>
      </div>
    </header>

    <section class="mx-auto max-w-4xl px-5 py-8">
      <div class="mb-6">
        <h1 class="text-2xl font-semibold">
          隐私政策
        </h1>
        <p class="mt-2 text-sm text-muted-foreground">
          当前版本：{{ policy.version || '1' }}
        </p>
      </div>

      <div
        v-if="loading"
        class="rounded-lg border border-border bg-background/70 p-6 text-sm text-muted-foreground"
      >
        正在加载...
      </div>
      <div
        v-else-if="loadError"
        class="rounded-lg border border-destructive/20 bg-destructive/5 p-6 text-sm text-destructive"
      >
        {{ loadError }}
      </div>
      <!-- eslint-disable vue/no-v-html -->
      <article
        v-else
        class="prose prose-sm dark:prose-invert max-w-none rounded-lg border border-border bg-background/70 p-6"
        v-html="renderedPolicy"
      />
      <!-- eslint-enable vue/no-v-html -->
    </section>
  </main>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { marked } from 'marked'
import { authApi, type RegistrationPrivacyPolicySettings } from '@/api/auth'
import HeaderLogo from '@/components/HeaderLogo.vue'
import { useSiteInfo } from '@/composables/useSiteInfo'
import { sanitizeHtml, sanitizeMarkdown } from '@/utils/sanitize'

const { siteName } = useSiteInfo()
const loading = ref(true)
const loadError = ref('')
const policy = ref<RegistrationPrivacyPolicySettings>({
  enabled: false,
  format: 'markdown',
  content: '',
  version: '1'
})

const renderedPolicy = computed(() => {
  if (!policy.value.content) return '<p>暂无隐私政策内容。</p>'
  if (policy.value.format === 'html') {
    return sanitizeHtml(policy.value.content)
  }
  return sanitizeMarkdown(marked(policy.value.content) as string)
})

onMounted(async () => {
  loading.value = true
  loadError.value = ''
  try {
    const settings = await authApi.getRegistrationSettings()
    policy.value = settings.privacy_policy ?? policy.value
  } catch {
    loadError.value = '隐私政策加载失败，请稍后重试。'
  } finally {
    loading.value = false
  }
})
</script>
