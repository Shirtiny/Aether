<template>
  <PageContainer>
    <PageHeader
      title="支付配置"
      description="配置易支付商户、回调地址、汇率和可用通道"
    >
      <template #actions>
        <div class="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            :disabled="testing || loading"
            @click="testGateway"
          >
            <PlugZap class="mr-2 h-4 w-4" />
            {{ testing ? '测试中...' : '测试配置' }}
          </Button>
          <Button
            size="sm"
            :disabled="saving || loading"
            @click="saveConfig"
          >
            <Save class="mr-2 h-4 w-4" />
            {{ saving ? '保存中...' : '保存' }}
          </Button>
        </div>
      </template>
    </PageHeader>

    <div class="mt-6 space-y-6">
      <div
        v-if="loading"
        class="py-16"
      >
        <LoadingState message="正在加载支付配置..." />
      </div>

      <template v-else>
        <div class="grid grid-cols-1 gap-4 lg:grid-cols-3">
          <Card class="p-5">
            <div class="text-xs uppercase tracking-wider text-muted-foreground">
              网关状态
            </div>
            <div class="mt-3 flex items-center gap-3">
              <Badge :variant="form.enabled ? 'success' : 'secondary'">
                {{ form.enabled ? '已启用' : '未启用' }}
              </Badge>
              <Switch v-model="form.enabled" />
            </div>
          </Card>
          <Card class="p-5">
            <div class="text-xs uppercase tracking-wider text-muted-foreground">
              商户密钥
            </div>
            <div class="mt-3">
              <Badge :variant="hasSecret ? 'success' : 'warning'">
                {{ hasSecret ? '已保存' : '未设置' }}
              </Badge>
            </div>
          </Card>
          <Card class="p-5">
            <div class="text-xs uppercase tracking-wider text-muted-foreground">
              汇率
            </div>
            <div class="mt-2 text-2xl font-semibold tabular-nums">
              1 USD = {{ Number(form.usd_exchange_rate || 0).toFixed(4) }} {{ form.pay_currency }}
            </div>
          </Card>
        </div>

        <CardSection
          title="易支付商户"
          description="密钥留空会保留原密钥；回调地址留空时后端会使用当前 API 访问地址，生产环境建议显式填写公网根地址"
        >
          <div class="grid grid-cols-1 gap-5 md:grid-cols-2">
            <div class="space-y-1.5">
              <Label for="epay-endpoint">易支付接口地址</Label>
              <Input
                id="epay-endpoint"
                v-model="form.endpoint_url"
                placeholder="https://pay.example.com/submit.php"
              />
            </div>

            <div class="space-y-1.5">
              <Label for="epay-callback-base">回调站点根地址</Label>
              <Input
                id="epay-callback-base"
                v-model="form.callback_base_url"
                :placeholder="defaultCallbackBaseUrl || 'https://aether.example.com'"
              />
              <p class="text-xs text-muted-foreground">
                留空保存为空配置；下单时默认使用当前 API 地址或 AETHER_PUBLIC_BASE_URL。
              </p>
            </div>

            <div class="space-y-1.5">
              <Label for="epay-merchant-id">商户 ID</Label>
              <Input
                id="epay-merchant-id"
                v-model="form.merchant_id"
                placeholder="1000"
                autocomplete="off"
              />
            </div>

            <div class="space-y-1.5">
              <Label for="epay-merchant-key">
                商户密钥
                <span class="text-xs font-normal text-muted-foreground">
                  {{ hasSecret ? '（留空保持不变）' : '' }}
                </span>
              </Label>
              <Input
                id="epay-merchant-key"
                v-model="form.merchant_key"
                masked
                :placeholder="hasSecret ? '已设置，输入新密钥后覆盖' : '请输入商户密钥'"
              />
            </div>
          </div>
        </CardSection>

        <CardSection
          title="计费参数"
          description="用户充值按美元金额下单，易支付按这里的币种和汇率收款"
        >
          <div class="grid grid-cols-1 gap-5 md:grid-cols-3">
            <div class="space-y-1.5">
              <Label for="epay-currency">支付币种</Label>
              <Input
                id="epay-currency"
                v-model="form.pay_currency"
                maxlength="16"
                placeholder="CNY"
              />
            </div>
            <div class="space-y-1.5">
              <Label for="epay-rate">USD 汇率</Label>
              <Input
                id="epay-rate"
                v-model.number="form.usd_exchange_rate"
                type="number"
                min="0.0001"
                step="0.0001"
              />
            </div>
            <div class="space-y-1.5">
              <Label for="epay-min">最低充值金额 (USD)</Label>
              <Input
                id="epay-min"
                v-model.number="form.min_recharge_usd"
                type="number"
                min="0.01"
                step="0.01"
              />
            </div>
          </div>
        </CardSection>

        <CardSection
          title="支付通道"
          description="通道值会传给易支付 type 字段"
        >
          <template #actions>
            <Button
              variant="outline"
              size="sm"
              @click="addChannel"
            >
              <Plus class="mr-2 h-4 w-4" />
              添加通道
            </Button>
          </template>

          <div class="space-y-3">
            <div
              v-for="(channel, index) in form.channels"
              :key="index"
              class="grid grid-cols-1 gap-3 rounded-lg border border-border/60 bg-muted/20 p-3 md:grid-cols-[1fr_1fr_auto]"
            >
              <div class="space-y-1.5">
                <Label :for="`epay-channel-${index}`">通道值</Label>
                <Input
                  :id="`epay-channel-${index}`"
                  v-model="channel.channel"
                  placeholder="alipay"
                />
              </div>
              <div class="space-y-1.5">
                <Label :for="`epay-channel-name-${index}`">显示名称</Label>
                <Input
                  :id="`epay-channel-name-${index}`"
                  v-model="channel.display_name"
                  placeholder="支付宝"
                />
              </div>
              <div class="flex items-end">
                <Button
                  variant="ghost"
                  size="icon"
                  title="移除通道"
                  :disabled="form.channels.length <= 1"
                  @click="removeChannel(index)"
                >
                  <Trash2 class="h-4 w-4" />
                </Button>
              </div>
            </div>
          </div>
        </CardSection>

        <p
          v-if="updatedAtText"
          class="text-xs text-muted-foreground"
        >
          最后更新：{{ updatedAtText }}
        </p>
      </template>
    </div>
  </PageContainer>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { PlugZap, Plus, Save, Trash2 } from 'lucide-vue-next'
