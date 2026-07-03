-- no-transaction

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_usage_client_family_created_id
    ON public.usage USING btree (
        (LOWER(BTRIM(COALESCE(
            request_metadata#>>'{client_session_affinity,client_family}',
            request_metadata->>'client_family',
            ''
        )))),
        created_at DESC,
        id ASC
    );
