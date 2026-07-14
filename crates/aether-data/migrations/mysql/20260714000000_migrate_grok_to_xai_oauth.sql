-- Grok is now an official xAI OAuth provider. Existing Grok credentials are
-- browser-session cookies and cannot be converted into refreshable OAuth grants.
UPDATE provider_endpoints e
JOIN providers p ON p.id = e.provider_id
SET e.base_url = 'https://api.x.ai/v1',
    e.custom_path = NULL,
    e.is_active = CASE
        WHEN LOWER(TRIM(COALESCE(e.api_format, ''))) IN ('openai:chat', 'openai:responses')
            THEN e.is_active
        ELSE 0
    END,
    e.updated_at = UNIX_TIMESTAMP()
WHERE LOWER(TRIM(COALESCE(p.provider_type, ''))) = 'grok';

UPDATE provider_api_keys k
JOIN providers p ON p.id = k.provider_id
SET k.is_active = 0,
    k.status = 'oauth_reauthentication_required',
    k.oauth_invalid_at = UNIX_TIMESTAMP(),
    k.oauth_invalid_reason = 'Grok 已迁移至官方 xAI OAuth，请重新授权账号',
    k.fingerprint = NULL,
    k.updated_at = UNIX_TIMESTAMP()
WHERE LOWER(TRIM(COALESCE(p.provider_type, ''))) = 'grok';

UPDATE provider_api_keys k
JOIN providers p ON p.id = k.provider_id
SET k.concurrent_limit = 1,
    k.updated_at = UNIX_TIMESTAMP()
WHERE LOWER(TRIM(COALESCE(p.provider_type, ''))) = 'grok'
  AND LOWER(TRIM(COALESCE(k.auth_type, ''))) = 'oauth';
