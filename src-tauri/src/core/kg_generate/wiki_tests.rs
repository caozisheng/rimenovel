use super::*;
use crate::core::story::{NextChapterWiki, Question, QuestionBatch, QuestionOption, SelectedOption};
use crate::core::story_plan::{resolve_selected_options, ResolvedAnswers, SelectedOptionStatus};

fn config() -> RuntimeConfig {
    RuntimeConfig { provider_id: 1, model_extract: "extract".into(), context_limit: 32768, output_reserve: 4096, prompt_version: "wiki/v1".into() }
}

fn database() -> (Connection, i64) {
    use crate::store::{Db, books::{create_book_with_chapters, NewChapter}};
    let conn = Connection::open_in_memory().unwrap();
    Db::apply_pragmas(&conn).unwrap();
    Db::new(&conn).migrate().unwrap();
    let chapters = (1..=3).map(|idx| NewChapter { idx, title: format!("章{idx}"), text: format!("原作第{idx}章"), token_est: 20 }).collect::<Vec<_>>();
    let book = create_book_with_chapters(&conn, "Wiki 作者测试", None, "txt", "test", &chapters).unwrap();
    repo::mark_read(&conn, book, 1).unwrap();
    (conn, book)
}

fn next_wiki() -> NextChapterWiki {
    NextChapterWiki {
        chapter_idx: 3,
        goals: vec!["把信件交给守门人".into()],
        state_directions: vec!["信使从驿站前往城门".into()],
        open_threads: vec!["核实信件来源".into()],
        avoid: vec!["把未来计划当成历史".into()],
        ending_direction: "在城门前作出决定".into(),
        related_entry_ids: vec!["messenger".into()],
    }
}

fn selected_text() -> TextVersion {
    TextVersion {
        id: 42,
        book_id: 1,
        chapter_idx: 2,
        source: "generated".into(),
        content: format!("完整开头\n{}\n完整结尾\n", "前章中段，不能截断。".repeat(1200)),
        content_hash: "selected-prose".into(),
        generated_id: Some(42),
    }
}

fn batch() -> QuestionBatch {
    QuestionBatch {
        batch_index: 0,
        question_batch_id: "batch-0".into(),
        questions: (0..3).map(|index| Question {
            id: format!("q{index}"),
            prompt: format!("方向问题{index}"),
            options: (0..2).map(|choice| QuestionOption {
                id: format!("o{index}-{choice}"),
                label: format!("方向答案{index}-{choice}"),
                conflicts_with: Vec::new(),
            }).collect(),
        }).collect(),
    }
}

fn input<'a>(history: &'a str, previous: &'a TextVersion, next: Option<&'a NextChapterWiki>, interventions: &'a [WikiWriterIntervention<'a>]) -> WikiGenerationInput<'a> {
    WikiGenerationInput {
        book_id: 1,
        chapter_idx: 3,
        historical_wiki: history,
        previous_selected_text: previous,
        next_wiki: next,
        next_wiki_frozen: false,
        interventions,
    }
}

#[test]
fn wiki_writer_has_exactly_three_story_inputs_and_no_original_next_reference() {
    let (_conn, book) = database();
    let mut previous = selected_text();
    previous.book_id = book;
    let full_prose = previous.content.clone();
    let next = next_wiki();
    let history = r#"{"entries":[{"id":"messenger","summary":"信使已经抵达驿站"}]}"#;
    let mut request = input(history, &previous, Some(&next), &[]);
    request.book_id = book;
    let context = build_wiki_generation_context(&request).unwrap();
    let messages = build_wiki_write_messages(&context, &config()).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
    let payload: Value = serde_json::from_str(&messages[1].content).unwrap();
    assert_eq!(payload.as_object().unwrap().keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["historical_wiki", "previous_selected_full_text", "effective_next_wiki"]));
    assert_eq!(payload["historical_wiki"], serde_json::from_str::<Value>(history).unwrap());
    assert_eq!(payload["previous_selected_full_text"].as_str(), Some(full_prose.as_str()));
    assert_eq!(payload["effective_next_wiki"]["wiki"], serde_json::to_value(&next).unwrap());
    let combined = messages.iter().map(|message| message.content.as_str()).collect::<Vec<_>>().join("\n");
    for excluded in ["原作第3章", "original_reference_text", "original_reference_graph", "hidden_reference", "working_spec", "actual_chapter", "hard_state", "intent_constraints", "known_entities"] {
        assert!(!combined.contains(excluded), "unexpected legacy writer input: {excluded}");
    }
}

#[test]
fn wiki_writer_excludes_conflicting_answers_but_keeps_valid_answer_meaning() {
    let mut batch = batch();
    batch.questions[0].options[0].conflicts_with.push("o1-0".into());
    let selected = vec!["o0-0".into(), "o1-0".into(), "o2-0".into()];
    let resolved = resolve_selected_options(&batch, &selected, None, &[]).unwrap();
    let interventions = [WikiWriterIntervention { batch: &batch, resolved: &resolved, user_free_text: None }];
    let previous = selected_text();
    let next = next_wiki();
    let context = build_wiki_generation_context(&input("{}", &previous, Some(&next), &interventions)).unwrap();
    let messages = build_wiki_write_messages(&context, &config()).unwrap();
    let payload: Value = serde_json::from_str(&messages[1].content).unwrap();
    assert_eq!(payload["effective_next_wiki"]["valid_selected_answers"], json!([
        {"question_id":"q2","prompt":"方向问题2","option_id":"o2-0","label":"方向答案2-0"}
    ]));
    for excluded in ["o0-0", "o1-0", "方向答案0-0", "方向答案1-0", "conflicts_with", "invalid_reason"] {
        assert!(!messages[1].content.contains(excluded));
    }
    assert_eq!(resolved.selected.len(), 3);
    assert!(!resolved.selected[0].valid);
}

