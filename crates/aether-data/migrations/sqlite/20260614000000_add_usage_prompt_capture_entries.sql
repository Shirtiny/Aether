CREATE TABLE IF NOT EXISTS usage_prompt_capture_entries (
    sha256 TEXT NOT NULL PRIMARY KEY,
    role TEXT NOT NULL,
    chars INTEGER NOT NULL,
    preview TEXT NOT NULL,
    truncated INTEGER NOT NULL DEFAULT 0,
    first_seen_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_seen_at INTEGER NOT NULL DEFAULT (unixepoch()),
    seen_count INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS usage_prompt_capture_entries_last_seen_at_idx
    ON usage_prompt_capture_entries (last_seen_at);
