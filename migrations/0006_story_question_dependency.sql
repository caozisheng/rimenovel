-- Source-bound question batches. Legacy rows retain NULL and cannot qualify for reuse.
ALTER TABLE story_question_batches ADD COLUMN dependency_digest TEXT;
