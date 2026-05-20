<template>
  <Dialog
    :model-value="isOpen"
    :title="isEditMode ? '编辑密钥' : '添加密钥'"
    :description="isEditMode ? '修改 API 密钥配置' : '为提供商添加新的 API 密钥'"
    :icon="isEditMode ? SquarePen : Key"
    size="xl"
    @update:model-value="handleDialogUpdate"
  >
    <form
      class="space-y-3"
      autocomplete="off"
      @submit.prevent="handleSave"
    >
      <!-- 基本信息 -->
      <div class="grid grid-cols-2 gap-3">
        <div>
          <Label :for="keyNameInputId">密钥名称 *</Label>
          <Input
            :id="keyNameInputId"
            v-model="form.name"
            :name="keyNameFieldName"
            required
            placeholder="例如：主 Key、备用 Key 1"
            maxlength="100"
            autocomplete="off"
            autocapitalize="none"
            autocorrect="off"
            spellcheck="false"
            data-form-type="other"
            data-lpignore="true"
            data-1p-ignore="true"
          />
        </div>
        <div v-if="showAuthTypeSelector">
          <Label :for="authTypeSelectId">认证类型</Label>
          <Select v-model="form.auth_type">
            <SelectTrigger :id="authTypeSelectId">
              <SelectValue placeholder="选择认证类型" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem
                v-for="option in authTypeOptions"
                :key="option.value"
                :value="option.value"
              >
                {{ option.label }}
              </SelectItem>
            </SelectContent>
          </Select>
        </div>
        <div :class="showAuthTypeSelector ? 'col-span-2' : undefined">
          <Label :for="apiKeyInputId">
            {{ authSecretLabel }}
            {{ authSecretRequiredMark }}
          </Label>
          <template v-if="form.auth_type === 'service_account'">
            <JsonImportInput
              v-model="form.auth_config_text"
              :disabled="saving"
              :reset-key="formNonce"
              accept=".json,.txt,application/json,text/plain"
              :multiple="false"
              drop-title="拖入 Service Account JSON 或点击选择"
              drop-hint="支持 .json / .txt，单文件导入"
              :manual-placeholder="editingKey ? '留空表示不修改，或粘贴完整的 Service Account JSON' : '粘贴完整的 Service Account JSON'"
              :manual-description="serviceAccountDescription"
              textarea-class="min-h-[160px] font-mono text-xs break-all !rounded-xl"
              @error="handleServiceAccountImportError"
            />
          </template>
          <template v-else>
            <div class="flex gap-2">
              <div class="min-w-0 flex-1">
                <Input
                  :id="apiKeyInputId"
                  v-model="form.api_key"
                  :name="apiKeyFieldName"
                  masked
                  :required="false"
                  :placeholder="editingKey ? editingKey.api_key_masked : authSecretPlaceholder"
                />
              </div>
              <Button
                type="button"
                variant="outline"
                class="h-11 shrink-0 gap-1.5 px-3"
                :disabled="!canQueryBalance"
                title="查询当前密钥的上游余额"
                @click="handleQueryBalance"
              >
                <Loader2
                  v-if="balanceLoading"
                  class="h-3.5 w-3.5 animate-spin"
                />
                <WalletCards
                  v-else
                  class="h-3.5 w-3.5"
                />
                <span>查询余额</span>
              </Button>
            </div>
          </template>
          <p
            v-if="editingKey && isRawSecretAuthType(form.auth_type)"
            class="text-xs text-muted-foreground mt-1"
          >
            留空表示不修改
          </p>
        </div>
      </div>

      <!-- 备注 -->
      <div>
        <Label for="note">备注</Label>
        <Input
          id="note"
          v-model="form.note"
          placeholder="可选的备注信息"
        />
      </div>

      <!-- API 格式 & 认证方式 -->
      <div v-if="visibleApiFormats.length > 0">
        <div class="flex items-center gap-1 mb-1.5">
          <Label>支持的 API 格式 *</Label>
          <span
            class="relative inline-flex"
            @mouseenter="apiFormatHelpHovered = true"
            @mouseleave="apiFormatHelpHovered = false"
          >
            <button
              type="button"
              class="inline-flex items-center justify-center rounded-sm p-0.5 text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              title="API 格式说明"
              aria-label="API 格式说明"
              :aria-expanded="apiFormatHelpVisible"
              @click.stop="toggleApiFormatHelp"
              @focus="apiFormatHelpHovered = true"
              @blur="apiFormatHelpHovered = false"
              @keydown.escape.stop.prevent="apiFormatHelpOpen = false"
            >
              <CircleHelp class="h-3.5 w-3.5" />
            </button>
            <span
              v-if="apiFormatHelpVisible"
              role="tooltip"
              class="absolute left-0 top-full z-[100] mt-1 w-80 rounded-md border bg-popover px-3 py-2 text-xs font-normal normal-case leading-5 tracking-normal text-popover-foreground shadow-md"
            >
              选择此密钥支持的 API 格式及对应认证方式。OpenAI 格式固定使用 Bearer Token；Claude / Gemini 格式可选 API Key 或 Bearer Token（如 Claude Code 应使用 Bearer Token）。
            </span>
          </span>
        </div>
        <div class="flex flex-col gap-1.5">
          <div
            v-for="format in visibleApiFormats"
            :key="format"
            class="flex items-center justify-between rounded-md border px-2 py-1.5 transition-colors cursor-pointer"
            :class="form.api_formats.includes(format)
              ? 'bg-primary/5 border-primary/30'
              : 'bg-muted/30 border-border hover:border-muted-foreground/30'"
            @click="toggleApiFormat(format)"
          >
            <div class="flex items-center gap-1.5 min-w-0">
              <span
                class="w-4 h-4 rounded border flex items-center justify-center text-xs shrink-0"
                :class="form.api_formats.includes(format)
                  ? 'bg-primary border-primary text-primary-foreground'
                  : 'border-muted-foreground/30'"
              >
                <span v-if="form.api_formats.includes(format)">✓</span>
              </span>
              <span
                class="text-sm"
                :class="form.api_formats.includes(format) ? 'text-primary' : 'text-muted-foreground'"
              >{{ formatApiFormat(format) }}</span>
            </div>
            <!-- 认证方式：已勾选且可覆盖时显示 radio -->
            <div class="flex items-center gap-3">
              <div
                v-if="canOverrideFormatAuth(format)"
                class="flex gap-2"
                @click.stop
              >
                <button
                  v-for="opt in authTypeOptions.filter(o => isRawSecretAuthType(o.value))"
                  :key="opt.value"
                  type="button"
                  class="flex items-center gap-1 text-[10px] leading-none transition-colors"
                  :class="getFormatAuthType(format) === opt.value ? 'text-primary' : 'text-muted-foreground hover:text-foreground'"
                  @click="setFormatAuthType(format, opt.value as RawSecretAuthType)"
                >
                  <span
                    class="w-2.5 h-2.5 rounded-full border flex items-center justify-center shrink-0"
                    :class="getFormatAuthType(format) === opt.value ? 'border-primary' : 'border-muted-foreground/40'"
                  >
                    <span
                      v-if="getFormatAuthType(format) === opt.value"
                      class="w-1 h-1 rounded-full bg-primary"
                    />
                  </span>
                  {{ opt.label }}
                </button>
              </div>
              <div
                v-if="canToggleAuthChannelMismatch(format)"
                class="flex items-center gap-1"
                title="允许客户端认证方式不一致时使用"
                @click.stop
              >
                <Switch
                  :model-value="isAuthChannelMismatchAllowed(format)"
                  class="scale-75"
                  @update:model-value="(value) => setAuthChannelMismatchAllowed(format, value)"
                />
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- 配置项 -->
      <div class="grid grid-cols-4 gap-3">
        <div>
          <Label
            for="internal_priority"
            class="text-xs"
          >优先级</Label>
          <Input
            id="internal_priority"
            v-model.number="form.internal_priority"
            type="number"
            min="0"
            class="h-8"
          />
          <p class="text-xs text-muted-foreground mt-0.5">
            越小越优先
          </p>
        </div>
        <div>
          <Label
            for="rpm_limit"
            class="text-xs"
          >RPM 限制</Label>
          <Input
            id="rpm_limit"
            :model-value="form.rpm_limit ?? ''"
            type="number"
            min="1"
            max="10000"
            placeholder="自适应"
            class="h-8"
            @update:model-value="(v) => form.rpm_limit = parseNullableNumberInput(v, { min: 1, max: 10000 })"
          />
          <p class="text-xs text-muted-foreground mt-0.5">
            留空自适应
          </p>
        </div>
        <div>
          <Label
            for="concurrent_limit"
            class="text-xs"
          >并发请求上限</Label>
          <Input
            id="concurrent_limit"
            :model-value="form.concurrent_limit ?? ''"
            type="number"
            min="0"
            placeholder="不限制"
            class="h-8"
            @update:model-value="(v) => form.concurrent_limit = parseNullableNumberInput(v, { min: 0 })"
          />
          <p class="text-xs text-muted-foreground mt-0.5">
            留空或 0 表示不限制
          </p>
        </div>
        <div>
          <Label
            for="cache_ttl_minutes"
            class="text-xs"
          >缓存 TTL</Label>
          <Input
            id="cache_ttl_minutes"
            :model-value="form.cache_ttl_minutes ?? ''"
            type="number"
            min="0"
            max="60"
            class="h-8"
            @update:model-value="(v) => form.cache_ttl_minutes = parseNumberInput(v, { min: 0, max: 60 }) ?? 5"
          />
          <p class="text-xs text-muted-foreground mt-0.5">
            分钟，0禁用
          </p>
        </div>
        <div>
          <Label
            for="max_probe_interval_minutes"
            class="text-xs"
          >熔断探测</Label>
          <Input
            id="max_probe_interval_minutes"
            :model-value="form.max_probe_interval_minutes ?? ''"
            type="number"
            min="0"
            max="32"
            placeholder="32"
            class="h-8"
            @update:model-value="(v) => form.max_probe_interval_minutes = parseNumberInput(v, { min: 0, max: 32 }) ?? 32"
          />
          <p class="text-xs text-muted-foreground mt-0.5">
            分钟，0-32
          </p>
        </div>
      </div>

      <!-- 自动获取模型 -->
      <div class="space-y-3 py-2 px-3 rounded-md border border-border/60 bg-muted/30">
        <div class="flex items-center justify-between">
          <div class="space-y-0.5">
            <Label class="text-sm font-medium">自动获取上游可用模型</Label>
            <p class="text-xs text-muted-foreground">
              定时更新上游模型, 配合模型映射使用
            </p>
            <p
              v-if="showAutoFetchWarning"
              class="text-xs text-amber-600 dark:text-amber-400"
            >
              {{ autoFetchWarningMessage }}
            </p>
          </div>
          <Switch v-model="form.auto_fetch_models" />
        </div>

        <!-- 模型过滤规则（仅当开启自动获取时显示） -->
        <div
          v-if="form.auto_fetch_models"
          class="space-y-2 pt-2 border-t border-border/40"
        >
          <div class="grid grid-cols-2 gap-3">
            <div>
              <Label class="text-xs">包含规则</Label>
              <Input
                v-model="form.model_include_patterns_text"
                placeholder="gpt-*, claude-*, 留空包含全部"
                class="h-8 text-sm"
              />
            </div>
            <div>
              <Label class="text-xs">排除规则</Label>
              <Input
                v-model="form.model_exclude_patterns_text"
                placeholder="*-preview, *-beta"
                class="h-8 text-sm"
              />
            </div>
          </div>
          <p class="text-xs text-muted-foreground">
            逗号分隔，支持 * ? 通配符，不区分大小写
          </p>
        </div>
      </div>
    </form>

    <template #footer>
      <Button
        variant="outline"
        @click="handleCancel"
      >
        取消
      </Button>
      <Button
        :disabled="saving || !canSave"
        @click="handleSave"
      >
        {{ saving ? (isEditMode ? '保存中...' : '添加中...') : (isEditMode ? '保存' : '添加') }}
      </Button>
    </template>
  </Dialog>

  <Dialog
    :model-value="balanceQueryDialogOpen"
    title="查询余额"
    description="选择上游余额接口模板"
    :icon="WalletCards"
    size="lg"
    :z-index="90"
    @update:model-value="balanceQueryDialogOpen = $event"
  >
    <div class="space-y-4">
      <div class="grid grid-cols-3 gap-1 rounded-md border border-border bg-muted/30 p-1">
        <button
          v-for="option in balanceQueryTemplateOptions"
          :key="option.value"
          type="button"
          class="min-w-0 rounded-sm px-2.5 py-2 text-sm font-medium transition-colors"
          :class="balanceQueryTemplate === option.value
            ? 'bg-background text-foreground shadow-sm'
            : 'text-muted-foreground hover:bg-background/70 hover:text-foreground'"
          @click="balanceQueryTemplate = option.value"
        >
          {{ option.label }}
        </button>
      </div>

      <div class="rounded-md border border-border bg-background px-3 py-3">
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <div class="text-sm font-medium text-foreground">
              {{ selectedBalanceQueryTemplate.title }}
            </div>
            <div class="mt-1 text-xs leading-5 text-muted-foreground">
              {{ selectedBalanceQueryTemplate.description }}
            </div>
          </div>
          <span class="shrink-0 rounded-sm bg-primary/10 px-2 py-1 text-xs text-primary">
            {{ selectedBalanceQueryTemplate.badge }}
          </span>
        </div>

        <div
          v-if="balanceQueryTemplate === 'new_api'"
          class="mt-3 space-y-3"
        >
          <div class="grid grid-cols-2 gap-2">
            <div>
              <Label
                for="balance-newapi-base-url"
                class="text-xs"
              >站点地址</Label>
              <Input
                id="balance-newapi-base-url"
                v-model="newApiBalanceQuery.base_url"
                placeholder="留空使用当前 Provider 的 base_url"
                class="h-8 text-sm"
              />
            </div>
            <div>
              <Label
                for="balance-newapi-user-id"
                class="text-xs"
              >用户 ID</Label>
              <Input
                id="balance-newapi-user-id"
                v-model="newApiBalanceQuery.user_id"
                placeholder="可选，对应 New-Api-User"
                class="h-8 text-sm"
              />
            </div>
          </div>
          <div>
            <Label
              for="balance-newapi-token"
              class="text-xs"
            >余额查询访问令牌</Label>
            <Input
              id="balance-newapi-token"
              v-model="newApiBalanceQuery.access_token"
              masked
              :placeholder="hasSavedBalanceSecret ? '留空使用已保存的余额查询凭据' : '来自 NewAPI 个人安全设置的访问令牌'"
              class="h-8 text-sm"
            />
            <p class="mt-1 text-xs leading-5 text-muted-foreground">
              单独用于余额查询；开启下方保存后会加密保存，不会替换当前模型 Key。
            </p>
          </div>
          <div class="grid grid-cols-2 gap-2 text-xs">
            <div class="rounded-sm border border-border/70 px-2.5 py-2">
              <div class="text-muted-foreground">
                接口
              </div>
              <div class="mt-1 font-mono text-foreground">
                GET /api/user/self
              </div>
            </div>
            <div class="rounded-sm border border-border/70 px-2.5 py-2">
              <div class="text-muted-foreground">
                额度换算
              </div>
              <div class="mt-1 font-mono text-foreground">
                quota / 500000
              </div>
            </div>
          </div>
        </div>

        <div
          v-else-if="balanceQueryTemplate === 'sub2api'"
          class="mt-3 space-y-3"
        >
          <div>
            <Label
              for="balance-sub2api-base-url"
              class="text-xs"
            >站点地址</Label>
            <Input
              id="balance-sub2api-base-url"
              v-model="sub2ApiBalanceQuery.base_url"
              placeholder="留空使用当前 Provider 的 base_url"
              class="h-8 text-sm"
            />
          </div>
          <div>
            <Label
              for="balance-sub2api-token"
              class="text-xs"
            >查询凭据</Label>
            <Input
              id="balance-sub2api-token"
              v-model="sub2ApiBalanceQuery.token"
              masked
              placeholder="留空使用外层 API 密钥"
              class="h-8 text-sm"
            />
            <p class="mt-1 text-xs leading-5 text-muted-foreground">
              使用 Sub2API 的 API Key 请求 /v1/usage；通常留空即可使用当前 Key 查询。
            </p>
          </div>
          <div class="text-xs">
            <div class="rounded-sm border border-border/70 px-2.5 py-2">
              <div class="text-muted-foreground">
                接口
              </div>
              <div class="mt-1 font-mono text-foreground">
                GET /v1/usage
              </div>
            </div>
          </div>
        </div>

        <div
          v-else
          class="mt-3 space-y-3"
        >
          <div>
            <Label
              for="balance-custom-base-url"
              class="text-xs"
            >站点地址</Label>
            <Input
              id="balance-custom-base-url"
              v-model="customBalanceQuery.base_url"
              placeholder="留空使用当前 Provider 的 base_url"
              class="h-8 text-sm"
            />
          </div>
          <div>
            <Label
              for="balance-custom-api-key"
              class="text-xs"
            >认证凭据</Label>
            <Input
              id="balance-custom-api-key"
              v-model="customBalanceQuery.api_key"
              masked
              placeholder="留空使用外层 API 密钥或已保存密钥"
              class="h-8 text-sm"
            />
          </div>
          <div class="grid grid-cols-[1fr_auto] gap-2">
            <div>
              <Label
                for="balance-custom-endpoint"
                class="text-xs"
              >查询路径</Label>
              <Input
                id="balance-custom-endpoint"
                v-model="customBalanceQuery.endpoint"
                placeholder="/api/user/self"
                class="h-8 font-mono text-sm"
              />
            </div>
            <div>
              <Label class="text-xs">方法</Label>
              <div class="grid h-8 grid-cols-2 gap-1 rounded-md border border-border bg-muted/30 p-0.5">
                <button
                  v-for="method in balanceQueryMethods"
                  :key="method"
                  type="button"
                  class="rounded-sm px-2 text-xs font-medium transition-colors"
                  :class="customBalanceQuery.method === method
                    ? 'bg-background text-foreground shadow-sm'
                    : 'text-muted-foreground hover:text-foreground'"
                  @click="customBalanceQuery.method = method"
                >
                  {{ method }}
                </button>
              </div>
            </div>
          </div>
          <div class="grid grid-cols-2 gap-2">
            <div>
              <Label
                for="balance-custom-currency"
                class="text-xs"
              >货币单位</Label>
              <Input
                id="balance-custom-currency"
                v-model="customBalanceQuery.currency"
                placeholder="USD"
                class="h-8 text-sm"
              />
            </div>
            <div>
              <Label
                for="balance-custom-divisor"
                class="text-xs"
              >额度除数</Label>
              <Input
                id="balance-custom-divisor"
                v-model.number="customBalanceQuery.quota_divisor"
                type="number"
                min="1"
                class="h-8 text-sm"
              />
            </div>
          </div>
          <div class="grid grid-cols-3 gap-2">
            <div>
              <Label
                for="balance-custom-balance-path"
                class="text-xs"
              >余额字段</Label>
              <Input
                id="balance-custom-balance-path"
                v-model="customBalanceQuery.balance_path"
                placeholder="data.quota"
                class="h-8 font-mono text-sm"
              />
            </div>
            <div>
              <Label
                for="balance-custom-used-path"
                class="text-xs"
              >已用字段</Label>
              <Input
                id="balance-custom-used-path"
                v-model="customBalanceQuery.used_path"
                placeholder="data.used_quota"
                class="h-8 font-mono text-sm"
              />
            </div>
            <div>
              <Label
                for="balance-custom-granted-path"
                class="text-xs"
              >总额字段</Label>
              <Input
                id="balance-custom-granted-path"
                v-model="customBalanceQuery.granted_path"
                placeholder="可留空自动计算"
                class="h-8 font-mono text-sm"
              />
            </div>
          </div>
        </div>
      </div>

      <div class="flex items-center justify-between rounded-md border border-border bg-muted/20 px-3 py-2">
        <div>
          <div class="text-sm font-medium text-foreground">
            查询成功后保存到当前 Key
          </div>
          <div class="mt-0.5 text-xs text-muted-foreground">
            {{ balanceSaveDescription }}
          </div>
        </div>
        <Switch
          :model-value="saveBalanceResult"
          :disabled="!canSaveBalanceResultToKey"
          @update:model-value="setSaveBalanceResult"
        />
      </div>
      <div
        v-if="canConfigureBalanceSecretSave"
        class="flex items-center justify-between rounded-md border border-border bg-muted/20 px-3 py-2"
        :class="!saveBalanceResult ? 'opacity-60' : ''"
      >
        <div>
          <div class="text-sm font-medium text-foreground">
            保存余额查询凭据
          </div>
          <div class="mt-0.5 text-xs text-muted-foreground">
            {{ balanceSecretSaveDescription }}
            <span v-if="hasSavedBalanceSecretForSelectedQuery">已保存过，可留空继续沿用。</span>
          </div>
        </div>
        <Switch
          :model-value="saveBalanceSecret"
          :disabled="!saveBalanceResult"
          @update:model-value="saveBalanceSecret = $event"
        />
      </div>
      <div class="grid grid-cols-[1fr_auto] items-end gap-3 rounded-md border border-border bg-background px-3 py-2">
        <div>
          <Label
            for="balance-auto-refresh-interval"
            class="text-xs"
          >自动查询间隔</Label>
          <p class="mt-1 text-xs leading-5 text-muted-foreground">
            保存到当前 Key 后生效，后台页面打开时会自动静默刷新；填 0 表示关闭。
          </p>
        </div>
        <div class="w-32">
          <Input
            id="balance-auto-refresh-interval"
            :model-value="balanceAutoRefreshIntervalMinutes"
            type="number"
            min="0"
            max="10080"
            class="h-8 text-sm"
            :disabled="!saveBalanceResult"
            @update:model-value="setBalanceAutoRefreshInterval"
          />
          <div class="mt-1 text-[10px] text-muted-foreground">
            分钟
          </div>
        </div>
      </div>
    </div>

    <template #footer>
      <div class="flex w-full items-center justify-end gap-2">
        <Button
          variant="outline"
          @click="balanceQueryDialogOpen = false"
        >
          取消
        </Button>
        <Button
          :disabled="!canConfirmBalanceQuery"
          @click="confirmBalanceQuery"
        >
          {{ balanceLoading ? '查询中...' : '开始查询' }}
        </Button>
      </div>
    </template>
  </Dialog>

  <Dialog
    :model-value="balanceDialogOpen"
    title="余额查询结果"
    description="当前密钥的上游账户余额"
    :icon="WalletCards"
    size="sm"
    :z-index="90"
    @update:model-value="balanceDialogOpen = $event"
  >
    <div class="space-y-3">
      <div
        v-if="balanceLoading"
        class="flex items-center justify-center gap-2 rounded-md border border-border bg-muted/30 px-3 py-6 text-sm text-muted-foreground"
      >
        <Loader2 class="h-4 w-4 animate-spin" />
        查询中...
      </div>

      <div
        v-else-if="balanceError"
        class="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5 text-sm text-destructive"
      >
        {{ balanceError }}
      </div>

      <template v-else-if="balanceResult">
        <div
          v-if="balanceResult.saved_to_key"
          class="rounded-md border border-emerald-500/30 bg-emerald-500/10 px-3 py-2.5 text-sm text-emerald-700 dark:text-emerald-300"
        >
          查询结果已保存到当前 Key，列表会显示最新余额摘要。
        </div>

        <div
          v-else-if="balanceResultPendingSave"
          class="rounded-md border border-emerald-500/30 bg-emerald-500/10 px-3 py-2.5 text-sm text-emerald-700 dark:text-emerald-300"
        >
          查询成功。保存 Key 后会自动写入余额摘要、查询模板和自动查询间隔。
        </div>

        <div
          v-else-if="balanceResult.save_message"
          class="rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 text-sm text-foreground"
        >
          {{ balanceResult.save_message }}
        </div>

        <div
          v-if="balanceResult.status !== 'success'"
          class="rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 text-sm text-foreground"
        >
          {{ balanceResult.message || balanceStatusText(balanceResult.status) }}
        </div>

        <template v-else-if="balanceData">
          <div class="rounded-md border border-border bg-muted/30 px-3 py-3">
            <div class="text-xs text-muted-foreground">
              可用余额
            </div>
            <div class="mt-1 text-2xl font-semibold text-foreground">
              {{ formatBalanceAmount(balanceData.total_available, balanceData.currency) }}
            </div>
          </div>

          <div class="grid grid-cols-2 gap-2">
            <div class="rounded-md border border-border/70 px-3 py-2">
              <div class="text-xs text-muted-foreground">
                已用额度
              </div>
              <div class="mt-1 text-sm font-medium text-foreground">
                {{ formatBalanceAmount(balanceData.total_used, balanceData.currency) }}
              </div>
            </div>
            <div class="rounded-md border border-border/70 px-3 py-2">
              <div class="text-xs text-muted-foreground">
                授予额度
              </div>
              <div class="mt-1 text-sm font-medium text-foreground">
                {{ formatBalanceAmount(balanceData.total_granted, balanceData.currency) }}
              </div>
            </div>
          </div>

          <div
            v-if="balanceExtraRows.length > 0"
            class="rounded-md border border-border/70 px-3 py-2"
          >
            <div class="text-xs text-muted-foreground">
              明细
            </div>
            <div class="mt-2 space-y-1.5 text-sm">
              <div
                v-for="row in balanceExtraRows"
                :key="row.label"
                class="flex items-center justify-between gap-3"
              >
                <span class="text-muted-foreground">{{ row.label }}</span>
                <span class="font-medium text-foreground">{{ row.value }}</span>
              </div>
            </div>
          </div>

          <div
            v-if="balanceResult.response_time_ms !== null"
            class="text-xs text-muted-foreground"
          >
            响应耗时 {{ balanceResult.response_time_ms }}ms
          </div>
        </template>
      </template>
    </div>

    <template #footer>
      <Button
        variant="outline"
        @click="balanceDialogOpen = false"
      >
        关闭
      </Button>
    </template>
  </Dialog>
