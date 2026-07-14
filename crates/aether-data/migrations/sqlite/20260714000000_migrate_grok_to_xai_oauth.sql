-- Grok is now an official xAI OAuth provider. Existing Grok credentials are
-- browser-session cookies and cannot be converted into refreshable OAuth grants.
UPDATE provider_endpoints
SET base_url = 'https://api.x.ai/v1',
    custom_path = NULL,
    is_active = CASE
        WHEN lower(trim(coalesce(api_format, ''))) IN ('openai:chat', 'openai:responses')
            THEN is_active
        ELSE 0
    END,
    updated_at = CAST(strftime('%s', 'now') AS INTEGER)
WHERE provider_id IN (
    SELECT id FROM providers WHERE lower(trim(coalesce(provider_type, ''))) = 'grok'
);

UPDATE provider_api_keys
SET is_active = 0,
    status = 'oauth_reauthentication_required',
    oauth_invalid_at = CAST(strftime('%s', 'now') AS INTEGER),
    oauth_invalid_reason = 'Grok 已迁移至官方 xAI OAuth，请重新授权账号',
    fingerprint = NULL,
    updated_at = CAST(strftime('%s', 'now') AS INTEGER)
WHERE provider_id IN (
    SELECT id FROM providers WHERE lower(trim(coalesce(provider_type, ''))) = 'grok'
);

UPDATE provider_api_keys
SET concurrent_limit = 1,
    updated_at = CAST(strftime('%s', 'now') AS INTEGER)
WHERE provider_id IN (
    SELECT id FROM providers WHERE lower(trim(coalesce(provider_type, ''))) = 'grok'
)
  AND lower(trim(coalesce(auth_type, ''))) = 'oauth';
