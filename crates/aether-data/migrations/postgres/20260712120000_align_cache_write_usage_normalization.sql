-- Align canonical usage normalization with cache-write-aware GPT-5.6 billing.
-- OpenAI/Gemini input tokens include cache-write and cache-read tokens; Claude input does not.
CREATE OR REPLACE VIEW public.usage_billing_facts AS
SELECT
  usage_rows.id,
  usage_rows.request_id,
  usage_rows.user_id,
  usage_rows.api_key_id,
  usage_rows.username,
  usage_rows.api_key_name,
  usage_rows.provider_name,
  usage_rows.model,
  usage_rows.target_model,
  usage_rows.provider_id,
  usage_rows.provider_endpoint_id,
  usage_rows.provider_api_key_id,
  usage_rows.request_type,
  usage_rows.api_format,
  usage_rows.api_family,
  usage_rows.endpoint_kind,
  usage_rows.endpoint_api_format,
  usage_rows.provider_api_family,
  usage_rows.provider_endpoint_kind,
  COALESCE(usage_rows.has_format_conversion, FALSE) AS has_format_conversion,
  COALESCE(usage_rows.is_stream, FALSE) AS is_stream,
  usage_rows.status_code,
  usage_rows.error_message,
  usage_rows.error_category,
  usage_rows.response_time_ms,
  usage_rows.first_byte_time_ms,
  usage_rows.status,
  COALESCE(settlement.billing_status, usage_rows.billing_status) AS billing_status,
  usage_rows.created_at,
  COALESCE(settlement.finalized_at, usage_rows.finalized_at) AS finalized_at,
  resolved_tokens.input_tokens::bigint AS input_tokens,
  normalized_tokens.effective_input_tokens::bigint AS effective_input_tokens,
  resolved_tokens.output_tokens::bigint AS output_tokens,
  resolved_tokens.cache_creation_tokens::bigint AS cache_creation_input_tokens,
  resolved_tokens.cache_creation_5m_tokens::bigint AS cache_creation_input_tokens_5m,
  resolved_tokens.cache_creation_1h_tokens::bigint AS cache_creation_input_tokens_1h,
  resolved_tokens.cache_read_tokens::bigint AS cache_read_input_tokens,
  (
    normalized_tokens.effective_input_tokens
      + resolved_tokens.output_tokens
      + resolved_tokens.cache_creation_tokens
      + resolved_tokens.cache_read_tokens
  )::bigint AS total_tokens,
  (
    normalized_tokens.effective_input_tokens
      + resolved_tokens.cache_creation_tokens
      + resolved_tokens.cache_read_tokens
  )::bigint AS total_input_context,
  COALESCE(CAST(usage_rows.input_cost_usd AS DOUBLE PRECISION), 0) AS input_cost_usd,
  COALESCE(CAST(usage_rows.output_cost_usd AS DOUBLE PRECISION), 0) AS output_cost_usd,
  COALESCE(
    CAST(settlement.billing_cache_creation_cost_usd AS DOUBLE PRECISION),
    CAST(usage_rows.cache_creation_cost_usd AS DOUBLE PRECISION),
    0
  ) AS cache_creation_cost_usd,
  COALESCE(
    CAST(settlement.billing_cache_read_cost_usd AS DOUBLE PRECISION),
    CAST(usage_rows.cache_read_cost_usd AS DOUBLE PRECISION),
    0
  ) AS cache_read_cost_usd,
  COALESCE(
    CAST(settlement.billing_total_cost_usd AS DOUBLE PRECISION),
    CAST(usage_rows.total_cost_usd AS DOUBLE PRECISION),
    0
  ) AS total_cost_usd,
  COALESCE(
    CAST(settlement.billing_actual_total_cost_usd AS DOUBLE PRECISION),
    CAST(usage_rows.actual_total_cost_usd AS DOUBLE PRECISION),
    0
  ) AS actual_total_cost_usd,
  COALESCE(
    CAST(settlement.output_price_per_1m AS DOUBLE PRECISION),
    CAST(usage_rows.output_price_per_1m AS DOUBLE PRECISION)
  ) AS output_price_per_1m,
  COALESCE(
    CAST(settlement.input_price_per_1m AS DOUBLE PRECISION),
    CAST(usage_rows.input_price_per_1m AS DOUBLE PRECISION)
  ) AS input_price_per_1m,
  COALESCE(
    CAST(settlement.cache_creation_price_per_1m AS DOUBLE PRECISION),
    CAST(usage_rows.cache_creation_price_per_1m AS DOUBLE PRECISION)
  ) AS cache_creation_price_per_1m,
  COALESCE(
    CAST(settlement.cache_read_price_per_1m AS DOUBLE PRECISION),
    CAST(usage_rows.cache_read_price_per_1m AS DOUBLE PRECISION)
  ) AS cache_read_price_per_1m,
  COALESCE(
    CAST(settlement.price_per_request AS DOUBLE PRECISION),
    CAST(usage_rows.price_per_request AS DOUBLE PRECISION)
  ) AS price_per_request,
  settlement.billing_pricing_source,
  settlement.billing_rule_id,
  settlement.billing_rule_version,
  COALESCE(usage_rows.upstream_is_stream, COALESCE(usage_rows.is_stream, FALSE)) AS upstream_is_stream
