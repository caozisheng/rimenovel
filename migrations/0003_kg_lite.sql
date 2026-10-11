-- Old facts/evidence tables remain archival only. Text, read pins and intent IDs survive.
CREATE TABLE kg_lite_snapshots (
 id INTEGER PRIMARY KEY,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 text_version_id INTEGER NOT NULL REFERENCES kg_text_versions(id) ON DELETE CASCADE,
 digest TEXT NOT NULL,
 dependency_digest TEXT NOT NULL,
 config_digest TEXT,
 schema_digest TEXT NOT NULL,
 prior_digest TEXT NOT NULL DEFAULT '',
 data_json TEXT NOT NULL,
 legacy INTEGER NOT NULL DEFAULT 0,
 active INTEGER NOT NULL DEFAULT 1,
 created_at TEXT NOT NULL DEFAULT (datetime('now')),
 UNIQUE(text_version_id,digest,config_digest)
);
CREATE UNIQUE INDEX idx_lite_active_snapshot ON kg_lite_snapshots(text_version_id) WHERE active=1;
CREATE INDEX idx_lite_snapshot_book ON kg_lite_snapshots(book_id,text_version_id);
-- Finite, chapter-scoped mappings resolved once from archived revealed links.
-- The runtime reads only these converted IDs, never legacy identity records.
CREATE TABLE kg_lite_legacy_retirements (
 snapshot_id INTEGER NOT NULL REFERENCES kg_lite_snapshots(id) ON DELETE CASCADE,
 retired_key TEXT NOT NULL,
 replacement_key TEXT NOT NULL,
 PRIMARY KEY(snapshot_id,retired_key)
);
CREATE TABLE kg_lite_specs (
 id INTEGER PRIMARY KEY,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 chapter_idx INTEGER NOT NULL,
 dependency_digest TEXT NOT NULL,
 line_version INTEGER NOT NULL,
 data_json TEXT NOT NULL,
 legacy INTEGER NOT NULL DEFAULT 0,
 active INTEGER NOT NULL DEFAULT 1,
 UNIQUE(book_id,chapter_idx,dependency_digest)
);
CREATE UNIQUE INDEX idx_lite_active_spec ON kg_lite_specs(book_id,chapter_idx) WHERE active=1;
CREATE TABLE kg_extraction_plans (
 book_id INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
 selected_json TEXT NOT NULL,
 calls_used INTEGER NOT NULL DEFAULT 0,
 elapsed_ms INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE kg_lite_legacy_graphs (
 id INTEGER PRIMARY KEY,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 chapter_idx INTEGER NOT NULL DEFAULT 0,
 source TEXT NOT NULL,
 data_json TEXT NOT NULL
);
-- Retain the original serialized overlays alongside their locally converted records.
INSERT INTO kg_legacy_archive(book_id,source_table,source_id,data_json,reason)
 SELECT book_id,'edits_log',id,json_object('before_json',before_json,'after_json',after_json),'lite-local-conversion' FROM edits_log;
