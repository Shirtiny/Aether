<template>
  <div class="space-y-6 pb-8">
    <!-- 统计卡片 -->
    <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
      <Card>
        <div class="p-6">
          <div class="flex items-center justify-between">
            <div>
              <p class="text-sm font-medium text-muted-foreground">
                黑名单 IP 数量
              </p>
              <h3 class="text-2xl font-bold mt-2">
                {{ blacklistData.total || blacklistStats.total || 0 }}
              </h3>
            </div>
            <div class="h-12 w-12 rounded-full bg-destructive/10 flex items-center justify-center">
              <ShieldX class="h-6 w-6 text-destructive" />
            </div>
          </div>
        </div>
      </Card>

      <Card>
        <div class="p-6">
          <div class="flex items-center justify-between">
            <div>
              <p class="text-sm font-medium text-muted-foreground">
                白名单 IP 数量
              </p>
              <h3 class="text-2xl font-bold mt-2">
                {{ whitelistData.total || 0 }}
              </h3>
            </div>
            <div class="h-12 w-12 rounded-full bg-primary/10 flex items-center justify-center">
              <ShieldCheck class="h-6 w-6 text-primary" />
            </div>
          </div>
        </div>
      </Card>
    </div>

    <!-- IP 黑名单管理 -->
    <Card
      variant="default"
      class="overflow-hidden"
    >
      <div class="px-4 sm:px-6 py-3 sm:py-3.5 border-b border-border/60">
        <div class="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3 sm:gap-4">
          <div class="shrink-0">
            <h3 class="text-sm sm:text-base font-semibold">
              IP 黑名单
            </h3>
            <p class="text-xs text-muted-foreground mt-0.5">
              管理被禁止访问的 IP 地址
            </p>
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <Button
              variant="ghost"
              size="icon"
              class="h-8 w-8"
              title="添加黑名单"
              @click="showAddBlacklistDialog = true"
            >
              <Plus class="w-3.5 h-3.5" />
            </Button>
            <RefreshButton
              :loading="loadingBlacklist"
              @click="loadBlacklist"
            />
          </div>
        </div>
      </div>

      <div
        v-if="loadingBlacklist"
        class="flex items-center justify-center py-12"
      >
        <div class="animate-spin rounded-full h-8 w-8 border-b-2 border-primary" />
      </div>

      <div
        v-else
        class="p-6"
      >
        <div
          v-if="!blacklistStats.available"
          class="mb-4 rounded-lg border border-destructive/20 bg-destructive/5 px-4 py-3 text-sm text-muted-foreground"
        >
          <div class="flex items-start gap-3">
            <AlertCircle class="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
            <div>
              <p class="font-medium text-foreground">
                黑名单状态不可用，列表可能不是最新
              </p>
              <p class="mt-1 text-xs">
                {{ blacklistStats.error }}
              </p>
            </div>
          </div>
        </div>

        <div
          v-if="blacklistListError"
          class="text-center py-8 text-muted-foreground"
        >
          <AlertCircle class="w-12 h-12 mx-auto mb-2 opacity-50" />
          <p>无法获取黑名单列表</p>
          <p class="text-xs mt-1">
            {{ blacklistListError }}
          </p>
        </div>
        <div
          v-else-if="blacklistData.items.length === 0"
          class="text-center py-8 text-muted-foreground"
        >
          <ShieldX class="w-12 h-12 mx-auto mb-2 opacity-50" />
          <p>暂无黑名单 IP</p>
        </div>
        <div
          v-else
          class="space-y-4"
        >
          <div class="text-sm text-muted-foreground">
            当前共有 <span class="font-semibold text-foreground">{{ blacklistData.total || blacklistStats.total || 0 }}</span> 个 IP 在黑名单中
          </div>

          <Table class="hidden sm:table">
            <TableHeader>
              <TableRow>
                <TableHead>IP 地址</TableHead>
                <TableHead>原因</TableHead>
                <TableHead>剩余时长</TableHead>
                <TableHead class="text-right">
                  操作
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow
                v-for="entry in blacklistData.items"
                :key="entry.ip_address"
              >
                <TableCell class="font-mono text-sm">
                  {{ entry.ip_address }}
                </TableCell>
                <TableCell class="max-w-[28rem] truncate">
                  {{ entry.reason }}
                </TableCell>
                <TableCell class="whitespace-nowrap">
                  {{ formatBlacklistTTL(entry.ttl_seconds) }}
                </TableCell>
                <TableCell class="text-right">
                  <Button
                    variant="ghost"
                    size="sm"
                    class="h-8 px-3"
                    @click="handleRemoveFromBlacklist(entry.ip_address)"
                  >
                    <Trash2 class="w-4 h-4 mr-1.5" />
                    移除
                  </Button>
                </TableCell>
              </TableRow>
            </TableBody>
          </Table>

          <div class="sm:hidden divide-y divide-border/40">
            <div
              v-for="entry in blacklistData.items"
              :key="entry.ip_address"
              class="p-4 flex items-start justify-between gap-3"
            >
              <div class="min-w-0 space-y-1">
                <div class="font-mono text-sm break-all">
                  {{ entry.ip_address }}
                </div>
                <div class="text-xs text-muted-foreground leading-5 break-words">
                  {{ entry.reason }}
                </div>
                <div class="text-xs text-muted-foreground">
                  {{ formatBlacklistTTL(entry.ttl_seconds) }}
                </div>
              </div>
              <Button
                variant="ghost"
                size="sm"
                class="h-8 px-3 shrink-0"
                @click="handleRemoveFromBlacklist(entry.ip_address)"
              >
                <Trash2 class="w-4 h-4" />
              </Button>
            </div>
          </div>
        </div>
      </div>
    </Card>

    <!-- IP 白名单管理 -->
    <Card
      variant="default"
      class="overflow-hidden"
    >
      <div class="px-4 sm:px-6 py-3 sm:py-3.5 border-b border-border/60">
        <div class="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3 sm:gap-4">
          <div class="shrink-0">
            <h3 class="text-sm sm:text-base font-semibold">
              IP 白名单
            </h3>
            <p class="text-xs text-muted-foreground mt-0.5">
              管理可信任的 IP 地址（支持 CIDR 格式）
            </p>
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <Button
              variant="ghost"
              size="icon"
              class="h-8 w-8"
              title="添加白名单"
              @click="showAddWhitelistDialog = true"
            >
              <Plus class="w-3.5 h-3.5" />
            </Button>
            <RefreshButton
              :loading="loadingWhitelist"
              @click="loadWhitelist"
            />
          </div>
        </div>
      </div>

      <div
        v-if="loadingWhitelist"
        class="flex items-center justify-center py-12"
      >
        <div class="animate-spin rounded-full h-8 w-8 border-b-2 border-primary" />
      </div>

      <div
        v-else-if="whitelistData.whitelist.length === 0"
        class="text-center py-12 text-muted-foreground"
      >
        <ShieldCheck class="w-12 h-12 mx-auto mb-2 opacity-50" />
        <p>暂无白名单 IP</p>
      </div>

      <div v-else>
        <Table class="hidden sm:table">
          <TableHeader>
            <TableRow>
              <TableHead>IP 地址 / CIDR</TableHead>
              <TableHead class="text-right">
                操作
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow
              v-for="ip in whitelistData.whitelist"
              :key="ip"
            >
              <TableCell class="font-mono text-sm">
                {{ ip }}
              </TableCell>
              <TableCell class="text-right">
                <Button
                  variant="ghost"
                  size="sm"
                  class="h-8 px-3"
                  @click="handleRemoveFromWhitelist(ip)"
                >
                  <Trash2 class="w-4 h-4 mr-1.5" />
                  移除
                </Button>
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>

        <!-- 移动端卡片列表 -->
        <div class="sm:hidden divide-y divide-border/40">
          <div
            v-for="ip in whitelistData.whitelist"
            :key="ip"
            class="p-4 flex items-center justify-between gap-3"
          >
            <span class="font-mono text-sm truncate">{{ ip }}</span>
            <Button
              variant="ghost"
              size="sm"
              class="h-8 px-3 shrink-0"
              @click="handleRemoveFromWhitelist(ip)"
            >
              <Trash2 class="w-4 h-4" />
            </Button>
          </div>
        </div>
      </div>
    </Card>

    <!-- 添加黑名单对话框 -->
    <Dialog v-model:open="showAddBlacklistDialog">
      <DialogContent class="sm:max-w-md !p-0 overflow-hidden">
        <DialogHeader class="!px-4 !py-3">
          <DialogTitle class="!text-base">
            添加 IP 到黑名单
          </DialogTitle>
          <DialogDescription class="!mt-1">
            被加入黑名单的 IP 将无法访问任何接口
          </DialogDescription>
        </DialogHeader>
        <div class="space-y-3 px-4 py-4">
          <div class="space-y-1.5">
            <label class="text-sm font-medium">IP 地址</label>
            <Input
              v-model="blacklistForm.ip_address"
              placeholder="例如: 192.168.1.100"
              class="font-mono"
            />
          </div>
          <div class="space-y-1.5">
            <label class="text-sm font-medium">原因</label>
            <Input
              v-model="blacklistForm.reason"
              placeholder="加入黑名单的原因"
              maxlength="200"
            />
          </div>
          <div class="space-y-1.5">
            <label class="text-sm font-medium">过期时间（可选）</label>
            <Input
              v-model.number="blacklistForm.ttl"
              type="number"
              placeholder="留空表示永久，单位：秒"
              min="1"
            />
            <p class="text-xs text-muted-foreground">
              留空表示永久封禁，或输入秒数（如 3600 表示 1 小时）
            </p>
          </div>
        </div>
        <DialogFooter class="!px-4 !py-3">
          <Button
            variant="ghost"
            @click="showAddBlacklistDialog = false"
          >
            取消
          </Button>
          <Button
            variant="destructive"
            :disabled="!blacklistForm.ip_address || !blacklistForm.reason"
            @click="handleAddToBlacklist"
          >
            添加到黑名单
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>

    <!-- 添加白名单对话框 -->
    <Dialog v-model:open="showAddWhitelistDialog">
      <DialogContent class="sm:max-w-md !p-0 overflow-hidden">
        <DialogHeader class="!px-4 !py-3">
          <DialogTitle class="!text-base">
            添加 IP 到白名单
          </DialogTitle>
          <DialogDescription class="!mt-1">
            白名单中的 IP 不受速率限制
          </DialogDescription>
        </DialogHeader>
        <div class="space-y-3 px-4 py-4">
          <div class="space-y-1.5">
            <label class="text-sm font-medium">IP 地址或 CIDR</label>
            <Input
              v-model="whitelistForm.ip_address"
              placeholder="例如: 192.168.1.0/24 或 192.168.1.100"
              class="font-mono"
            />
            <p class="text-xs text-muted-foreground leading-5">
              支持单个 IP 或 CIDR 网段格式
            </p>
          </div>
        </div>
        <DialogFooter class="!px-4 !py-3">
          <Button
            variant="ghost"
            @click="showAddWhitelistDialog = false"
          >
            取消
          </Button>
          <Button
            :disabled="!whitelistForm.ip_address"
            @click="handleAddToWhitelist"
          >
            添加到白名单
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { Plus, Trash2, ShieldX, ShieldCheck, AlertCircle } from 'lucide-vue-next'
import {
  Card,
  Button,
  Input,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  RefreshButton
} from '@/components/ui'
import { blacklistApi, whitelistApi, type BlacklistStats, type BlacklistResponse, type WhitelistResponse } from '@/api/security'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { parseApiError } from '@/utils/errorParser'

