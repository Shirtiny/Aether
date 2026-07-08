-- no-transaction

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_request_candidates_provider_session_risk_created
    ON public.request_candidates USING btree (
        provider_id,
        ((extra_data#>>'{client_session_affinity,session_key}')),
        status,
        created_at DESC
    )
    WHERE status IN ('failed', 'cancelled');
