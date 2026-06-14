CREATE TABLE IF NOT EXISTS public.usage_prompt_capture_entries (
    sha256 character varying(64) NOT NULL,
    role character varying(32) NOT NULL,
    chars integer NOT NULL,
    preview text NOT NULL,
    truncated boolean DEFAULT false NOT NULL,
    first_seen_at timestamp with time zone DEFAULT now() NOT NULL,
    last_seen_at timestamp with time zone DEFAULT now() NOT NULL,
    seen_count bigint DEFAULT 1 NOT NULL,
    CONSTRAINT usage_prompt_capture_entries_pkey PRIMARY KEY (sha256)
);

CREATE INDEX IF NOT EXISTS usage_prompt_capture_entries_last_seen_at_idx
    ON public.usage_prompt_capture_entries USING btree (last_seen_at);
