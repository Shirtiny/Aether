UPDATE "usage"
SET request_metadata = (
    COALESCE(request_metadata::jsonb, '{}'::jsonb)
    || jsonb_build_object('is_risk_control', true)
)::json
WHERE COALESCE(request_metadata::jsonb->>'is_risk_control', 'false') <> 'true'
  AND (
    LOWER(COALESCE(error_message, '')) LIKE '%flagged for possible cybersecurity risk%'
    OR LOWER(COALESCE(error_message, '')) LIKE '%possible cybersecurity risk%'
    OR LOWER(COALESCE(error_message, '')) LIKE '%trusted access for cyber%'
    OR LOWER(COALESCE(error_message, '')) LIKE '%chatgpt.com/cyber%'
    OR LOWER(COALESCE(client_response_body::text, '')) LIKE '%flagged for possible cybersecurity risk%'
    OR LOWER(COALESCE(client_response_body::text, '')) LIKE '%possible cybersecurity risk%'
    OR LOWER(COALESCE(client_response_body::text, '')) LIKE '%trusted access for cyber%'
    OR LOWER(COALESCE(client_response_body::text, '')) LIKE '%chatgpt.com/cyber%'
    OR LOWER(COALESCE(response_body::text, '')) LIKE '%flagged for possible cybersecurity risk%'
    OR LOWER(COALESCE(response_body::text, '')) LIKE '%possible cybersecurity risk%'
    OR LOWER(COALESCE(response_body::text, '')) LIKE '%trusted access for cyber%'
    OR LOWER(COALESCE(response_body::text, '')) LIKE '%chatgpt.com/cyber%'
  );