FROM public."usage" AS usage_rows
LEFT JOIN public.usage_settlement_snapshots AS settlement
  ON settlement.request_id = usage_rows.request_id
CROSS JOIN LATERAL (
  SELECT
    GREATEST(COALESCE(settlement.billing_input_tokens, usage_rows.input_tokens, 0), 0) AS input_tokens,
    GREATEST(COALESCE(settlement.billing_output_tokens, usage_rows.output_tokens, 0), 0) AS output_tokens,
    GREATEST(
      COALESCE(
        settlement.billing_cache_creation_tokens,
        CASE
          WHEN COALESCE(usage_rows.cache_creation_input_tokens, 0) = 0
               AND (
                 COALESCE(usage_rows.cache_creation_input_tokens_5m, 0)
                 + COALESCE(usage_rows.cache_creation_input_tokens_1h, 0)
               ) > 0
          THEN COALESCE(usage_rows.cache_creation_input_tokens_5m, 0)
             + COALESCE(usage_rows.cache_creation_input_tokens_1h, 0)
          ELSE COALESCE(usage_rows.cache_creation_input_tokens, 0)
        END,
        0
      ),
      0
    ) AS cache_creation_tokens,
    GREATEST(
      COALESCE(
        settlement.billing_cache_creation_5m_tokens,
        usage_rows.cache_creation_input_tokens_5m,
        0
      ),
      0
    ) AS cache_creation_5m_tokens,
    GREATEST(
      COALESCE(
        settlement.billing_cache_creation_1h_tokens,
        usage_rows.cache_creation_input_tokens_1h,
        0
      ),
      0
    ) AS cache_creation_1h_tokens,
    GREATEST(
      COALESCE(settlement.billing_cache_read_tokens, usage_rows.cache_read_input_tokens, 0),
      0
    ) AS cache_read_tokens
) AS resolved_tokens
CROSS JOIN LATERAL (
  SELECT GREATEST(
    COALESCE(
      settlement.billing_effective_input_tokens,
      CASE
        WHEN settlement.billing_input_tokens IS NOT NULL
        THEN resolved_tokens.input_tokens
        WHEN split_part(
          lower(COALESCE(usage_rows.endpoint_api_format, usage_rows.api_format, '')),
          ':',
          1
        ) IN ('openai', 'gemini', 'google')
        THEN GREATEST(
          resolved_tokens.input_tokens
            - resolved_tokens.cache_creation_tokens
            - resolved_tokens.cache_read_tokens,
          0
        )
        ELSE resolved_tokens.input_tokens
      END
    ),
    0
  ) AS effective_input_tokens
) AS normalized_tokens;
COMMENT ON VIEW public.usage_billing_facts IS
  'Canonical billing read model. OpenAI/Gemini effective input excludes cache-write and cache-read tokens; total token fields add each dimension exactly once.';
