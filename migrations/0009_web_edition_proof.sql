-- Retain legacy text-similarity evidence while admitting score-free AI proofs.
-- This table has no inbound foreign keys; chapter maps reference revisions and
-- remain untouched by the transactional rebuild.
CREATE TABLE quick_read_web_outlines_0009 (
  revision_id INTEGER PRIMARY KEY REFERENCES quick_read_revisions(id) ON DELETE CASCADE,
  normalized_book_title TEXT NOT NULL,
  match_score REAL,
  provenance_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  proof_kind TEXT NOT NULL CHECK(proof_kind IN ('ai_paragraph','text_similarity')),
  proof_chapter_idx INTEGER NOT NULL CHECK(proof_chapter_idx>0),
  CHECK((proof_kind='ai_paragraph' AND match_score IS NULL) OR
        (proof_kind='text_similarity' AND proof_chapter_idx=1 AND
         match_score IS NOT NULL AND match_score>=0 AND match_score<=1))
);

INSERT INTO quick_read_web_outlines_0009(
  revision_id,normalized_book_title,match_score,provenance_json,created_at,proof_kind,proof_chapter_idx
)
SELECT revision_id,normalized_book_title,match_score,provenance_json,created_at,'text_similarity',1
FROM quick_read_web_outlines;

DROP TABLE quick_read_web_outlines;
ALTER TABLE quick_read_web_outlines_0009 RENAME TO quick_read_web_outlines;