</template>

<script setup lang="ts">
import { ref, computed, watch } from 'vue'
import {
  Dialog,
  Button,
  Input,
  Label,
  Switch,
  Select,
  SelectTrigger,
  SelectValue,
  SelectContent,
  SelectItem,
} from '@/components/ui'
import { Key, SquarePen, CircleHelp, WalletCards, Loader2 } from 'lucide-vue-next'
import { useToast } from '@/composables/useToast'
import { useFormDialog } from '@/composables/useFormDialog'
import { parseApiError } from '@/utils/errorParser'
import { parseNumberInput, parseNullableNumberInput } from '@/utils/form'
import JsonImportInput from '@/components/common/JsonImportInput.vue'
import {
  addProviderKey,
  updateProviderKey,
  queryProviderKeyBalance,
  sortApiFormats,
  type EndpointAPIKey,
  type EndpointAPIKeyUpdate,
  type ProviderKeyBalanceInfo,
  type ProviderKeyBalanceQuery,
  type ProviderKeyBalanceResult,
  type ProviderEndpoint,
  type ProviderType
} from '@/api/endpoints'
import { formatApiFormat, normalizeApiFormatAlias, formatSupportsAuthOverride } from '@/api/endpoints/types/api-format'

type RawSecretAuthType = 'api_key' | 'bearer'
type ProviderKeyFormAuthType = RawSecretAuthType | 'service_account'