#[test]
fn wiki_writer_preserves_user_text_and_excludes_overridden_answers() {
    let batch = batch();
    let text = "  不走选项中的路线，改由守门人来取信。\n";
    let resolved = resolve_selected_options(&batch, &["o0-0".into(), "o2-0".into()], Some(text), &["o0-0".into()]).unwrap();
    let interventions = [WikiWriterIntervention { batch: &batch, resolved: &resolved, user_free_text: Some(text) }];
    let previous = selected_text();
    let next = next_wiki();
    let context = build_wiki_generation_context(&input("{}", &previous, Some(&next), &interventions)).unwrap();
    let messages = build_wiki_write_messages(&context, &config()).unwrap();
    let payload: Value = serde_json::from_str(&messages[1].content).unwrap();
    assert_eq!(payload["effective_next_wiki"]["user_free_text"], json!([text]));
    assert_eq!(payload["effective_next_wiki"]["valid_selected_answers"].as_array().unwrap().len(), 1);
    assert!(!messages[1].content.contains("o0-0"));
    assert!(!messages[1].content.contains("方向答案0-0"));
    assert!(messages[0].content.contains("用户自由文字 > 有效选项答案"));
}

#[test]
fn wiki_writer_does_not_trust_invalid_or_overridden_effective_records() {
    let batch = batch();
    let resolved = ResolvedAnswers {
        effective: vec![
            SelectedOption { question_id: "q0".into(), option_id: "o0-0".into() },
            SelectedOption { question_id: "q1".into(), option_id: "o1-0".into() },
        ],
        selected: vec![
            SelectedOptionStatus { question_id: "q0".into(), option_id: "o0-0".into(), valid: false, invalid_reason: Some("不兼容".into()) },
            SelectedOptionStatus { question_id: "q1".into(), option_id: "o1-0".into(), valid: true, invalid_reason: None },
        ],
        overridden_option_ids: vec!["o1-0".into()],
    };
    let interventions = [WikiWriterIntervention { batch: &batch, resolved: &resolved, user_free_text: Some("采用用户指定的路线") }];
    let previous = selected_text();
    let next = next_wiki();
    let context = build_wiki_generation_context(&input("{}", &previous, Some(&next), &interventions)).unwrap();
    let messages = build_wiki_write_messages(&context, &config()).unwrap();
    let payload: Value = serde_json::from_str(&messages[1].content).unwrap();
    assert_eq!(payload["effective_next_wiki"]["valid_selected_answers"], json!([]));
    assert!(!messages[1].content.contains("o0-0"));
    assert!(!messages[1].content.contains("o1-0"));
}

#[test]
fn wiki_writer_rejects_missing_frozen_or_wrong_chapter_wiki() {
    let previous = selected_text();
    let mut next = next_wiki();
    assert!(build_wiki_generation_context(&input("{}", &previous, None, &[])).err().unwrap().contains("缺失"));
    let mut frozen = input("{}", &previous, Some(&next), &[]);
    frozen.next_wiki_frozen = true;
    assert!(build_wiki_generation_context(&frozen).err().unwrap().contains("冻结"));
    next.chapter_idx = 4;
    assert!(build_wiki_generation_context(&input("{}", &previous, Some(&next), &[])).is_err());
}

#[test]
fn wiki_writer_rejects_bad_history_or_wrong_selected_predecessor() {
    let mut previous = selected_text();
    let next = next_wiki();
    assert!(build_wiki_generation_context(&input("not JSON", &previous, Some(&next), &[])).is_err());
    previous.chapter_idx = 1;
    assert!(build_wiki_generation_context(&input("{}", &previous, Some(&next), &[])).is_err());
    previous.chapter_idx = 2;
    previous.book_id = 2;
    assert!(build_wiki_generation_context(&input("{}", &previous, Some(&next), &[])).is_err());
}

#[test]
fn wiki_writer_allows_zero_answers_but_rejects_budget_instead_of_truncating_prose() {
    let previous = selected_text();
    let next = next_wiki();
    let context = build_wiki_generation_context(&input("{}", &previous, Some(&next), &[])).unwrap();
    let mut tiny = config();
    tiny.context_limit = 100;
    assert!(build_wiki_write_messages(&context, &tiny).err().unwrap().contains("上下文预算"));
    let messages = build_wiki_write_messages(&context, &config()).unwrap();
    let payload: Value = serde_json::from_str(&messages[1].content).unwrap();
    assert_eq!(payload["previous_selected_full_text"], previous.content);
    assert_eq!(payload["effective_next_wiki"]["valid_selected_answers"], json!([]));
    assert_eq!(payload["effective_next_wiki"]["user_free_text"], json!([]));
}
