-- RimeNovel 初始 schema（设计文档 §3.1，13 张表）
-- 迁移运行器按文件名序执行；user_version 随之推进

CREATE TABLE IF NOT EXISTS books (
  id          INTEGER PRIMARY KEY,
  title       TEXT NOT NULL,
  author      TEXT,
  source_path TEXT NOT NULL,
  format      TEXT NOT NULL,             -- txt|epub
  style_guide TEXT,                       -- json: narrative_person/tone/pacing/signature
  synopsis    TEXT,
  meta_json   TEXT,
  created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS chapters (
  id            INTEGER PRIMARY KEY,
  book_id       INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  idx           INTEGER NOT NULL,         -- 章序号从 1 起
  title         TEXT NOT NULL,
  original_text TEXT NOT NULL,            -- 原文永不删改
  token_est     INTEGER NOT NULL DEFAULT 0,
  UNIQUE(book_id, idx)
);
CREATE INDEX IF NOT EXISTS idx_chapters_book ON chapters(book_id, idx);

CREATE TABLE IF NOT EXISTS generated_chapters (
  id            INTEGER PRIMARY KEY,
  book_id       INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_idx   INTEGER NOT NULL,
  content       TEXT NOT NULL,
  model         TEXT NOT NULL,
  prompt_digest TEXT NOT NULL,
  active        INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_gen_book_ch ON generated_chapters(book_id, chapter_idx, active);

CREATE TABLE IF NOT EXISTS edits_log (
  id             INTEGER PRIMARY KEY,
  book_id        INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  target_type    TEXT NOT NULL,           -- graph|directive|drift_pref
  target_id      TEXT NOT NULL,
  action         TEXT NOT NULL,           -- upsert|delete|revert
  before_json    TEXT,
  after_json     TEXT,
  effective_from INTEGER NOT NULL,        -- 本章图谱编辑=章idx；阅读第N章时的全局编辑/指令=N+1
  created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_edits_book_ef ON edits_log(book_id, effective_from);

CREATE TABLE IF NOT EXISTS reader_intent (
  book_id     INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
  drift_pref  REAL NOT NULL DEFAULT 0,    -- 偏离旋钮（意愿）0..1
  updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS directives (
  id             INTEGER PRIMARY KEY,
  book_id        INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  polarity       TEXT NOT NULL,           -- positive|negative
  text           TEXT NOT NULL,
  effective_from INTEGER NOT NULL,
  active         INTEGER NOT NULL DEFAULT 1,
  created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_directives_book ON directives(book_id, active);

CREATE TABLE IF NOT EXISTS graph_nodes (
  id             INTEGER PRIMARY KEY,
  book_id        INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  scope          TEXT NOT NULL,           -- global|chapter
  chapter_idx    INTEGER,                 -- scope=chapter 时必填
  key            TEXT NOT NULL,           -- 稳定 slug
  type           TEXT NOT NULL,           -- character|location|item|faction|concept
  name           TEXT NOT NULL,
  aliases_json   TEXT NOT NULL DEFAULT '[]',
  summary        TEXT,
  provenance     TEXT NOT NULL,           -- ai_original|ai_adapted|ai_generated|user
  superseded_by  TEXT,
  updated_at     TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(book_id, scope, chapter_idx, key)
);
CREATE INDEX IF NOT EXISTS idx_nodes_book_scope ON graph_nodes(book_id, scope, chapter_idx);

CREATE TABLE IF NOT EXISTS graph_edges (
  id                INTEGER PRIMARY KEY,
  book_id           INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  scope             TEXT NOT NULL,
  chapter_idx       INTEGER,
  src_key           TEXT NOT NULL,
  dst_key           TEXT NOT NULL,
  rel_type          TEXT NOT NULL,
  description       TEXT,
  status            TEXT NOT NULL DEFAULT 'active',  -- active|broken|former|unknown
  since_chapter_idx INTEGER,
  provenance        TEXT NOT NULL,
  updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_edges_book_scope ON graph_edges(book_id, scope, chapter_idx);

CREATE TABLE IF NOT EXISTS graph_events (
  id                   INTEGER PRIMARY KEY,
  book_id              INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  scope                TEXT NOT NULL,
  chapter_idx          INTEGER NOT NULL,
  order_in_chapter     INTEGER NOT NULL,
  title                TEXT NOT NULL,
  participant_keys_json TEXT NOT NULL DEFAULT '[]',
  location_key         TEXT,
  summary              TEXT,
  outcome              TEXT,
  provenance           TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_book_ch ON graph_events(book_id, scope, chapter_idx);

CREATE TABLE IF NOT EXISTS chapter_graph_meta (
  book_id      INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_idx  INTEGER NOT NULL,
  adapted      INTEGER NOT NULL DEFAULT 0, -- 前向调和状态
  drift_score  REAL,                       -- 偏离度 0..1（事实度量）
  adapted_at   TEXT,
  PRIMARY KEY (book_id, chapter_idx)
);

CREATE TABLE IF NOT EXISTS jobs (
  id           INTEGER PRIMARY KEY,
  book_id      INTEGER,
  kind         TEXT NOT NULL,              -- global_extract|chapter_extract|generate|reconcile
  state        TEXT NOT NULL,              -- queued|running|paused|done|failed|cancelled
  progress     REAL NOT NULL DEFAULT 0,
  payload_json TEXT,
  error        TEXT,
  updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_jobs_book_state ON jobs(book_id, kind, state);

CREATE TABLE IF NOT EXISTS llm_providers (
  id            INTEGER PRIMARY KEY,
  name          TEXT NOT NULL,
  protocol      TEXT NOT NULL,             -- openai|anthropic
  base_url      TEXT NOT NULL,
  model_extract TEXT,
  model_write   TEXT,
  model_merge   TEXT,
  params_json   TEXT,
  key_ref       TEXT NOT NULL              -- OS 安全存储的引用，非明文 key
);

CREATE TABLE IF NOT EXISTS llm_cache (
  hash          TEXT PRIMARY KEY,          -- sha256(provider_id+model+messages+schema_id+params)
  response_json TEXT NOT NULL,
  tokens_in     INTEGER NOT NULL DEFAULT 0,
  tokens_out    INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS llm_usage (
  id          INTEGER PRIMARY KEY,
  provider_id INTEGER NOT NULL,
  book_id     INTEGER,
  task        TEXT NOT NULL,               -- extract|merge|write
  tokens_in   INTEGER NOT NULL DEFAULT 0,
  tokens_out  INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_usage_book ON llm_usage(book_id, task);
