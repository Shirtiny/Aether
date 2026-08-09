export interface ProviderKeyAuthCarrier {
  provider_type?: string | null
  auth_type?: string | null
  credential_kind?: string | null
  runtime_auth_kind?: string | null
  oauth_managed?: boolean | null
  oauth_temporary?: boolean | null
  can_refresh_oauth?: boolean | null
  can_export_oauth?: boolean | null
  can_edit_oauth?: boolean | null
}

function normalizeText(value: unknown): string | null {
  if (typeof value !== 'string') return null
  const text = value.trim().toLowerCase()
  return text || null
}

function resolveProviderType(input: ProviderKeyAuthCarrier, providerType?: string | null): string | null {
  return normalizeText(providerType) ?? normalizeText(input.provider_type)
}

function isGrokSessionCredential(input: ProviderKeyAuthCarrier, providerType?: string | null): boolean {
  // Legacy Grok browser-session credentials were also represented as
  // `auth_type=oauth`, but they have no refresh token. Do not classify every
  // Grok OAuth credential as a Session Cookie: the official xAI OAuth path is
  // refreshable and must expose the normal OAuth controls.
  return resolveProviderType(input, providerType) === 'grok'
    && isOAuthManagedCredential(input)
    && input.oauth_temporary !== true
    && input.can_refresh_oauth === false
}

export function getProviderCredentialKind(
  input: ProviderKeyAuthCarrier,
): 'raw_secret' | 'oauth_session' | 'service_account' {
  const credentialKind = normalizeText(input.credential_kind)
  if (
    credentialKind === 'raw_secret'
    || credentialKind === 'oauth_session'
    || credentialKind === 'service_account'
  ) {
    return credentialKind
  }

  if (typeof input.oauth_managed === 'boolean') {
    return input.oauth_managed ? 'oauth_session' : 'raw_secret'
  }

  const authType = normalizeText(input.auth_type)
  if (authType === 'oauth') return 'oauth_session'
  if (authType === 'service_account' || authType === 'vertex_ai') return 'service_account'
  return 'raw_secret'
}

export function getProviderRuntimeAuthKind(
  input: ProviderKeyAuthCarrier,
): 'api_key' | 'bearer' | 'service_account' | 'mixed' | 'unknown' {
  const runtimeAuthKind = normalizeText(input.runtime_auth_kind)
  if (
    runtimeAuthKind === 'api_key'
    || runtimeAuthKind === 'bearer'
    || runtimeAuthKind === 'service_account'
    || runtimeAuthKind === 'mixed'
  ) {
    return runtimeAuthKind
  }

  const authType = normalizeText(input.auth_type)
  if (authType === 'service_account' || authType === 'vertex_ai') return 'service_account'
  if (authType === 'bearer') return 'bearer'
  if (authType === 'api_key') return 'api_key'
  return 'unknown'
}

export function isOAuthManagedCredential(input: ProviderKeyAuthCarrier): boolean {
  if (typeof input.oauth_managed === 'boolean') {
    return input.oauth_managed
  }
  return getProviderCredentialKind(input) === 'oauth_session'
}

export function isServiceAccountCredential(input: ProviderKeyAuthCarrier): boolean {
  return getProviderCredentialKind(input) === 'service_account'
}

export function canRefreshOAuthCredential(input: ProviderKeyAuthCarrier): boolean {
  if (input.oauth_temporary === true) {
    return false
  }
  if (typeof input.can_refresh_oauth === 'boolean') {
    return input.can_refresh_oauth
  }
  return isOAuthManagedCredential(input)
}

export function shouldShowOAuthRefreshControl(
  input: ProviderKeyAuthCarrier,
  providerType?: string | null,
): boolean {
  // Refreshability is provider/backend metadata. In particular, Grok OAuth
  // accounts with a stored refresh token are refreshable just like Codex.
  // Keep the provider argument for API compatibility with existing callers.
  void providerType
  return canRefreshOAuthCredential(input)
}

export function canExportOAuthCredential(input: ProviderKeyAuthCarrier): boolean {
  if (typeof input.can_export_oauth === 'boolean') {
    return input.can_export_oauth
  }
  return isOAuthManagedCredential(input)
}

export function canEditOAuthCredential(input: ProviderKeyAuthCarrier): boolean {
  if (typeof input.can_edit_oauth === 'boolean') {
    return input.can_edit_oauth
  }
  return isOAuthManagedCredential(input)
}

export function getProviderAuthLabel(input: ProviderKeyAuthCarrier): string {
  if (isOAuthManagedCredential(input)) return 'OAuth'
  if (isServiceAccountCredential(input)) return '服务账号'
  if (getProviderRuntimeAuthKind(input) === 'mixed') return '混合'
  return getProviderRuntimeAuthKind(input) === 'bearer' ? 'Bearer' : 'API Key'
}

export function getProviderMaskedSecretLabel(
  input: ProviderKeyAuthCarrier,
  providerType?: string | null,
): string {
  if (isGrokSessionCredential(input, providerType)) return '[Session Cookie]'
  if (isOAuthManagedCredential(input)) return '[OAuth Token]'
  if (isServiceAccountCredential(input)) return '[Service Account]'
  if (getProviderRuntimeAuthKind(input) === 'mixed') return '[Key]'
  return getProviderRuntimeAuthKind(input) === 'bearer' ? '[Bearer Token]' : '[Key]'
}