interface AuthTypeOption {
  value: ProviderKeyFormAuthType
  label: string
}

type BalanceQueryTemplate = 'new_api' | 'sub2api' | 'generic_api'
type BalanceQueryMethod = 'GET' | 'POST'

interface BalanceQueryTemplateOption {
  value: BalanceQueryTemplate
  label: string
  title: string
  description: string
  badge: string
}

const props = defineProps<{
  open: boolean
  endpoint: ProviderEndpoint | null
  editingKey: EndpointAPIKey | null
  providerId: string | null
  providerType: ProviderType | null
  availableApiFormats: string[]  // Provider 支持的所有 API 格式
}>()

const emit = defineEmits<{
  close: []
  saved: []
}>()

const { success, error: showError } = useToast()

function isRawSecretAuthType(authType: string | null | undefined): authType is RawSecretAuthType {
  return authType === 'api_key' || authType === 'bearer'
}

function normalizeRawSecretAuthType(authType: string | null | undefined): RawSecretAuthType | null {
  const normalized = (authType || '').trim().toLowerCase()
  if (normalized === 'api_key' || normalized === 'apikey' || normalized === 'api-key') return 'api_key'
  if (normalized === 'bearer' || normalized === 'bearer_token' || normalized === 'bearer-token' || normalized === 'authorization') return 'bearer'
  return null
}

