-- A Grok CLI subscription grant serves non-media chat from the CLI chat-proxy,
-- not from the official xAI API root that the initial OAuth migration wrote.
-- Only the official root is rewritten so a deliberate custom base URL survives.
UPDATE public.provider_endpoints e
SET base_url = 'https://cli-chat-proxy.grok.com/v1',
    updated_at = now()
FROM public.providers p
WHERE p.id = e.provider_id
  AND lower(trim(coalesce(p.provider_type, ''))) = 'grok'
  AND rtrim(lower(trim(coalesce(e.base_url, ''))), '/') = 'https://api.x.ai/v1';
