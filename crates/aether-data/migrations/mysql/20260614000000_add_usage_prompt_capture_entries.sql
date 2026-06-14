CREATE TABLE IF NOT EXISTS usage_prompt_capture_entries (
    sha256 VARCHAR(64) NOT NULL,
    role VARCHAR(32) NOT NULL,
    chars INT NOT NULL,
    preview TEXT NOT NULL,
    truncated BOOLEAN NOT NULL DEFAULT FALSE,
    first_seen_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_seen_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    seen_count BIGINT NOT NULL DEFAULT 1,
    PRIMARY KEY (sha256),
    KEY usage_prompt_capture_entries_last_seen_at_idx (last_seen_at)
);
