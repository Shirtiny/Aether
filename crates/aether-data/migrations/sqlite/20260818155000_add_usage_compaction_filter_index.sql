CREATE INDEX IF NOT EXISTS idx_usage_compaction_created_id
    ON usage (created_at_unix_ms DESC, id ASC)
    WHERE json_extract(request_metadata, '$.is_compaction') = 1;
