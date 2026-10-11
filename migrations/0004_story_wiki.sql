-- Read-only narrative Wiki revisions and staged questionnaire interventions.
CREATE TABLE IF NOT EXISTS story_chapter_revisions (
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  text_version_id INTEGER NOT NULL REFERENCES kg_text_versions(id) ON DELETE CASCADE,
  dependency_digest TEXT NOT NULL,
  data_json TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('pending','ready','failed','stale')),
  active INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_story_active_chapter_revision
  ON story_chapter_revisions(text_version_id) WHERE active=1;

CREATE TABLE IF NOT EXISTS story_wiki_entries (
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  entry_kind TEXT NOT NULL,
  stable_key TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(book_id, stable_key)
);
CREATE TABLE IF NOT EXISTS story_wiki_observations (
  id INTEGER PRIMARY KEY,
  chapter_revision_id INTEGER NOT NULL REFERENCES story_chapter_revisions(id) ON DELETE CASCADE,
  entry_id INTEGER NOT NULL REFERENCES story_wiki_entries(id) ON DELETE CASCADE,
  data_json TEXT NOT NULL,
  source_chapter INTEGER NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_story_wiki_observations_entry
  ON story_wiki_observations(entry_id, source_chapter);

CREATE TABLE IF NOT EXISTS story_interventions (
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL,
  batch_index INTEGER,
  question_batch_id TEXT,
  selected_json TEXT NOT NULL DEFAULT '[]',
  free_text TEXT,
  overridden_json TEXT NOT NULL DEFAULT '[]',
  state TEXT NOT NULL CHECK(state IN ('active','ended','cancelled','stale')) DEFAULT 'active',
  effective_from INTEGER NOT NULL,
  reading_line_version INTEGER NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_story_interventions_book_chapter
  ON story_interventions(book_id, chapter_idx, state);

CREATE TABLE IF NOT EXISTS story_next_wiki_revisions (
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL,
  history_revision INTEGER NOT NULL,
  previous_text_version_id INTEGER NOT NULL REFERENCES kg_text_versions(id) ON DELETE CASCADE,
  dependency_digest TEXT NOT NULL,
  data_json TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('pending','ready','stale','failed')),
  active INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_story_active_next_wiki
  ON story_next_wiki_revisions(book_id, chapter_idx) WHERE active=1;

CREATE TABLE IF NOT EXISTS story_next_wiki_reveals (
  id INTEGER PRIMARY KEY,
  plan_revision_id INTEGER NOT NULL REFERENCES story_next_wiki_revisions(id) ON DELETE CASCADE,
  reading_line_version INTEGER NOT NULL,
  revealed_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(plan_revision_id, reading_line_version)
);

CREATE TABLE IF NOT EXISTS story_question_batches (
  id INTEGER PRIMARY KEY,
  book_id INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_idx INTEGER NOT NULL,
  batch_index INTEGER NOT NULL,
  question_batch_id TEXT NOT NULL,
  data_json TEXT NOT NULL,
  reading_line_version INTEGER NOT NULL,
  active INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(book_id, chapter_idx, question_batch_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_story_active_question_batch
  ON story_question_batches(book_id, chapter_idx, batch_index) WHERE active=1;