const { success, error } = useToast()
const { confirmDanger } = useConfirm()

// 黑名单状态
const loadingBlacklist = ref(false)
const blacklistStats = ref<BlacklistStats>({
  available: false,
  total: 0
})
const blacklistData = ref<BlacklistResponse>({
  items: [],
  total: 0
})
const blacklistListError = ref<string | null>(null)
const showAddBlacklistDialog = ref(false)
const blacklistForm = ref({
  ip_address: '',
  reason: '',
  ttl: undefined as number | undefined
})

// 白名单状态
const loadingWhitelist = ref(false)
const whitelistData = ref<WhitelistResponse>({
  whitelist: [],
  total: 0
})
const showAddWhitelistDialog = ref(false)
const whitelistForm = ref({
  ip_address: ''
})

/**
 * 加载黑名单统计和列表
 */
async function loadBlacklist() {
  loadingBlacklist.value = true
  blacklistListError.value = null
  try {
    const [statsResult, listResult] = await Promise.allSettled([
      blacklistApi.getStats(),
      blacklistApi.getList()
    ])

    if (statsResult.status === 'fulfilled') {
      blacklistStats.value = statsResult.value
    } else {
      blacklistStats.value = {
        available: false,
        total: 0,
        error: parseApiError(statsResult.reason, '无法获取黑名单统计')
      }
    }

    if (listResult.status === 'fulfilled') {
      blacklistData.value = listResult.value
    } else {
      blacklistData.value = {
        items: [],
        total: 0
      }
      blacklistListError.value = parseApiError(listResult.reason, '无法获取黑名单列表')
    }
  } catch (err: unknown) {
    error(parseApiError(err, '无法获取黑名单数据'))
  } finally {
    loadingBlacklist.value = false
  }
}