import { epayGatewayApi, type EpayChannelConfig } from '@/api/billing'
import {
  Badge,
  Button,
  Card,
  Input,
  Label,
  Switch,
} from '@/components/ui'
import { LoadingState } from '@/components/common'
import { CardSection, PageContainer, PageHeader } from '@/components/layout'
import { useToast } from '@/composables/useToast'
import { parseApiError } from '@/utils/errorParser'
import { log } from '@/utils/logger'

const { success, error: showError } = useToast()

const loading = ref(true)
const saving = ref(false)
const testing = ref(false)
const hasSecret = ref(false)
const updatedAt = ref<number | null>(null)

const form = reactive({
  enabled: false,
  endpoint_url: '',
  callback_base_url: '',
  merchant_id: '',
  merchant_key: '',
  pay_currency: 'CNY',
  usd_exchange_rate: 7.2,
  min_recharge_usd: 1,
  channels: [
    { channel: 'alipay', display_name: '支付宝' },
    { channel: 'wxpay', display_name: '微信支付' },
  ] as EpayChannelConfig[],
})

const updatedAtText = computed(() => {
  if (!updatedAt.value) return ''
  return new Date(updatedAt.value * 1000).toLocaleString('zh-CN')
})

const defaultCallbackBaseUrl = computed(() => {
  if (typeof window === 'undefined') return ''
  return window.location.origin
})

