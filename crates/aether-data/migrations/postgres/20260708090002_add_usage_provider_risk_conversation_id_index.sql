-- no-transaction

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_usage_provider_risk_conversation_id_created
    ON public.usage USING btree (
        provider_id,
        (LOWER(COALESCE(request_metadata->>'conversation_id', ''))),
        created_at DESC,
        id ASC
    )
    WHERE (request_metadata->>'is_risk_control') = 'true';
