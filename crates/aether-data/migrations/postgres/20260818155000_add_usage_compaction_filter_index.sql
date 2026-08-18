-- no-transaction

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_usage_compaction_created_id
    ON public.usage USING btree (created_at DESC, id ASC)
    WHERE (request_metadata->>'is_compaction') = 'true';