onMounted(() => {
  void loadConfig()
})

async function loadConfig() {
  loading.value = true
  try {
    const config = await epayGatewayApi.get()
    form.enabled = config.enabled
    form.endpoint_url = config.endpoint_url || ''
    form.callback_base_url = config.callback_base_url || ''
    form.merchant_id = config.merchant_id || ''
    form.merchant_key = ''
    form.pay_currency = config.pay_currency || 'CNY'
    form.usd_exchange_rate = Number(config.usd_exchange_rate || 7.2)
    form.min_recharge_usd = Number(config.min_recharge_usd || 1)
    form.channels = config.channels?.length
      ? config.channels.map((item) => ({ ...item }))
      : [
          { channel: 'alipay', display_name: '支付宝' },
          { channel: 'wxpay', display_name: '微信支付' },
        ]
    hasSecret.value = config.has_secret
    updatedAt.value = config.updated_at ?? null
  } catch (err) {
    log.error('加载易支付配置失败:', err)
    showError(parseApiError(err, '加载易支付配置失败'))
  } finally {
    loading.value = false
  }
}

function normalizeChannels(): EpayChannelConfig[] {
  return form.channels
    .map((item) => ({
      channel: item.channel.trim(),
      display_name: item.display_name.trim(),
    }))
    .filter((item) => item.channel && item.display_name)
}

function validateForm(): string | null {
  if (!form.endpoint_url.trim()) return '请输入易支付接口地址'
  if (!form.merchant_id.trim()) return '请输入商户 ID'
  if (!hasSecret.value && !form.merchant_key.trim()) return '首次配置需要填写商户密钥'
  if (!form.pay_currency.trim()) return '请输入支付币种'
  if (!Number.isFinite(Number(form.usd_exchange_rate)) || Number(form.usd_exchange_rate) <= 0) {
    return 'USD 汇率必须大于 0'
  }
  if (!Number.isFinite(Number(form.min_recharge_usd)) || Number(form.min_recharge_usd) <= 0) {
    return '最低充值金额必须大于 0'
  }
  if (normalizeChannels().length === 0) return '至少需要一个支付通道'
  return null
}

async function saveConfig() {
  const validationError = validateForm()
  if (validationError) {
    showError(validationError)
    return
  }

  saving.value = true
  try {
    const callbackBaseUrl = form.callback_base_url.trim()
    const payload = {
      enabled: form.enabled,
      endpoint_url: form.endpoint_url.trim(),
      callback_base_url: callbackBaseUrl || null,
      merchant_id: form.merchant_id.trim(),
      pay_currency: form.pay_currency.trim().toUpperCase(),
      usd_exchange_rate: Number(form.usd_exchange_rate),
      min_recharge_usd: Number(form.min_recharge_usd),
      channels: normalizeChannels(),
      ...(form.merchant_key.trim() ? { merchant_key: form.merchant_key.trim() } : {}),
    }
    const config = await epayGatewayApi.update(payload)
    hasSecret.value = config.has_secret
    updatedAt.value = config.updated_at ?? null
    form.callback_base_url = config.callback_base_url || ''
    form.merchant_key = ''
    success('支付配置已保存')
  } catch (err) {
    log.error('保存易支付配置失败:', err)
    showError(parseApiError(err, '保存易支付配置失败'))
  } finally {
    saving.value = false
  }
}

async function testGateway() {
  testing.value = true
  try {
    await epayGatewayApi.test()
    success('易支付配置可用')
  } catch (err) {
    log.error('测试易支付配置失败:', err)
    showError(parseApiError(err, '测试易支付配置失败'))
  } finally {
    testing.value = false
  }
}

function addChannel() {
  form.channels.push({ channel: '', display_name: '' })
}

function removeChannel(index: number) {
  if (form.channels.length <= 1) return
  form.channels.splice(index, 1)
}
</script>