function normalizeFormAuthType(authType: string | null | undefined): ProviderKeyFormAuthType {
  const normalized = (authType || '').trim().toLowerCase()
  if (normalized === 'bearer') return 'bearer'
  if (normalized === 'service_account' || normalized === 'vertex_ai') return 'service_account'
  return 'api_key'
}

function getAuthTypeOptions(providerType: ProviderType | null): AuthTypeOption[] {
  if ((providerType || '').toLowerCase() === 'vertex_ai') {
    return [
      { value: 'api_key', label: 'API Key' },
      { value: 'service_account', label: 'Service Account' },
    ]
  }

  return [
    { value: 'api_key', label: 'API Key' },
    { value: 'bearer', label: 'Bearer Token' },
  ]
}

function getVertexAllowedFormatsByAuth(authType: ProviderKeyFormAuthType): Set<string> {
  if (authType === 'api_key') {
    return new Set(['gemini:generate_content', 'gemini:embedding'])
  }
  if (authType === 'service_account') {
    return new Set(['gemini:generate_content', 'gemini:embedding', 'claude:messages'])
  }
  return new Set()
}

function normalizeApiFormat(format: string): string {
  return normalizeApiFormatAlias(format).trim().toLowerCase()
}

function getSelectableApiFormats(authType = form.value.auth_type): string[] {
  const sorted = sortApiFormats(props.availableApiFormats)
  if (props.providerType !== 'vertex_ai') {
    return sorted
  }

  const allowed = getVertexAllowedFormatsByAuth(authType)
  return sorted.filter(fmt => allowed.has(normalizeApiFormat(fmt)))
}

function sanitizeApiFormats(formats: string[], authType = form.value.auth_type): string[] {
  const selectable = new Set(getSelectableApiFormats(authType).map(normalizeApiFormat))
  if (selectable.size === 0) {
    return []
  }

  return formats.filter(format => selectable.has(normalizeApiFormat(format)))
}

function sanitizeAuthTypeByFormat(
  authTypeByFormat: Record<string, string> | null | undefined,
  formats = form.value.api_formats,
  authType = form.value.auth_type
): Record<string, RawSecretAuthType> {
  if (!isRawSecretAuthType(authType) || !authTypeByFormat) {
    return {}
  }

  const selected = new Set(formats.map(normalizeApiFormat))
  const sanitized: Record<string, RawSecretAuthType> = {}
  for (const [format, rawAuthType] of Object.entries(authTypeByFormat)) {
    const normalizedFormat = normalizeApiFormat(format)
    if (!selected.has(normalizedFormat)) continue
    const normalizedAuthType = normalizeRawSecretAuthType(rawAuthType)
    if (!normalizedAuthType || normalizedAuthType === authType) continue
    sanitized[normalizedFormat] = normalizedAuthType
  }
  return sanitized
}

function sanitizeAllowAuthChannelMismatchFormats(
  formats: string[] | null | undefined,
  selectedFormats = form.value.api_formats
): string[] {
  if (!formats) return []
  const selected = new Set(selectedFormats.map(normalizeApiFormat))
  const seen = new Set<string>()
  const sanitized: string[] = []
  for (const format of formats) {
    const normalizedFormat = normalizeApiFormat(format)
    if (!normalizedFormat || !selected.has(normalizedFormat) || seen.has(normalizedFormat)) {
      continue
    }
    seen.add(normalizedFormat)
    sanitized.push(normalizedFormat)
  }
  return sanitized
}

function getDefaultApiFormats(): string[] {
  const endpointFormat = props.endpoint?.api_format
  if (endpointFormat) {
    const endpointFormats = sanitizeApiFormats([endpointFormat])
    if (endpointFormats.length > 0) {
      return endpointFormats
    }
  }

  const firstAvailableFormat = getSelectableApiFormats()[0]
  return firstAvailableFormat ? [firstAvailableFormat] : []
}

// 按 provider/auth_type 过滤后的可用 API 格式列表
const visibleApiFormats = computed(() => getSelectableApiFormats())

const authTypeOptions = computed(() => getAuthTypeOptions(props.providerType))
const showAuthTypeSelector = computed(() => props.providerType === 'vertex_ai')

const apiFormatHelpOpen = ref(false)
const apiFormatHelpHovered = ref(false)
const apiFormatHelpVisible = computed(() => apiFormatHelpOpen.value || apiFormatHelpHovered.value)

function toggleApiFormatHelp() {
  apiFormatHelpOpen.value = !apiFormatHelpOpen.value
  if (!apiFormatHelpOpen.value) {
    apiFormatHelpHovered.value = false
  }
}

const authSecretLabel = computed(() => {
  if (form.value.auth_type === 'service_account') return 'Service Account JSON'
  if (form.value.auth_type === 'bearer') return 'Bearer Token'
  return 'API 密钥'
})

const authSecretPlaceholder = computed(() =>
  form.value.auth_type === 'bearer' ? 'token-...' : 'sk-...'
)

const authSecretRequiredMark = computed(() => {
  if (form.value.auth_type === 'service_account' && (!props.editingKey || switchingToServiceAccount.value)) {
    return '*'
  }
  return ''
})



function getFormatAuthType(format: string): ProviderKeyFormAuthType {
  if (!isRawSecretAuthType(form.value.auth_type)) {
    return form.value.auth_type
  }
  return form.value.auth_type_by_format[normalizeApiFormat(format)] || form.value.auth_type
}

function canOverrideFormatAuth(format: string): boolean {
  return isRawSecretAuthType(form.value.auth_type) && form.value.api_formats.includes(format) && formatSupportsAuthOverride(format)
}

