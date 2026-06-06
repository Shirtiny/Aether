<template>
  <div class="space-y-6 pb-8">
    <div>
      <h1 class="text-2xl font-semibold text-foreground">
        我的邀请
      </h1>
      <p class="mt-1 text-sm text-muted-foreground">
        分享邀请码后，符合规则的返利会进入赠款余额
      </p>
    </div>

    <div
      v-if="loading"
      class="rounded-lg border border-border bg-card p-6 text-sm text-muted-foreground"
    >
      正在加载...
    </div>

    <template v-else-if="dashboard">
      <div class="grid grid-cols-1 gap-4 md:grid-cols-3">
        <Card class="p-5">
          <p class="text-xs text-muted-foreground">
            总邀请
          </p>
          <p class="mt-2 text-2xl font-semibold">
            {{ dashboard.summary.total_invites }}
          </p>
        </Card>
        <Card class="p-5">
          <p class="text-xs text-muted-foreground">
            有效邀请
          </p>
          <p class="mt-2 text-2xl font-semibold">
            {{ dashboard.summary.effective_invites }}
          </p>
        </Card>
        <Card class="p-5">
          <p class="text-xs text-muted-foreground">
            已发返利
          </p>
          <p class="mt-2 text-2xl font-semibold">
            {{ formatUsd(dashboard.summary.paid_reward_usd) }}
          </p>
        </Card>
      </div>

      <Card class="p-5">
        <div class="grid grid-cols-1 gap-4 lg:grid-cols-[240px_1fr]">
          <div>
            <Label class="text-xs text-muted-foreground">
              邀请码
            </Label>
            <div class="mt-2 flex items-center gap-2">
              <code class="rounded-lg border border-border bg-muted px-3 py-2 font-mono text-sm">
                {{ dashboard.invite_code }}
              </code>
              <Button
                type="button"
                variant="outline"
                size="sm"
                @click="copyToClipboard(dashboard.invite_code)"
              >
                <Copy class="mr-2 h-4 w-4" />
                复制
              </Button>
            </div>
          </div>

          <div>
            <Label class="text-xs text-muted-foreground">
              邀请链接
            </Label>
            <div class="mt-2 flex min-w-0 items-center gap-2">
              <Input
                :model-value="dashboard.invitation_link"
                readonly
                class="min-w-0"
              />
              <Button
                type="button"
                variant="outline"
                size="sm"
                class="shrink-0"
                @click="copyToClipboard(dashboard.invitation_link)"
              >
                <Copy class="mr-2 h-4 w-4" />
                复制
              </Button>
            </div>
          </div>
        </div>
      </Card>

      <div class="grid grid-cols-1 gap-4 md:grid-cols-2">
        <Card class="p-5">
          <p class="text-xs text-muted-foreground">
            待发返利
          </p>
          <p class="mt-2 text-xl font-semibold">
            {{ formatUsd(dashboard.summary.pending_reward_usd) }}
          </p>
        </Card>
        <Card class="p-5">
          <p class="text-xs text-muted-foreground">
            已冲回返利
          </p>
          <p class="mt-2 text-xl font-semibold">
            {{ formatUsd(dashboard.summary.reversed_reward_usd) }}
          </p>
        </Card>
      </div>
    </template>

    <div
      v-else
      class="rounded-lg border border-border bg-card p-6 text-sm text-muted-foreground"
    >
      邀请数据暂不可用
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { Copy } from 'lucide-vue-next'
import { referralApi, type ReferralDashboardResponse } from '@/api/referrals'
import { Button, Card, Input, Label } from '@/components/ui'
import { useClipboard } from '@/composables/useClipboard'
import { useToast } from '@/composables/useToast'

const dashboard = ref<ReferralDashboardResponse | null>(null)
const loading = ref(false)
const { copyToClipboard } = useClipboard()
const { error: showError } = useToast()

function formatUsd(value: number): string {
  return `$${Number(value || 0).toFixed(2)}`
}

async function loadReferralDashboard() {
  loading.value = true
  try {
    dashboard.value = await referralApi.getMyReferral()
  } catch {
    dashboard.value = null
    showError('加载邀请数据失败')
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  void loadReferralDashboard()
})
</script>
