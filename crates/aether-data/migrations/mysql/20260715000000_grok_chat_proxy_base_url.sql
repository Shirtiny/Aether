-- A Grok CLI subscription grant serves non-media chat from the CLI chat-proxy,
-- not from the official xAI API root that the initial OAuth migration wrote.
-- Only the official root is rewritten so a deliberate custom base URL survives.
UPDATE provider_endpoints e
JOIN providers p ON p.id = e.provider_id
SET e.base_url = 'https://cli-chat-proxy.grok.com/v1',
    e.updated_at = UNIX_TIMESTAMP()
WHERE LOWER(TRIM(COALESCE(p.provider_type, ''))) = 'grok'
  AND TRIM(TRAILING '/' FROM LOWER(TRIM(COALESCE(e.base_url, '')))) = 'https://api.x.ai/v1';