function setFormatAuthType(format: string, authType: RawSecretAuthType) {
  if (!isRawSecretAuthType(form.value.auth_type)) return
  const normalizedFormat = normalizeApiFormat(format)
  const next = { ...form.value.auth_type_by_format }
  if (authType === form.value.auth_type) {
    delete next[normalizedFormat]
  } else {
    next[normalizedFormat] = authType
  }
  form.value.auth_type_by_format = sanitizeAuthTypeByFormat(next)
}

function canToggleAuthChannelMismatch(format: string): boolean {
  return canOverrideFormatAuth(format)
}

function isAuthChannelMismatchAllowed(format: string): boolean {
  return form.value.allow_auth_channel_mismatch_formats.includes(normalizeApiFormat(format))
}

function setAuthChannelMismatchAllowed(format: string, allowed: boolean) {
  const normalizedFormat = normalizeApiFormat(format)
  const next = new Set(form.value.allow_auth_channel_mismatch_formats.map(normalizeApiFormat))
  if (allowed) {
    next.add(normalizedFormat)
  } else {
    next.delete(normalizedFormat)
  }
  form.value.allow_auth_channel_mismatch_formats = sanitizeAllowAuthChannelMismatchFormats([...next])
}

function buildAuthTypeByFormatPayload(): Record<string, RawSecretAuthType> | null {
  const sanitized = sanitizeAuthTypeByFormat(form.value.auth_type_by_format)
  return Object.keys(sanitized).length > 0 ? sanitized : null
}

function buildAllowAuthChannelMismatchFormatsPayload(): string[] {
  const sanitized = sanitizeAllowAuthChannelMismatchFormats(form.value.allow_auth_channel_mismatch_formats)
  return sanitized
}

const serviceAccountDescription = computed(() => (
  props.editingKey
    ? '留空表示不修改；JSON 格式，包含 project_id、private_key 等字段'
    : 'JSON 格式，包含 project_id、private_key 等字段'
))

// 默认认证类型
function getDefaultAuthType(): ProviderKeyFormAuthType {
  return authTypeOptions.value[0]?.value || 'api_key'
}

function getDefaultAllowAuthChannelMismatchFormats(formats = getDefaultApiFormats()): string[] {
  return sanitizeAllowAuthChannelMismatchFormats(formats, formats)
}

// 显示自动获取模型警告：编辑模式下，原本未启用但现在启用，且已有 allowed_models
const showAutoFetchWarning = computed(() => {
  if (!props.editingKey) return false
  // 原本已启用，不需要警告
  if (props.editingKey.auto_fetch_models) return false
  // 现在未启用，不需要警告
  if (!form.value.auto_fetch_models) return false
  // 检查是否有已配置的模型权限
  const allowedModels = props.editingKey.allowed_models
  if (!allowedModels) return false
  if (Array.isArray(allowedModels) && allowedModels.length === 0) return false
  if (typeof allowedModels === 'object' && Object.keys(allowedModels).length === 0) return false
  return true
})

const autoFetchWarningMessage = computed(() => {
  if (!showAutoFetchWarning.value || !props.editingKey?.allowed_models) return ''
  const models = Array.isArray(props.editingKey.allowed_models)
    ? props.editingKey.allowed_models
    : []
  if (models.length === 0) return ''
  return `当前 Key 模型权限存在以下模型：${models.map(model => `“${model}”`).join('、')}，开启自动获取后将被覆盖`
})

// 检查是否正在切换认证类型
const switchingToServiceAccount = computed(() =>
  !!props.editingKey &&
  props.editingKey.auth_type !== 'service_account' &&
  form.value.auth_type === 'service_account'
)

// 表单是否可以保存
const canSave = computed(() => {
  // 必须填写密钥名称
  if (!form.value.name.trim()) return false
  // 新增模式下根据认证类型判断必填字段
  if (!props.editingKey) {
    if (form.value.auth_type === 'service_account' && !form.value.auth_config_text.trim()) return false
  } else {
    // 编辑模式下切换认证类型时，必须填写对应字段
    if (switchingToServiceAccount.value && !form.value.auth_config_text.trim()) return false
  }
  // 必须至少选择一个 API 格式
  if (form.value.api_formats.length === 0) return false
  return true
})

const isOpen = computed(() => props.open)
const saving = ref(false)
const formNonce = ref(createFieldNonce())
const keyNameInputId = computed(() => `key-name-${formNonce.value}`)
const apiKeyInputId = computed(() => `api-key-${formNonce.value}`)
const authTypeSelectId = computed(() => `auth-type-${formNonce.value}`)
const keyNameFieldName = computed(() => `key-name-field-${formNonce.value}`)
const apiKeyFieldName = computed(() => `api-key-field-${formNonce.value}`)

// 新增密钥时默认不自动开启上游模型获取
const defaultAutoFetchModels = computed(() => false)

const form = ref({
  name: '',
  api_key: '',  // 标准 API Key
  auth_type: 'api_key' as ProviderKeyFormAuthType,  // 认证类型
  auth_type_by_format: {} as Record<string, RawSecretAuthType>,
  allow_auth_channel_mismatch_formats: [] as string[],
  auth_config_text: '',  // Service Account JSON 文本（用于表单输入）
  api_formats: [] as string[],  // 支持的 API 格式列表
  rate_multipliers: {} as Record<string, number>,  // 按 API 格式的成本倍率
  internal_priority: 10,
  rpm_limit: undefined as number | null | undefined,  // RPM 限制（null=自适应，undefined=保持原值）
  concurrent_limit: undefined as number | null | undefined,  // 并发请求上限（null/0=不限制，undefined=保持原值）
  cache_ttl_minutes: 5,
  max_probe_interval_minutes: 32,
  note: '',
  is_active: true,
  auto_fetch_models: false,
  model_include_patterns_text: '',  // 包含规则文本（逗号分隔）
  model_exclude_patterns_text: ''   // 排除规则文本（逗号分隔）
})

const balanceDialogOpen = ref(false)
const balanceQueryDialogOpen = ref(false)
const saveBalanceResult = ref(false)
const saveBalanceSecret = ref(false)
const balanceAutoRefreshIntervalMinutes = ref<number>(0)
const balanceQueryTemplate = ref<BalanceQueryTemplate>('new_api')
const balanceQueryMethods: BalanceQueryMethod[] = ['GET', 'POST']
const balanceQueryTemplateOptions: BalanceQueryTemplateOption[] = [
  {
    value: 'new_api',
    label: 'NewAPI',
    title: 'NewAPI 余额接口',
    description: '使用个人安全设置中的访问令牌请求 /api/user/self，并按 quota / 500000 换算余额。',
    badge: '/api/user/self'
  },
  {
    value: 'sub2api',
    label: 'Sub2API',
    title: 'Sub2API 余量接口',
    description: '使用当前 Key 或单独填写的 API Key 请求 /v1/usage 查询余量。',
    badge: '/v1/usage'
  },
  {
    value: 'generic_api',
    label: '自定义',
    title: '自定义余额接口',
    description: '自行指定请求路径、额度换算和响应字段，适合 DeepSeek 或其他提供商的官方余额接口。',
    badge: 'Custom'
  }
]
const newApiBalanceQuery = ref({
  base_url: '',
  access_token: '',
  user_id: ''
})
const sub2ApiBalanceQuery = ref({
  base_url: '',
  token: ''
})
const customBalanceQuery = ref({
  base_url: '',
  api_key: '',
  endpoint: '/api/user/self',
  method: 'GET' as BalanceQueryMethod,
  currency: 'USD',
  quota_divisor: 500000 as number | string,
  balance_path: 'data.quota',
  used_path: 'data.used_quota',
  granted_path: ''
})
const balanceLoading = ref(false)
const balanceResult = ref<ProviderKeyBalanceResult | null>(null)
const balanceError = ref('')
const pendingBalanceSaveQuery = ref<ProviderKeyBalanceQuery | null>(null)
const canSaveBalanceResultToKey = computed(() => (
  !!props.providerId && isRawSecretAuthType(form.value.auth_type)
))
const hasSavedBalanceSecret = computed(() => (
  props.editingKey?.upstream_metadata?.balance_query?.query_config?.has_saved_secret === true
))
const savedBalanceQueryTemplate = computed(() => getSavedBalanceQueryTemplate())
const savedBalanceQueryConfig = computed(() => (
  props.editingKey?.upstream_metadata?.balance_query?.query_config || null
))
const hasSavedBalanceSecretForSelectedQuery = computed(() => {
  if (!hasSavedBalanceSecret.value || savedBalanceQueryTemplate.value !== balanceQueryTemplate.value) {
    return false
  }
  if (balanceQueryTemplate.value !== 'sub2api') {
    return true
  }
  const savedKind = String(savedBalanceQueryConfig.value?.sub2api_credential_kind || '').trim()
  return savedKind === 'api_key'
})
const balanceCredentialRequiresDedicatedSecret = computed(() => {
  if (balanceQueryTemplate.value === 'new_api') return true
  return false
})
const canConfigureBalanceSecretSave = computed(() => (
  balanceCredentialRequiresDedicatedSecret.value || hasSavedBalanceSecretForSelectedQuery.value
))
const balanceSaveDescription = computed(() => (
  props.editingKey?.id
    ? '保存余额摘要、更新时间和查询配置'
    : '新增 Key 会在保存后自动写入余额摘要、查询模板和自动查询间隔'
))
const balanceSecretSaveDescription = computed(() => (
  saveBalanceResult.value
    ? '加密保存本次填写的余额查询令牌，用于刷新和自动查询；不替换当前 Key。'
    : '需要先开启“查询成功后保存到当前 Key”。'
))
const effectiveBalanceSecret = computed(() => {
  if (balanceQueryTemplate.value === 'new_api') {
    return newApiBalanceQuery.value.access_token.trim()
  }
  if (balanceQueryTemplate.value === 'sub2api') {
    return sub2ApiBalanceQuery.value.token.trim() || form.value.api_key.trim()
  }
  return customBalanceQuery.value.api_key.trim() || form.value.api_key.trim()
})
const hasBalanceQuerySecret = computed(() => {
  if (effectiveBalanceSecret.value || hasSavedBalanceSecretForSelectedQuery.value) {
    return true
  }
  return !!props.editingKey?.id && !balanceCredentialRequiresDedicatedSecret.value
})
const selectedBalanceQueryTemplate = computed(() => (
  balanceQueryTemplateOptions.find(option => option.value === balanceQueryTemplate.value)
  || balanceQueryTemplateOptions[0]
))
const canConfirmBalanceQuery = computed(() => {
  if (balanceLoading.value || !hasBalanceQuerySecret.value) return false
  if (balanceQueryTemplate.value === 'generic_api') {
    return !!customBalanceQuery.value.endpoint.trim()
  }
  return true
})

