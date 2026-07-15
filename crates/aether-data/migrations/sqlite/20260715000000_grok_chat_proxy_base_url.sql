-- A Grok CLI subscription grant serves non-media chat from the CLI chat-proxy,
-- not from the official xAI API root that the initial OAuth migration wrote.
-- Only the official root is rewritten so a deliberate custom base URL survives.
UPDATE provider_endpoints
SET base_url = 'https://cli-chat-proxy.grok.com/v1',
    updated_at = CAST(strftime('%s', 'now') AS INTEGER)
WHERE provider_id IN (
    SELECT id FROM providers WHERE lower(trim(coalesce(provider_type, ''))) = 'grok'
)
  AND rtrim(lower(trim(coalesce(base_url, ''))), '/') = 'https://api.x.ai/v1';
