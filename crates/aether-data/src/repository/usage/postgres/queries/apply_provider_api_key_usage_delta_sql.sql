UPDATE provider_api_keys
SET
  request_count = LEAST(GREATEST(COALESCE(request_count, 0) + $2, 0), $11::bigint),
  success_count = LEAST(GREATEST(COALESCE(success_count, 0) + $3, 0), $11::bigint),
  error_count = LEAST(GREATEST(COALESCE(error_count, 0) + $4, 0), $11::bigint),
  total_tokens = GREATEST(total_tokens + $5, 0),
  total_cost_usd = CAST(
    GREATEST(CAST(total_cost_usd AS DOUBLE PRECISION) + $6, 0) AS NUMERIC(20,8)
  ),
  total_response_time_ms = LEAST(
    GREATEST(COALESCE(total_response_time_ms, 0) + $7, 0),
    $10::bigint
  ),
  last_used_at = CASE
    WHEN $8::double precision IS NOT NULL THEN CASE
      WHEN last_used_at IS NULL THEN TO_TIMESTAMP($8::double precision)
      ELSE GREATEST(last_used_at, TO_TIMESTAMP($8::double precision))
    END
    WHEN $9::double precision IS NOT NULL
      AND last_used_at IS NOT NULL
      AND EXTRACT(EPOCH FROM last_used_at)::BIGINT = $9::BIGINT
    THEN (
      SELECT MAX(created_at)
      FROM "usage"
      WHERE provider_api_key_id = $1
        AND status NOT IN ('pending', 'streaming')
    )
    ELSE last_used_at
  END
WHERE id = $1