const canQueryBalance = computed(() => (
  !!props.providerId &&
  isRawSecretAuthType(form.value.auth_type) &&
  !saving.value &&
  !balanceLoading.value
))

const balanceData = computed<ProviderKeyBalanceInfo | null>(() => (
  balanceResult.value?.data ?? null
))
const balanceResultPendingSave = computed(() => (
  !props.editingKey?.id
  && !!pendingBalanceSaveQuery.value
  && balanceResult.value?.status === 'success'
))

const balanceExtraRows = computed(() => {
  const data = balanceData.value
  const extra = data?.extra
  if (!extra) return []
  const rows: Array<{ label: string; value: string }> = []
  const currency = data.currency || 'USD'
  const planName = typeof extra.plan_name === 'string'
    ? extra.plan_name
    : typeof extra.planName === 'string'
      ? extra.planName
      : null
  const balance = toFiniteNumber(extra.balance)
  const points = toFiniteNumber(extra.points)
  const activeSubscriptions = toFiniteNumber(extra.active_subscriptions)
  const totalUsedUsd = toFiniteNumber(extra.total_used_usd)
  if (planName) {
    rows.push({ label: '套餐', value: planName })
  }
  if (balance !== null) {
    rows.push({ label: '余额', value: formatBalanceAmount(balance, currency) })
  }
  if (points !== null) {
    rows.push({ label: '积分', value: formatBalanceAmount(points, currency) })
  }
  if (activeSubscriptions !== null) {
    rows.push({ label: '有效订阅', value: `${activeSubscriptions}` })
  }
  if (totalUsedUsd !== null) {
    rows.push({ label: '订阅已用', value: formatBalanceAmount(totalUsedUsd, 'USD') })
  }
  return rows
})

function toFiniteNumber(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value === 'string' && value.trim()) {
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : null
  }
  return null
}

function normalizeAutoRefreshInterval(value: unknown): number {
  const parsed = toFiniteNumber(value)
  if (parsed === null || parsed <= 0) return 0
  return Math.min(Math.floor(parsed), 10080)
}

function setBalanceAutoRefreshInterval(value: unknown) {
  balanceAutoRefreshIntervalMinutes.value = normalizeAutoRefreshInterval(value)
}

function formatBalanceAmount(value: unknown, currency = 'USD'): string {
  const numberValue = toFiniteNumber(value)
  if (numberValue === null) return '未知'
  const normalizedCurrency = (currency || 'USD').toUpperCase()
  const prefix = normalizedCurrency === 'USD'
    ? '$'
    : normalizedCurrency === 'CNY'
      ? '¥'
      : `${normalizedCurrency} `
  const decimals = Math.abs(numberValue) >= 100 ? 2 : 4
  return `${prefix}${numberValue.toFixed(decimals)}`
}

function balanceStatusText(status: ProviderKeyBalanceResult['status']): string {
  const labels: Record<ProviderKeyBalanceResult['status'], string> = {
    success: '查询成功',
    pending: '查询处理中',
    auth_failed: '认证失败',
    auth_expired: '认证已过期',
    rate_limited: '请求频率受限',
    network_error: '网络错误',
    parse_error: '响应解析失败',
    not_configured: '未配置余额查询',
    not_supported: '当前上游不支持余额查询',
    already_done: '已完成',
    unknown_error: '查询失败'
  }
  return labels[status] || '查询失败'
}

function normalizeBalanceQueryTemplate(value: unknown): BalanceQueryTemplate | null {
  const normalized = String(value || '').trim().toLowerCase().replace(/-/g, '_')
  if (normalized === 'newapi' || normalized === 'new_api') return 'new_api'
  if (normalized === 'sub2api') return 'sub2api'
  if (normalized === 'generic' || normalized === 'custom' || normalized === 'generic_api') return 'generic_api'
  return null
}

function getSavedBalanceQueryTemplate(): BalanceQueryTemplate | null {
  return normalizeBalanceQueryTemplate(props.editingKey?.upstream_metadata?.balance_query?.architecture_id)
}

function getSavedBalanceAutoRefreshInterval(): number {
  return normalizeAutoRefreshInterval(
    props.editingKey?.upstream_metadata?.balance_query?.query_config?.auto_refresh_interval_minutes
  )
}

function setSaveBalanceResult(value: boolean) {
  saveBalanceResult.value = value && canSaveBalanceResultToKey.value
  if (!saveBalanceResult.value) {
    saveBalanceSecret.value = false
    pendingBalanceSaveQuery.value = null
  } else if (props.editingKey || balanceCredentialRequiresDedicatedSecret.value) {
    saveBalanceSecret.value = true
  }
}

watch(
  [() => form.value.auth_type, () => props.providerType, () => props.availableApiFormats],
  () => {
    const allowedAuthTypes = new Set(authTypeOptions.value.map(option => option.value))
    if (!allowedAuthTypes.has(form.value.auth_type)) {
      form.value.auth_type = getDefaultAuthType()
      return
    }
    if (!isRawSecretAuthType(form.value.auth_type)) {
      setSaveBalanceResult(false)
    }

    const filtered = sanitizeApiFormats(form.value.api_formats)
    if (filtered.length !== form.value.api_formats.length) {
      form.value.api_formats = [...filtered]
    }
    form.value.auth_type_by_format = sanitizeAuthTypeByFormat(form.value.auth_type_by_format)
    form.value.allow_auth_channel_mismatch_formats = sanitizeAllowAuthChannelMismatchFormats(
      form.value.allow_auth_channel_mismatch_formats
    )
  },
  { immediate: true }
)

watch(
  [() => props.availableApiFormats, () => props.open, () => props.editingKey],
  ([, open, editingKey]) => {
    if (!open) {
      return
    }

    const filtered = sanitizeApiFormats(form.value.api_formats)
    if (filtered.length !== form.value.api_formats.length) {
      form.value.api_formats = [...filtered]
      form.value.auth_type_by_format = sanitizeAuthTypeByFormat(form.value.auth_type_by_format, filtered)
      form.value.allow_auth_channel_mismatch_formats = sanitizeAllowAuthChannelMismatchFormats(
        form.value.allow_auth_channel_mismatch_formats,
        filtered
      )
      return
    }

    if (!editingKey && form.value.api_formats.length === 0) {
      const defaults = getDefaultApiFormats()
      if (defaults.length > 0) {
        form.value.api_formats = defaults
        form.value.allow_auth_channel_mismatch_formats =
          getDefaultAllowAuthChannelMismatchFormats(defaults)
      }
    }
  },
  { deep: true, immediate: true }
)

