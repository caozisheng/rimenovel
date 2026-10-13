-- Persist the producing engine without changing revision or chapter-map keys.
-- Existing outlines were produced by Tavily; the default backfills those rows.
-- No table rebuild or external foreign-key changes are required. Db::migrate
-- applies this ALTER once using user_version, so repeated migration is harmless.
ALTER TABLE quick_read_web_outlines ADD COLUMN search_engine TEXT NOT NULL DEFAULT 'tavily';