/**
 * 加载白名单列表
 */
async function loadWhitelist() {
  loadingWhitelist.value = true
  try {
    whitelistData.value = await whitelistApi.getList()
  } catch (err: unknown) {
    error(parseApiError(err, '无法获取白名单列表'))
  } finally {
    loadingWhitelist.value = false
  }
}

/**
 * 添加 IP 到黑名单
 */
async function handleAddToBlacklist() {
  try {
    await blacklistApi.add({
      ip_address: blacklistForm.value.ip_address,
      reason: blacklistForm.value.reason,
      ttl: blacklistForm.value.ttl
    })

    success(`IP ${blacklistForm.value.ip_address} 已加入黑名单`)

    showAddBlacklistDialog.value = false
    blacklistForm.value = { ip_address: '', reason: '', ttl: undefined }
    await loadBlacklist()
  } catch (err: unknown) {
    error(parseApiError(err, '无法添加 IP 到黑名单'))
  }
}

/**
 * 添加 IP 到白名单
 */
async function handleAddToWhitelist() {
  try {
    await whitelistApi.add({
      ip_address: whitelistForm.value.ip_address
    })

    success(`IP ${whitelistForm.value.ip_address} 已加入白名单`)

    showAddWhitelistDialog.value = false
    whitelistForm.value = { ip_address: '' }
    await loadWhitelist()
  } catch (err: unknown) {
    error(parseApiError(err, '无法添加 IP 到白名单'))
  }
}