// API 格式切换
function toggleApiFormat(format: string) {
  const index = form.value.api_formats.indexOf(format)
  if (index === -1) {
    // 添加格式
    form.value.api_formats.push(format)
    setAuthChannelMismatchAllowed(format, true)
  } else {
    // 移除格式，但保留隐藏配置（用户可能只是临时取消）
    form.value.api_formats.splice(index, 1)
  }
  form.value.allow_auth_channel_mismatch_formats = sanitizeAllowAuthChannelMismatchFormats(
    form.value.allow_auth_channel_mismatch_formats
  )
}


// 重置表单
function resetForm() {
  formNonce.value = createFieldNonce()
  pendingBalanceSaveQuery.value = null
  saveBalanceResult.value = false
  saveBalanceSecret.value = false
  balanceAutoRefreshIntervalMinutes.value = 0
  const defaultApiFormats = getDefaultApiFormats()
  form.value = {
    name: '',
    api_key: '',
    auth_type: getDefaultAuthType(),
    auth_type_by_format: {},
    allow_auth_channel_mismatch_formats:
      getDefaultAllowAuthChannelMismatchFormats(defaultApiFormats),
    auth_config_text: '',
    api_formats: defaultApiFormats,
    rate_multipliers: {},
    internal_priority: 10,
    rpm_limit: undefined,
    concurrent_limit: undefined,
    cache_ttl_minutes: 5,
    max_probe_interval_minutes: 32,
    note: '',
    is_active: true,
    auto_fetch_models: defaultAutoFetchModels.value,
    model_include_patterns_text: '',
    model_exclude_patterns_text: ''
  }
}

// 添加成功后清除部分字段以便继续添加
function clearForNextAdd() {
  formNonce.value = createFieldNonce()
  pendingBalanceSaveQuery.value = null
  saveBalanceResult.value = false
  saveBalanceSecret.value = false
  balanceAutoRefreshIntervalMinutes.value = 0
  form.value.name = ''
  form.value.api_key = ''
  form.value.auth_config_text = ''
  form.value.auth_type_by_format = sanitizeAuthTypeByFormat(form.value.auth_type_by_format)
  form.value.allow_auth_channel_mismatch_formats = sanitizeAllowAuthChannelMismatchFormats(
    form.value.allow_auth_channel_mismatch_formats
  )
}

// 加载密钥数据（编辑模式）
function loadKeyData() {
  if (!props.editingKey) return
  formNonce.value = createFieldNonce()
  pendingBalanceSaveQuery.value = null
  form.value = {
    name: props.editingKey.name,
    api_key: '',
    auth_type: normalizeFormAuthType(props.editingKey.auth_type),
    auth_type_by_format: sanitizeAuthTypeByFormat(
      props.editingKey.auth_type_by_format || {},
      props.editingKey.api_formats || [],
      normalizeFormAuthType(props.editingKey.auth_type)
    ),
    allow_auth_channel_mismatch_formats: sanitizeAllowAuthChannelMismatchFormats(
      props.editingKey.allow_auth_channel_mismatch_formats || [],
      props.editingKey.api_formats || []
    ),
    auth_config_text: '',  // auth_config 不返回给前端，编辑时需要重新输入
    api_formats: props.editingKey.api_formats?.length > 0
      ? sanitizeApiFormats(
        props.editingKey.api_formats,
        normalizeFormAuthType(props.editingKey.auth_type)
      )
      : [],  // 编辑模式下保持原有选择，不默认全选
    rate_multipliers: { ...(props.editingKey.rate_multipliers || {}) },
    internal_priority: props.editingKey.internal_priority ?? 10,
    // 保留原始的 null/undefined 状态，null 表示自适应模式
    rpm_limit: props.editingKey.rpm_limit ?? undefined,
    concurrent_limit: props.editingKey.concurrent_limit ?? undefined,
    cache_ttl_minutes: props.editingKey.cache_ttl_minutes ?? 5,
    max_probe_interval_minutes: props.editingKey.max_probe_interval_minutes ?? 32,
    note: props.editingKey.note || '',
    is_active: props.editingKey.is_active,
    auto_fetch_models: props.editingKey.auto_fetch_models ?? false,
    model_include_patterns_text: (props.editingKey.model_include_patterns || []).join(', '),
    model_exclude_patterns_text: (props.editingKey.model_exclude_patterns || []).join(', ')
  }
}

// 使用 useFormDialog 统一处理对话框逻辑
const { isEditMode, handleDialogUpdate, handleCancel } = useFormDialog({
  isOpen: () => props.open,
  entity: () => props.editingKey,
  isLoading: saving,
  onClose: () => emit('close'),
  loadData: loadKeyData,
  resetForm,
})

function createFieldNonce(): string {
  return Math.random().toString(36).slice(2, 10)
}

// 将逗号分隔的文本解析为数组（去空、去重）
// 返回空数组而非 undefined，以便后端能正确清除已有规则
function parsePatternText(text: string): string[] {
  if (!text.trim()) return []
  const patterns = text
    .split(',')
    .map(s => s.trim())
    .filter(s => s.length > 0)
  return [...new Set(patterns)]
}

// 解析 Service Account JSON 文本
function parseAuthConfig(): Record<string, unknown> | null {
  if (form.value.auth_type !== 'service_account') return null
  const text = form.value.auth_config_text.trim()
  if (!text) return null
  try {
    return JSON.parse(text)
  } catch {
    return null
  }
}

function handleServiceAccountImportError(payload: { message: string, title?: string }) {
  showError(payload.message, payload.title || '错误')
}

async function handleQueryBalance() {
  if (!props.providerId) {
    showError('无法查询：缺少提供商信息', '错误')
    return
  }
  if (!isRawSecretAuthType(form.value.auth_type)) {
    showError('余额查询仅支持 API Key 或 Bearer Token', '错误')
    return
  }
  balanceQueryTemplate.value = getSavedBalanceQueryTemplate() || inferBalanceQueryTemplate()
  setSaveBalanceResult(!!props.editingKey)
  balanceAutoRefreshIntervalMinutes.value = getSavedBalanceAutoRefreshInterval()
  balanceQueryDialogOpen.value = true
}

async function confirmBalanceQuery() {
  if (!props.providerId) {
    showError('无法查询：缺少提供商信息', '错误')
    return
  }
  if (!isRawSecretAuthType(form.value.auth_type)) {
    showError('余额查询仅支持 API Key 或 Bearer Token', '错误')
    return
  }

  const apiKey = effectiveBalanceSecret.value
  if (!apiKey && !props.editingKey?.id) {
    showError('请填写本次查询凭据，或先在外层填写/保存 API 密钥', '验证失败')
    return
  }
  if (balanceCredentialRequiresDedicatedSecret.value && !apiKey && !hasSavedBalanceSecretForSelectedQuery.value) {
    showError('请填写本次查询凭据，或先保存过同类型的余额查询凭据', '验证失败')
    return
  }
  if (balanceQueryTemplate.value === 'generic_api' && !customBalanceQuery.value.endpoint.trim()) {
    showError('请填写自定义查询路径', '验证失败')
    return
  }

  balanceQueryDialogOpen.value = false
  balanceDialogOpen.value = true
  balanceLoading.value = true
  balanceResult.value = null
  balanceError.value = ''

  try {
    const saveToExistingKey = saveBalanceResult.value && !!props.editingKey?.id
    const autoRefreshInterval = saveBalanceResult.value
      ? normalizeAutoRefreshInterval(balanceAutoRefreshIntervalMinutes.value)
      : 0
    const query = buildBalanceQueryPayload(apiKey, {
      keyId: props.editingKey?.id,
      saveResult: saveToExistingKey,
      saveBalanceSecret: saveToExistingKey && saveBalanceSecret.value,
      autoRefreshIntervalMinutes: autoRefreshInterval,
    })
    balanceResult.value = await queryProviderKeyBalance(props.providerId, query)
    if (balanceResult.value.saved_to_key) {
      emit('saved')
    }
    if (!props.editingKey?.id) {
      pendingBalanceSaveQuery.value = saveBalanceResult.value && balanceResult.value.status === 'success'
        ? buildBalanceQueryPayload(apiKey, {
          saveResult: true,
          saveBalanceSecret: saveBalanceSecret.value,
          autoRefreshIntervalMinutes: autoRefreshInterval,
        })
        : null
    }
  } catch (err: unknown) {
    balanceError.value = parseApiError(err, '查询余额失败')
  } finally {
    balanceLoading.value = false
  }
}

