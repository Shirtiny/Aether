-- no-transaction

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_usage_cafecode_uname_created_id
    ON public.usage USING btree (
        (LOWER(BTRIM(COALESCE(request_metadata->>'cafecode_uname', '')))),
        created_at DESC,
        id ASC
    );
