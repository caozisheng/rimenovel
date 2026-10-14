-- Full-book quick-read reference and resumable slice artifacts. Job rows use kind='quick_read'.
CREATE TABLE IF NOT EXISTS quick_read_revisions (
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  source_digest TEXT NOT NULL,
  dependency_digest TEXT NOT NULL,
  provider_id INTEGER NOT NULL,
  model TEXT NOT NULL,
  prompt_version TEXT NOT NULL,
  schema_version TEXT NOT NULL,
  total_chars INTEGER NOT NULL CHECK(total_chars>=0),
  total_chapters INTEGER NOT NULL CHECK(total_chapters>0),
  completed_chars INTEGER NOT NULL DEFAULT 0 CHECK(completed_chars>=0),
  completed_slices INTEGER NOT NULL DEFAULT 0 CHECK(completed_slices>=0),
  total_slices INTEGER NOT NULL CHECK(total_slices>0),
  plot_summary TEXT,
  plot_notes_json TEXT NOT NULL DEFAULT '[]',
  characters_json TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL CHECK(status IN ('pending','running','failed','ready','stale')),
  active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_quick_read_active_revision
  ON quick_read_revisions(book_id) WHERE active=1;
CREATE INDEX IF NOT EXISTS idx_quick_read_revision_source
  ON quick_read_revisions(book_id,source_digest,dependency_digest);

CREATE TABLE IF NOT EXISTS quick_read_slices (
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
CREATE INDEX IF NOT EXISTS idx_quick_read_slices_status
  ON quick_read_slices(revision_id,status,slice_idx);

CREATE TABLE IF NOT EXISTS quick_read_chapter_summaries (
  revision_id INTEGER NOT NULL REFERENCES quick_read_revisions(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL CHECK(chapter_idx>0),
  data_json TEXT NOT NULL,
  PRIMARY KEY(revision_id,chapter_idx)
);

CREATE TRIGGER IF NOT EXISTS quick_read_source_insert
AFTER INSERT ON chapters
BEGIN
  UPDATE quick_read_revisions SET status='stale',active=0 WHERE book_id=NEW.book_id AND status IN ('pending','running','failed','ready');
END;
CREATE TRIGGER IF NOT EXISTS quick_read_source_update
AFTER UPDATE OF original_text,title,idx ON chapters
WHEN OLD.original_text<>NEW.original_text OR OLD.title<>NEW.title OR OLD.idx<>NEW.idx
BEGIN
  UPDATE quick_read_revisions SET status='stale',active=0 WHERE book_id=NEW.book_id AND status IN ('pending','running','failed','ready');
END;
CREATE TRIGGER IF NOT EXISTS quick_read_source_delete
AFTER DELETE ON chapters
BEGIN
  UPDATE quick_read_revisions SET status='stale',active=0 WHERE book_id=OLD.book_id AND status IN ('pending','running','failed','ready');
END;
