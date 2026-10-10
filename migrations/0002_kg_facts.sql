-- Authoritative immutable text, extraction revisions and evidence. Existing graph
-- tables remain projections/intents; evidence-less legacy AI is archived, not trusted.
CREATE TABLE kg_lines (
 book_id INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
 progress INTEGER NOT NULL DEFAULT 0,
 version INTEGER NOT NULL DEFAULT 0,
 revealed_idx INTEGER
);
INSERT INTO kg_lines(book_id) SELECT id FROM books;
ALTER TABLE edits_log ADD COLUMN active INTEGER NOT NULL DEFAULT 1;
ALTER TABLE edits_log ADD COLUMN scope TEXT NOT NULL DEFAULT 'global';
ALTER TABLE edits_log ADD COLUMN chapter_idx INTEGER;
ALTER TABLE edits_log ADD COLUMN reverted_at TEXT;
ALTER TABLE generated_chapters ADD COLUMN line_version INTEGER NOT NULL DEFAULT 0;
CREATE TABLE kg_text_versions (
 id INTEGER PRIMARY KEY,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 chapter_idx INTEGER NOT NULL,
 source TEXT NOT NULL CHECK(source IN ('original','generated')),
 content TEXT NOT NULL,
 content_hash TEXT NOT NULL,
 generated_id INTEGER REFERENCES generated_chapters(id) ON DELETE CASCADE,
 CHECK((source='original' AND generated_id IS NULL) OR (source='generated' AND generated_id IS NOT NULL)),
 UNIQUE(generated_id)
);
CREATE TABLE kg_read_selection (
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 chapter_idx INTEGER NOT NULL,
 text_version_id INTEGER NOT NULL REFERENCES kg_text_versions(id) ON DELETE CASCADE,
 PRIMARY KEY(book_id,chapter_idx)
);
CREATE UNIQUE INDEX idx_kg_original_text ON kg_text_versions(book_id,chapter_idx) WHERE source='original';
CREATE TRIGGER kg_immutable_text BEFORE UPDATE ON kg_text_versions BEGIN SELECT RAISE(ABORT,'immutable text version'); END;
CREATE TRIGGER kg_immutable_generated BEFORE UPDATE OF content,book_id,chapter_idx,prompt_digest,model ON generated_chapters BEGIN SELECT RAISE(ABORT,'immutable generated text'); END;
CREATE TRIGGER kg_immutable_original BEFORE UPDATE OF original_text,book_id,idx ON chapters BEGIN SELECT RAISE(ABORT,'immutable original text'); END;
CREATE TABLE kg_revisions (
 id INTEGER PRIMARY KEY,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 text_version_id INTEGER NOT NULL REFERENCES kg_text_versions(id) ON DELETE CASCADE,
 digest TEXT NOT NULL,
 dependency_digest TEXT NOT NULL,
 stage_a_version TEXT NOT NULL,
 stage_b_version TEXT NOT NULL,
 summary TEXT NOT NULL,
 open_threads_json TEXT NOT NULL,
 unresolved_json TEXT NOT NULL,
 complete INTEGER NOT NULL DEFAULT 1,
 active INTEGER NOT NULL DEFAULT 0,
 config_digest TEXT,
 schema_digest TEXT NOT NULL,
 prior_digest TEXT NOT NULL DEFAULT '',
 created_at TEXT NOT NULL DEFAULT (datetime('now')),
 UNIQUE(text_version_id,digest)
);
CREATE UNIQUE INDEX idx_kg_active_revision ON kg_revisions(text_version_id) WHERE active=1;
CREATE TABLE kg_entities (
 id INTEGER PRIMARY KEY,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 key TEXT NOT NULL,
 entity_type TEXT NOT NULL,
 UNIQUE(book_id,key)
);
CREATE TABLE kg_spans (
 id INTEGER PRIMARY KEY,
 revision_id INTEGER NOT NULL REFERENCES kg_revisions(id) ON DELETE CASCADE,
 key TEXT NOT NULL,
 chapter_idx INTEGER NOT NULL,
 start_byte INTEGER NOT NULL,
 end_byte INTEGER NOT NULL,
 text TEXT NOT NULL,
 UNIQUE(revision_id,key)
);
CREATE TABLE kg_records (
 id INTEGER PRIMARY KEY,
 revision_id INTEGER NOT NULL REFERENCES kg_revisions(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('identity','mention','fact','relation','change','link','precondition')),
 key TEXT NOT NULL,
 subject_key TEXT,
 observed_chapter INTEGER NOT NULL,
 observed_order INTEGER NOT NULL,
 data_json TEXT NOT NULL,
 UNIQUE(revision_id,kind,key)
);
CREATE INDEX idx_kg_records_position ON kg_records(revision_id,observed_chapter,observed_order);
CREATE INDEX idx_kg_records_subject ON kg_records(subject_key,revision_id);
ALTER TABLE graph_events ADD COLUMN revision_id INTEGER REFERENCES kg_revisions(id) ON DELETE CASCADE;
ALTER TABLE graph_events ADD COLUMN record_key TEXT;
ALTER TABLE graph_events ADD COLUMN observed_chapter INTEGER;
ALTER TABLE graph_events ADD COLUMN observed_order INTEGER;
ALTER TABLE graph_events ADD COLUMN data_json TEXT;
CREATE UNIQUE INDEX idx_kg_events_revision ON graph_events(revision_id,record_key) WHERE revision_id IS NOT NULL;
CREATE TABLE kg_specs (
 id INTEGER PRIMARY KEY,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 chapter_idx INTEGER NOT NULL,
 dependency_digest TEXT NOT NULL,
 line_version INTEGER NOT NULL,
 data_json TEXT NOT NULL,
 preconditions_json TEXT NOT NULL DEFAULT '[]',
 active INTEGER NOT NULL DEFAULT 1,
 UNIQUE(book_id,chapter_idx,dependency_digest)
);
CREATE UNIQUE INDEX idx_kg_active_spec ON kg_specs(book_id,chapter_idx) WHERE active=1;
CREATE TABLE kg_actual_drift (
 text_version_id INTEGER PRIMARY KEY REFERENCES kg_text_versions(id) ON DELETE CASCADE,
 diff REAL NOT NULL CHECK(diff>=0 AND diff<=1)
);
CREATE TABLE kg_legacy_archive (
 id INTEGER PRIMARY KEY,
 book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
 source_table TEXT NOT NULL,
 source_id INTEGER NOT NULL,
 data_json TEXT NOT NULL,
 reason TEXT NOT NULL DEFAULT 'needs-reextract'
);
INSERT INTO kg_legacy_archive(book_id,source_table,source_id,data_json)
 SELECT book_id,'graph_nodes',id,json_object('key',key,'type',type,'name',name,'aliases_json',aliases_json,'summary',summary,'provenance',provenance) FROM graph_nodes WHERE provenance!='user';
INSERT INTO kg_legacy_archive(book_id,source_table,source_id,data_json)
 SELECT book_id,'graph_edges',id,json_object('src',src_key,'dst',dst_key,'rel_type',rel_type,'status',status,'description',description,'provenance',provenance) FROM graph_edges WHERE provenance!='user';
INSERT INTO kg_legacy_archive(book_id,source_table,source_id,data_json)
 SELECT book_id,'graph_events',id,json_object('title',title,'participants',participant_keys_json,'summary',summary,'outcome',outcome,'provenance',provenance) FROM graph_events WHERE provenance!='user';
DELETE FROM graph_nodes WHERE provenance!='user';
DELETE FROM graph_edges WHERE provenance!='user';
DELETE FROM graph_events WHERE provenance!='user';
UPDATE generated_chapters SET active=0;
UPDATE chapter_graph_meta SET adapted=0,drift_score=NULL,adapted_at=NULL;
