
#[cfg(test)]
mod tests {
    use crate::core::story::{InterventionInput, NextChapterWiki, Question, QuestionBatch, QuestionOption};
    use crate::store::story::{active_interventions, active_next_wiki, question_batch, save_intervention, save_next_wiki, save_question_batch};
    use crate::store::{books::{create_book_with_chapters, NewChapter}, Db};
    use rusqlite::Connection;

    fn db() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        Db::apply_pragmas(&conn).unwrap();
        Db::new(&conn).migrate().unwrap();
        let book = create_book_with_chapters(&conn, "Wiki", None, "txt", "wiki.txt", &[NewChapter { idx: 1, title: "一".into(), text: "正文".into(), token_est: 2 }]).unwrap();
        (conn, book)
    }

    #[test]
    fn intervention_round_trip_preserves_text_and_selected_answers() {
        let (conn, book) = db();
        let input = InterventionInput { book_id: book, chapter_idx: 2, batch_index: Some(0), question_batch_id: Some("batch-1".into()), selected_option_ids: vec!["q1:alone".into()], free_text: Some("让两人继续合作".into()), overridden_option_ids: vec!["q2:split".into()], reading_line_version: 0 };
        save_intervention(&conn, &input).unwrap();
        assert_eq!(active_interventions(&conn, book, 2).unwrap(), vec![input]);
    }

    #[test]
    fn replacing_next_wiki_deactivates_previous_revision() {
        let (conn, book) = db();
        let previous = crate::store::kg::text_version(&conn, book, 1, "original", None).unwrap().id;
        let wiki = NextChapterWiki { chapter_idx: 2, goals: vec!["调查".into()], state_directions: vec![], open_threads: vec![], avoid: vec![], ending_direction: "留下线索".into(), related_entry_ids: vec![] };
        save_next_wiki(&conn, book, 2, previous, 1, "a", &wiki).unwrap();
        save_next_wiki(&conn, book, 2, previous, 1, "b", &wiki).unwrap();
        let active: i64 = conn.query_row("SELECT COUNT(*) FROM story_next_wiki_revisions WHERE book_id=?1 AND chapter_idx=2 AND active=1", [book], |r| r.get(0)).unwrap();
        assert_eq!(active, 1);
        assert_eq!(active_next_wiki(&conn, book, 2).unwrap(), Some(wiki));
    }

    #[test]
    fn saving_the_same_question_batch_id_is_idempotent() {
        let (conn, book) = db();
        let batch = QuestionBatch { batch_index: 0, question_batch_id: "batch-1".into(), questions: (0..3).map(|index| Question { id: format!("q-{index}"), prompt: "方向".into(), options: vec![QuestionOption { id: format!("o-{index}"), label: "继续".into(), conflicts_with: Vec::new() }, QuestionOption { id: format!("p-{index}"), label: "改变".into(), conflicts_with: Vec::new() }] }).collect() };
        save_question_batch(&conn, book, 2, 0, &batch).unwrap();
        save_question_batch(&conn, book, 2, 0, &batch).unwrap();
        assert_eq!(question_batch(&conn, book, 2, 0).unwrap(), Some(batch));
    }
}