function buildBalanceQueryPayload(
  apiKey: string,
  options: {
    keyId?: string
    saveResult: boolean
    saveBalanceSecret: boolean
    autoRefreshIntervalMinutes: number
  },
): ProviderKeyBalanceQuery {
  const query: ProviderKeyBalanceQuery = {
    key_id: options.keyId,
    api_key: apiKey || undefined,
    auth_type: balanceQueryAuthType(),
    api_formats: [...form.value.api_formats],
    architecture_id: balanceQueryTemplate.value,
    save_result: options.saveResult,
    save_balance_secret: options.saveBalanceSecret,
    auto_refresh_interval_minutes: options.autoRefreshIntervalMinutes,
  }
  if (balanceQueryTemplate.value === 'new_api') {
    query.custom_base_url = trimmedOrUndefined(newApiBalanceQuery.value.base_url)
    query.new_api_user_id = trimmedOrUndefined(newApiBalanceQuery.value.user_id)
  } else if (balanceQueryTemplate.value === 'sub2api') {
    query.custom_base_url = trimmedOrUndefined(sub2ApiBalanceQuery.value.base_url)
    query.sub2api_credential_kind = 'api_key'
  } else if (balanceQueryTemplate.value === 'generic_api') {
    const quotaDivisor = toFiniteNumber(customBalanceQuery.value.quota_divisor)
    query.custom_base_url = trimmedOrUndefined(customBalanceQuery.value.base_url)
    query.custom_endpoint = customBalanceQuery.value.endpoint.trim()
    query.custom_method = customBalanceQuery.value.method
    query.custom_currency = trimmedOrUndefined(customBalanceQuery.value.currency)
    query.custom_quota_divisor = quotaDivisor && quotaDivisor > 0 ? quotaDivisor : undefined
    query.custom_balance_path = trimmedOrUndefined(customBalanceQuery.value.balance_path)
    query.custom_used_path = trimmedOrUndefined(customBalanceQuery.value.used_path)
    query.custom_granted_path = trimmedOrUndefined(customBalanceQuery.value.granted_path)
  }
  return query
}

function balanceQueryAuthType(): ProviderKeyBalanceQuery['auth_type'] {
  if (balanceQueryTemplate.value === 'sub2api') {
    return 'api_key'
  }
  if (balanceQueryTemplate.value === 'new_api') {
    return 'api_key'
  }
  return form.value.auth_type
}

function inferBalanceQueryTemplate(): BalanceQueryTemplate {
  const fingerprint = [
    props.providerType || '',
    props.endpoint?.provider_name || '',
    props.endpoint?.base_url || ''
  ].join(' ').toLowerCase()
  return fingerprint.includes('sub2api') ? 'sub2api' : 'new_api'
}

function trimmedOrUndefined(value: string | null | undefined): string | undefined {
  const trimmed = (value || '').trim()
  return trimmed ? trimmed : undefined
}

async function persistPendingBalanceForCreatedKey(keyId: string): Promise<boolean> {
  if (!props.providerId || !pendingBalanceSaveQuery.value) {
    return false
  }
  try {
    const result = await queryProviderKeyBalance(props.providerId, {
      ...pendingBalanceSaveQuery.value,
      key_id: keyId,
      save_result: true,
    })
    if (result.status !== 'success' || !result.saved_to_key) {
      showError(result.save_message || result.message || '余额配置未写入新 Key', '余额保存失败')
      return false
    }
    pendingBalanceSaveQuery.value = null
    return true
  } catch (err: unknown) {
    showError(parseApiError(err, '余额配置未写入新 Key'), '余额保存失败')
    return false
  }
}

async function handleSave() {
  // 必须有 providerId
  if (!props.providerId) {
    showError('无法保存：缺少提供商信息', '错误')
    return
  }

  // 验证认证信息
  if (form.value.auth_type === 'service_account') {
    if (!props.editingKey && !form.value.auth_config_text.trim()) {
      showError('请输入 Service Account JSON', '验证失败')
      return
    }
    // 验证 JSON 格式
    if (form.value.auth_config_text.trim()) {
      const parsed = parseAuthConfig()
      if (!parsed) {
        showError('Service Account JSON 格式无效', '验证失败')
        return
      }
      // 验证必要字段
      if (!parsed.client_email || !parsed.private_key || !parsed.project_id) {
        showError('Service Account JSON 缺少必要字段 (client_email, private_key, project_id)', '验证失败')
        return
      }
    }
  }

  form.value.api_formats = sanitizeApiFormats(form.value.api_formats)
  form.value.allow_auth_channel_mismatch_formats = sanitizeAllowAuthChannelMismatchFormats(
    form.value.allow_auth_channel_mismatch_formats
  )

  // 验证至少选择一个 API 格式
  if (form.value.api_formats.length === 0) {
    showError('请至少选择一个 API 格式', '验证失败')
    return
  }

  saving.value = true
  try {
    // 准备 rate_multipliers 数据：只保留已选中格式的倍率配置
    const filteredMultipliers: Record<string, number> = {}
    for (const format of form.value.api_formats) {
      if (form.value.rate_multipliers[format] !== undefined) {
        filteredMultipliers[format] = form.value.rate_multipliers[format]
      }
    }
    const rateMultipliersData = Object.keys(filteredMultipliers).length > 0
      ? filteredMultipliers
      : null

    // 准备认证相关数据
    const authConfig = parseAuthConfig()
    const authTypeByFormat = buildAuthTypeByFormatPayload()
    const allowAuthChannelMismatchFormats = buildAllowAuthChannelMismatchFormatsPayload()

    if (props.editingKey) {
      const shouldClearAllowedModels = !!props.editingKey.auto_fetch_models && !form.value.auto_fetch_models
      // 更新模式
      // 注意：rpm_limit 使用 null 表示自适应模式
      // undefined 表示"保持原值不变"（会在 JSON 序列化时被忽略）
      const updateData: EndpointAPIKeyUpdate = {
        api_formats: form.value.api_formats,
        name: form.value.name,
        auth_type: form.value.auth_type,
        auth_type_by_format: authTypeByFormat,
        allow_auth_channel_mismatch_formats: allowAuthChannelMismatchFormats,
        rate_multipliers: rateMultipliersData,
        internal_priority: form.value.internal_priority,
        rpm_limit: form.value.rpm_limit,
        concurrent_limit: form.value.concurrent_limit,
        cache_ttl_minutes: form.value.cache_ttl_minutes,
        max_probe_interval_minutes: form.value.max_probe_interval_minutes,
        note: form.value.note,
        is_active: form.value.is_active,
        allowed_models: shouldClearAllowedModels ? null : undefined,
        auto_fetch_models: form.value.auto_fetch_models,
        model_include_patterns: parsePatternText(form.value.model_include_patterns_text),
        model_exclude_patterns: parsePatternText(form.value.model_exclude_patterns_text)
      }

      // 根据认证类型设置对应字段
      if (isRawSecretAuthType(form.value.auth_type) && form.value.api_key.trim()) {
        updateData.api_key = form.value.api_key
      }
      if (form.value.auth_type === 'service_account' && authConfig) {
        updateData.auth_config = authConfig
      }

      await updateProviderKey(props.editingKey.id, updateData)
      success('密钥已更新', '成功')
    } else {
      // 新增模式
      const created = await addProviderKey(props.providerId, {
        api_formats: form.value.api_formats,
        api_key: form.value.api_key,
        auth_type: form.value.auth_type,
        auth_type_by_format: authTypeByFormat,
        allow_auth_channel_mismatch_formats: allowAuthChannelMismatchFormats,
        auth_config: authConfig || undefined,
        name: form.value.name,
        rate_multipliers: rateMultipliersData,
        internal_priority: form.value.internal_priority,
        rpm_limit: form.value.rpm_limit,
        concurrent_limit: form.value.concurrent_limit,
        cache_ttl_minutes: form.value.cache_ttl_minutes,
        max_probe_interval_minutes: form.value.max_probe_interval_minutes,
        note: form.value.note,
        auto_fetch_models: form.value.auto_fetch_models,
        model_include_patterns: parsePatternText(form.value.model_include_patterns_text),
        model_exclude_patterns: parsePatternText(form.value.model_exclude_patterns_text)
      })
      const hadPendingBalanceSave = !!pendingBalanceSaveQuery.value
      const balanceSaved = await persistPendingBalanceForCreatedKey(created.id)

      success(
        hadPendingBalanceSave && balanceSaved
          ? '密钥已添加，余额配置已保存'
          : '密钥已添加',
        '成功'
      )
      // 添加模式：不关闭对话框，只清除名称和密钥以便继续添加
      emit('saved')
      clearForNextAdd()
      return
    }

    emit('saved')
    emit('close')
  } catch (err: unknown) {
    const errorMessage = parseApiError(err, '保存密钥失败')
    showError(errorMessage, '错误')
  } finally {
    saving.value = false
  }
}
</script>
