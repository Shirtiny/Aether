-- Grok is now an official xAI OAuth provider. Existing Grok credentials are
-- browser-session cookies and cannot be converted into refreshable OAuth grants.
UPDATE public.provider_endpoints e
SET base_url = 'https://api.x.ai/v1',
    custom_path = NULL,
    is_active = CASE
        WHEN lower(trim(coalesce(e.api_format, ''))) IN ('openai:chat', 'openai:responses')
            THEN e.is_active
        ELSE false
    END,
    updated_at = now()
FROM public.providers p
WHERE p.id = e.provider_id
  AND lower(trim(coalesce(p.provider_type, ''))) = 'grok';

UPDATE public.provider_api_keys k
SET is_active = false,
    status = 'oauth_reauthentication_required',
    oauth_invalid_at = now(),
    oauth_invalid_reason = 'Grok 已迁移至官方 xAI OAuth，请重新授权账号',
    fingerprint = NULL,
    updated_at = now()
FROM public.providers p
WHERE p.id = k.provider_id
  AND lower(trim(coalesce(p.provider_type, ''))) = 'grok';

UPDATE public.provider_api_keys k
SET concurrent_limit = 1,
    updated_at = now()
FROM public.providers p
WHERE p.id = k.provider_id
  AND lower(trim(coalesce(p.provider_type, ''))) = 'grok'
  AND lower(trim(coalesce(k.auth_type, ''))) = 'oauth';