/**
 * 从白名单移除 IP
 */
async function handleRemoveFromWhitelist(ip: string) {
  const confirmed = await confirmDanger(
    `确定要从白名单移除 ${ip} 吗？\n\n此操作无法撤销。`,
    '移除白名单'
  )

  if (!confirmed) return

  try {
    await whitelistApi.remove(ip)

    success(`IP ${ip} 已从白名单移除`)

    await loadWhitelist()
  } catch (err: unknown) {
    error(parseApiError(err, '无法从白名单移除 IP'))
  }
}

/**
 * 从黑名单移除 IP
 */
async function handleRemoveFromBlacklist(ip: string) {
  const confirmed = await confirmDanger(
    `确定要从黑名单移除 ${ip} 吗？\n\n此操作无法撤销。`,
    '移除黑名单'
  )

  if (!confirmed) return

  try {
    await blacklistApi.remove(ip)

    success(`IP ${ip} 已从黑名单移除`)

    await loadBlacklist()
  } catch (err: unknown) {
    error(parseApiError(err, '无法从黑名单移除 IP'))
  }
}

function formatBlacklistTTL(ttlSeconds?: number | null) {
  if (ttlSeconds == null) return '永久'
  if (ttlSeconds <= 0) return '即将过期'

  const days = Math.floor(ttlSeconds / 86400)
  if (days > 0) return `${days} 天`

  const hours = Math.floor(ttlSeconds / 3600)
  if (hours > 0) return `${hours} 小时`

  const minutes = Math.floor(ttlSeconds / 60)
  if (minutes > 0) return `${minutes} 分钟`

  return `${ttlSeconds} 秒`
}

onMounted(() => {
  loadBlacklist()
  loadWhitelist()
})
</script>
