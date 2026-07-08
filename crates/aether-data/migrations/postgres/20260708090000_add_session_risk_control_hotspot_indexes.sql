-- no-transaction

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_usage_provider_risk_client_session_created
    ON public.usage USING btree (
        provider_id,
        (LOWER(COALESCE(request_metadata#>>'{client_session_affinity,session_key}', ''))),
        created_at DESC,
        id ASC
    )
    WHERE (request_metadata->>'is_risk_control') = 'true';

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_usage_provider_risk_session_id_created
    ON public.usage USING btree (
        provider_id,
        (LOWER(COALESCE(request_metadata->>'session_id', ''))),
        created_at DESC,
        id ASC
    )
    WHERE (request_metadata->>'is_risk_control') = 'true';

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_usage_provider_risk_conversation_id_created
    ON public.usage USING btree (
        provider_id,
        (LOWER(COALESCE(request_metadata->>'conversation_id', ''))),
        created_at DESC,
        id ASC
    )
    WHERE (request_metadata->>'is_risk_control') = 'true';

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_request_candidates_provider_session_risk_created
    ON public.request_candidates USING btree (
        provider_id,
        ((extra_data#>>'{client_session_affinity,session_key}')),
        status,
        created_at DESC
    )
    WHERE status IN ('failed', 'cancelled');
