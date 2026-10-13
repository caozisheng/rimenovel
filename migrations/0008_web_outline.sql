-- Source-qualified web outline references.  Web outlines deliberately have no
-- source slices: their provenance is a verified external outline, not imported prose.
PRAGMA defer_foreign_keys=ON;

DROP TRIGGER IF EXISTS quick_read_source_insert;
DROP TRIGGER IF EXISTS quick_read_source_update;
DROP TRIGGER IF EXISTS quick_read_source_delete;

ALTER TABLE quick_read_slices RENAME TO quick_read_slices_0005;
ALTER TABLE quick_read_chapter_summaries RENAME TO quick_read_chapter_summaries_0005;
ALTER TABLE quick_read_revisions RENAME TO quick_read_revisions_0005;

DROP INDEX IF EXISTS idx_quick_read_active_revision;
DROP INDEX IF EXISTS idx_quick_read_revision_source;
DROP INDEX IF EXISTS idx_quick_read_slices_status;

CREATE TABLE quick_read_revisions (
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  source_digest TEXT NOT NULL,
  dependency_digest TEXT NOT NULL,
  provider_id INTEGER NOT NULL,
  model TEXT NOT NULL,
  prompt_version TEXT NOT NULL,
  schema_version TEXT NOT NULL,
  source_kind TEXT NOT NULL DEFAULT 'local_full_text' CHECK(source_kind IN ('local_full_text','web_outline')),
  total_chars INTEGER NOT NULL CHECK(total_chars>=0),
  total_chapters INTEGER NOT NULL CHECK(total_chapters>0),
  completed_chars INTEGER NOT NULL DEFAULT 0 CHECK(completed_chars>=0),
  completed_slices INTEGER NOT NULL DEFAULT 0 CHECK(completed_slices>=0),
  total_slices INTEGER NOT NULL CHECK(total_slices>=0),
  plot_summary TEXT,
  plot_notes_json TEXT NOT NULL DEFAULT '[]',
  characters_json TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL CHECK(status IN ('pending','running','failed','ready','stale')),
  active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  CHECK((source_kind='local_full_text' AND total_slices>0) OR
        (source_kind='web_outline' AND total_chars=0 AND completed_chars=0 AND total_slices=0 AND completed_slices=0))
);
INSERT INTO quick_read_revisions(id,book_id,source_digest,dependency_digest,provider_id,model,prompt_version,schema_version,source_kind,total_chars,total_chapters,completed_chars,completed_slices,total_slices,plot_summary,plot_notes_json,characters_json,status,active,created_at)
SELECT id,book_id,source_digest,dependency_digest,provider_id,model,prompt_version,schema_version,'local_full_text',total_chars,total_chapters,completed_chars,completed_slices,total_slices,plot_summary,plot_notes_json,characters_json,status,active,created_at
FROM quick_read_revisions_0005;

CREATE TABLE quick_read_slices (
  id INTEGER PRIMARY KEY,
  revision_id INTEGER NOT NULL REFERENCES quick_read_revisions(id) ON DELETE CASCADE,
  slice_idx INTEGER NOT NULL CHECK(slice_idx>=0),
  source_hash TEXT NOT NULL,
  start_chapter INTEGER NOT NULL CHECK(start_chapter>0),
  start_char INTEGER NOT NULL CHECK(start_char>=0),
  end_chapter INTEGER NOT NULL CHECK(end_chapter>0),
  end_char INTEGER NOT NULL CHECK(end_char>=0),
  spans_json TEXT NOT NULL,
  result_json TEXT,
  tokens_in INTEGER,
  tokens_out INTEGER,
  status TEXT NOT NULL CHECK(status IN ('pending','running','failed','ready')),
  error TEXT,
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(revision_id,slice_idx),
  CHECK(end_chapter>start_chapter OR (end_chapter=start_chapter AND end_char>start_char))
);
INSERT INTO quick_read_slices(id,revision_id,slice_idx,source_hash,start_chapter,start_char,end_chapter,end_char,spans_json,result_json,tokens_in,tokens_out,status,error,updated_at)
SELECT id,revision_id,slice_idx,source_hash,start_chapter,start_char,end_chapter,end_char,spans_json,result_json,tokens_in,tokens_out,status,error,updated_at
FROM quick_read_slices_0005;

CREATE TABLE quick_read_chapter_summaries (
  revision_id INTEGER NOT NULL REFERENCES quick_read_revisions(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL CHECK(chapter_idx>0),
  data_json TEXT NOT NULL,
  PRIMARY KEY(revision_id,chapter_idx)
);
INSERT INTO quick_read_chapter_summaries(revision_id,chapter_idx,data_json)
SELECT revision_id,chapter_idx,data_json FROM quick_read_chapter_summaries_0005;

DROP TABLE quick_read_slices_0005;
DROP TABLE quick_read_chapter_summaries_0005;
DROP TABLE quick_read_revisions_0005;

CREATE UNIQUE INDEX idx_quick_read_active_revision
  ON quick_read_revisions(book_id) WHERE active=1;
CREATE INDEX idx_quick_read_revision_source
  ON quick_read_revisions(book_id,source_digest,dependency_digest);
CREATE INDEX idx_quick_read_slices_status
  ON quick_read_slices(revision_id,status,slice_idx);

CREATE TABLE quick_read_web_outlines (
  revision_id INTEGER PRIMARY KEY REFERENCES quick_read_revisions(id) ON DELETE CASCADE,
  normalized_book_title TEXT NOT NULL,
  match_score REAL NOT NULL CHECK(match_score>=0 AND match_score<=1),
  provenance_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE quick_read_web_outline_chapters (
  revision_id INTEGER NOT NULL REFERENCES quick_read_revisions(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL CHECK(chapter_idx>0),
  outline_heading TEXT NOT NULL,
  source_url TEXT NOT NULL,
  PRIMARY KEY(revision_id,chapter_idx)
);
CREATE INDEX idx_quick_read_web_outline_chapters_revision
  ON quick_read_web_outline_chapters(revision_id,chapter_idx);

CREATE TRIGGER quick_read_source_insert
AFTER INSERT ON chapters
BEGIN
  UPDATE quick_read_revisions SET status='stale',active=0 WHERE book_id=NEW.book_id AND status IN ('pending','running','failed','ready');
END;
CREATE TRIGGER quick_read_source_update
AFTER UPDATE OF original_text,title,idx ON chapters
WHEN OLD.original_text<>NEW.original_text OR OLD.title<>NEW.title OR OLD.idx<>NEW.idx
BEGIN
  UPDATE quick_read_revisions SET status='stale',active=0 WHERE book_id=NEW.book_id AND status IN ('pending','running','failed','ready');
END;
CREATE TRIGGER quick_read_source_delete
AFTER DELETE ON chapters
BEGIN
  UPDATE quick_read_revisions SET status='stale',active=0 WHERE book_id=OLD.book_id AND status IN ('pending','running','failed','ready');
END;
