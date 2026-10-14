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
fn resolved_writer_context_keeps_actual_history_and_selected_prose_separate_from_future_wiki() {
    let (_conn, book) = database();
    let mut previous = selected_text();
    previous.book_id = book;
    let full_prose = previous.content.clone();
    let next = next_wiki();
    let history = r#"{"entries":[{"id":"messenger","summary":"信使已经抵达驿站"}]}"#;
    let mut request = input(history, &previous, Some(&next), &[]);
    request.book_id = book;
    let context = build_wiki_generation_context(&request).unwrap();
    let payload = serde_json::to_value(&context).unwrap();
    assert_eq!(payload.as_object().unwrap().keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from(["historical_wiki", "previous_selected_full_text", "effective_next_wiki"]));
    assert_eq!(payload["historical_wiki"], serde_json::from_str::<Value>(history).unwrap());
    assert_eq!(payload["previous_selected_full_text"].as_str(), Some(full_prose.as_str()));
    assert_eq!(payload["effective_next_wiki"]["wiki"], serde_json::to_value(&next).unwrap());
    let combined = payload.to_string();
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
    let payload = serde_json::to_value(&context).unwrap();
    assert_eq!(payload["effective_next_wiki"]["valid_selected_answers"], json!([
        {"question_id":"q2","prompt":"方向问题2","option_id":"o2-0","label":"方向答案2-0"}
    ]));
    for excluded in ["o0-0", "o1-0", "方向答案0-0", "方向答案1-0", "conflicts_with", "invalid_reason"] {
        assert!(!payload.to_string().contains(excluded));
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
    let payload = serde_json::to_value(&context).unwrap();
    assert_eq!(payload["effective_next_wiki"]["user_free_text"], json!([text]));
    assert_eq!(payload["effective_next_wiki"]["valid_selected_answers"].as_array().unwrap().len(), 1);
    assert!(!payload.to_string().contains("o0-0"));
    assert!(!payload.to_string().contains("方向答案0-0"));
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
    let payload = serde_json::to_value(&context).unwrap();
    assert_eq!(payload["effective_next_wiki"]["valid_selected_answers"], json!([]));
    assert!(!payload.to_string().contains("o0-0"));
    assert!(!payload.to_string().contains("o1-0"));
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
    let quick=quick_read_reference();
    let original=TextVersion { id:99, book_id:1, chapter_idx:3, source:"original".into(), content:"原作第三章完整正文".into(), content_hash:"original-3".into(), generated_id:None };
    let request=RewriteInputs { book_id:1, chapter_idx:3, quick_read:&quick, historical_wiki:"{}", previous_selected_text:&previous, original_target:&original, original_target_wiki:None, next_wiki:&next, interventions:&[] };
    let mut tiny=config(); tiny.context_limit=100;
    assert!(build_rewrite_messages(&request,&tiny).is_err());
    let messages=build_rewrite_messages(&request,&config()).unwrap();
    let payload:Value=serde_json::from_str(&messages[1].content).unwrap();
    assert_eq!(payload["previous_selected_full_text"],previous.content);
    assert_eq!(payload["next_chapter_wiki"]["valid_selected_answers"],json!([]));
    assert_eq!(payload["next_chapter_wiki"]["user_free_text"],json!([]));
}

fn quick_read_reference()->crate::core::quick_read::QuickReadReference {
    crate::core::quick_read::QuickReadReference {
        schema_version:"v1".into(),source_digest:"book-digest".into(),provider_id:1,model:"reader".into(),prompt_version:"quick/v1".into(),
        source_kind:crate::core::quick_read::QuickReadSourceKind::LocalFullText,source_provenance:vec![],
        chapter_summaries:vec![crate::core::quick_read::ChapterQuickRead{chapter_idx:1,summary:"主角救下侍童".into(),character_refs:vec!["hero".into()]},crate::core::quick_read::ChapterQuickRead{chapter_idx:20,summary:"侍童身份揭示".into(),character_refs:vec!["hero".into()]}],
        characters:vec![crate::core::quick_read::QuickReadCharacterCard{id:"hero".into(),name:"顾岚".into(),aliases:vec!["阿七".into()],identity_evidence:vec!["第二十章确认两名相同".into()],motivations:vec!["保护边城".into()],narrative_roles:vec!["主线人物".into()],first_appearance_chapter:1,revelations:vec![crate::core::quick_read::CharacterReveal{chapter_idx:20,evidence:"真实身份揭示".into()}],source_chapters:vec![1,20]}],
        plot_summary:"守城主线".into(),plot_notes:vec![],completed_spans:vec![],
    }
}

#[test]
fn rewrite_writer_receives_all_required_sources_and_keeps_future_reference_separate(){
    let previous=selected_text();let next=next_wiki();let quick=quick_read_reference();
    let original=TextVersion{id:99,book_id:1,chapter_idx:3,source:"original".into(),content:"原作第三章完整正文：顾岚将受伤，但阿七会出现。".into(),content_hash:"original-3".into(),generated_id:None};
    let actual_history=r#"{"entries":[{"id":"hero","name":"顾岚","summary":"第一章救下侍童"}]}"#;
    let original_wiki=json!({"chapter":{"headline":"原作第三章","narrative":"身份线索","ending":{"open_threads":[]}},"entry_ids":["future-secret"]});
    let message=build_rewrite_messages(&RewriteInputs{book_id:1,chapter_idx:3,quick_read:&quick,historical_wiki:actual_history,previous_selected_text:&previous,original_target:&original,original_target_wiki:Some(&original_wiki),next_wiki:&next,interventions:&[]},&config()).unwrap();
    let payload:Value=serde_json::from_str(&message[1].content).unwrap();
    assert_eq!(payload["quick_read"]["characters"][0]["name"],"顾岚");
    assert_eq!(payload["previous_selected_full_text"],previous.content);
    assert_eq!(payload["original_target_chapter"],original.content);
    assert_eq!(payload["original_target_wiki"],original_wiki);
    assert_eq!(payload["actual_history"],serde_json::from_str::<Value>(actual_history).unwrap());
    assert_eq!(payload["next_chapter_wiki"]["wiki"]["goals"][0],"把信件交给守门人");
    assert!(message[0].content.contains("未干预人物默认沿用原作核心身份和动机"));
    assert!(message[0].content.contains("原作未来不是已发生事实"));
}

#[test]
fn paired_rewrite_output_rejects_empty_prose_and_empty_actual_wiki(){
    let schema=super::rewrite_output_schema();let validator=jsonschema::validator_for(schema).unwrap();
    let valid=json!({"prose":"改写后的完整第三章正文","actual_chapter_wiki":{"chapter_idx":3,"chapter":{"headline":"城门受袭","narrative":"守军发现奸细，顾岚决定留下断后。","ending":{"open_threads":["奸细身份未明"]}},"entries":[{"id":"hero","kind":"character","name":"顾岚","aliases":["阿七"],"summary":"保护边城的主线人物","state_items":["第三章选择留下断后"],"related_entries":[],"open_threads":[],"source_chapters":[1,3]}]}});
    assert!(validator.is_valid(&valid));
    let mut missing_wiki=valid.clone();missing_wiki.as_object_mut().unwrap().remove("actual_chapter_wiki");assert!(super::decode_rewrite_result(missing_wiki,3).is_err());
    let mut empty_prose=valid.clone();empty_prose["prose"]=json!(" ");assert!(super::decode_rewrite_result(empty_prose,3).is_err());
    let mut wrong_chapter=valid;wrong_chapter["actual_chapter_wiki"]["chapter_idx"]=json!(2);assert!(super::decode_rewrite_result(wrong_chapter,3).is_err());
}

#[test]
fn paired_rewrite_payload_has_no_future_actual_claim_until_bound_to_output(){
    let value=json!({"prose":"顾岚改变了出场行动，但仍是保护边城的核心人物。","actual_chapter_wiki":{"chapter_idx":3,"chapter":{"headline":"顾岚改变出场行动","narrative":"顾岚没有在桥上救人，而是在城门内安排撤离。","ending":{"open_threads":[]}},"entries":[{"id":"hero","kind":"character","name":"顾岚","aliases":[],"summary":"保护边城","state_items":["主动安排撤离"],"related_entries":[],"open_threads":[],"source_chapters":[3]}]}});
    let decoded=decode_rewrite_result(value,3).unwrap();
    assert!(decoded.prose.contains("改变了出场行动"));
    assert_eq!(decoded.actual_chapter_wiki["chapter"]["narrative"],"顾岚没有在桥上救人，而是在城门内安排撤离。");
    assert!(decode_rewrite_result(json!({"prose":"正文","actual_chapter_wiki":{"chapter_idx":4,"chapter":{"headline":"标题","narrative":"叙事","ending":{"open_threads":[]}},"entries":[]}}),3).is_err());
}
