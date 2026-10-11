use super::*;
use serde_json::json;

fn batch_value() -> Value {
    json!({
        "batch_index": 0,
        "question_batch_id": "batch-1",
        "questions": [
            {"id": "q1", "prompt": "如何行动？", "options": [{"id": "alone", "label": "独自调查"}, {"id": "together", "label": "共同调查"}]},
            {"id": "q2", "prompt": "如何交流？", "options": [{"id": "ask", "label": "直接询问"}, {"id": "observe", "label": "先观察"}]},
            {"id": "q3", "prompt": "节奏？", "options": [{"id": "slow", "label": "谨慎推进"}, {"id": "fast", "label": "加快行动"}]}
        ]
    })
}

fn batch() -> QuestionBatch {
    decode_question_batch(batch_value()).unwrap()
}

#[test]
fn question_batch_requires_exactly_three_questions() {
    assert_eq!(batch().questions.len(), 3);
    for count in [0, 1, 2, 4] {
        let mut value = batch_value();
        let questions = value["questions"].as_array_mut().unwrap();
        if count > 3 {
            questions.push(json!({"id": "q4", "prompt": "方向？", "options": [{"id": "left", "label": "左"}, {"id": "right", "label": "右"}]}));
        } else {
            questions.truncate(count);
        }
        assert!(decode_question_batch(value).is_err(), "accepted {count} questions");
    }
}

#[test]
fn question_batch_rejects_empty_and_duplicate_identifiers_and_labels() {
    for path in ["batch", "question", "prompt", "option", "label"] {
        let mut value = batch_value();
        match path {
            "batch" => value["question_batch_id"] = json!("  "),
            "question" => value["questions"][0]["id"] = json!("  "),
            "prompt" => value["questions"][0]["prompt"] = json!("  "),
            "option" => value["questions"][0]["options"][0]["id"] = json!("  "),
            "label" => value["questions"][0]["options"][0]["label"] = json!("  "),
            _ => unreachable!(),
        }
        assert!(decode_question_batch(value).is_err(), "accepted empty {path}");
    }
    let mut value = batch_value();
    value["questions"][1]["id"] = json!("q1");
    assert!(decode_question_batch(value).is_err());
    let mut value = batch_value();
    value["questions"][1]["options"][0]["id"] = json!("alone");
    assert!(decode_question_batch(value).is_err());
    let mut value = batch_value();
    value["questions"][0]["options"][1]["label"] = json!("独自调查");
    assert!(decode_question_batch(value).is_err());
    let mut value = batch_value();
    value["questions"][0]["options"] = json!([]);
    assert!(decode_question_batch(value).is_err());
}

#[test]
fn question_batch_rejects_leaked_compatibility_metadata_and_unknown_fields() {
    for key in ["conflicts_with", "compatible_with", "internal_constraint"] {
        let mut value = batch_value();
        value["questions"][0]["options"][0][key] = json!(["ask"]);
        assert!(decode_question_batch(value).is_err());
    }
    let mut value = batch_value();
    value["hidden_reference"] = json!("spoiler");
    assert!(decode_question_batch(value).is_err());
}

#[test]
fn user_text_overrides_conflicting_answers_without_erasing_history() {
    let original = batch();
    let selected = vec!["alone".into(), "ask".into()];
    let resolved = resolve_selected_options(&original, &selected, Some("我要与同伴一起调查"), &["alone".into()]).unwrap();
    assert_eq!(resolved.effective, vec![SelectedOption {question_id: "q2".into(), option_id: "ask".into()}]);
    assert_eq!(resolved.selected.len(), 2);
    assert_eq!(resolved.selected[0].option_id, "alone");
    assert!(!resolved.selected[0].valid);
    assert_eq!(resolved.selected[0].invalid_reason.as_deref(), Some("已被用户文字覆盖"));
    assert!(resolved.selected[1].valid);
    assert_eq!(resolved.overridden_option_ids, vec!["alone"]);
    assert_eq!(original, batch());
    assert_eq!(selected, vec!["alone", "ask"]);
}

#[test]
fn zero_to_three_partial_answers_are_allowed() {
    let options: Vec<String> = vec!["alone".into(), "ask".into(), "slow".into()];
    for count in 0..=3 {
        let resolved = resolve_selected_options(&batch(), &options[..count], None, &[]).unwrap();
        assert_eq!(resolved.effective.len(), count);
        assert_eq!(resolved.selected.len(), count);
        assert!(resolved.selected.iter().all(|answer| answer.valid));
    }
    let resolved = resolve_selected_options(&batch(), &[], Some("保持轻松的风格"), &[]).unwrap();
    assert!(resolved.effective.is_empty());
}

#[test]
fn incompatible_options_stay_visible_but_do_not_become_effective_answers() {
    let mut original = batch();
    original.questions[0].options[0].conflicts_with.push("ask".into());
    let resolved = resolve_selected_options(&original, &["alone".into(), "ask".into(), "slow".into()], None, &[]).unwrap();
    assert_eq!(resolved.effective, vec![SelectedOption {question_id: "q3".into(), option_id: "slow".into()}]);
    assert_eq!(resolved.selected.len(), 3);
    for answer in &resolved.selected[..2] {
        assert!(!answer.valid);
        assert_eq!(answer.invalid_reason.as_deref(), Some("与当前其他已选答案不兼容"));
    }
    let public = serde_json::to_string(&resolved).unwrap();
    assert!(!public.contains("conflicts_with"));
    assert!(!public.contains("compatibility"));
}

#[test]
fn overridden_answers_do_not_invalidate_other_selected_answers() {
    let mut original = batch();
    original.questions[0].options[0].conflicts_with.push("ask".into());
    let resolved = resolve_selected_options(&original, &["alone".into(), "ask".into()], Some("不独自行动"), &["alone".into()]).unwrap();
    assert_eq!(resolved.effective, vec![SelectedOption {question_id: "q2".into(), option_id: "ask".into()}]);
    assert!(resolved.selected[1].valid);
}

#[test]
fn selection_and_override_references_are_validated() {
    let original = batch();
    for selected in [vec!["missing".into()], vec!["alone".into(), "alone".into()], vec!["alone".into(), "together".into()]] {
        assert!(resolve_selected_options(&original, &selected, None, &[]).is_err());
    }
    assert!(resolve_selected_options(&original, &["alone".into()], None, &["alone".into()]).is_err());
    assert!(resolve_selected_options(&original, &["alone".into()], Some("  "), &["alone".into()]).is_err());
    assert!(resolve_selected_options(&original, &["alone".into()], Some("一起行动"), &["ask".into()]).is_err());
    assert!(resolve_selected_options(&original, &["alone".into()], Some("一起行动"), &["alone".into(), "alone".into()]).is_err());
    let mut malformed = original;
    malformed.questions[0].options[0].conflicts_with.push("missing".into());
    assert!(resolve_selected_options(&malformed, &["alone".into()], None, &[]).is_err());
}

#[test]
fn planner_messages_preserve_full_context_and_explicit_text_precedence() {
    let selected = vec!["ask".into()];
    let batches = vec![batch()];
    let full_text = "前章完整正文\n保留最后一段。";
    let input = PlannerInput {
        chapter_idx: 2,
        batch_index: 0,
        historical_wiki: r#"{"entries":[{"id":"hero","name":"主角"}]}"#,
        previous_selected_text: full_text,
        valid_selected_option_ids: &selected,
        existing_question_batches: &batches,
        user_free_text: Some("用户文字要求共同调查"),
        hidden_reference: Some("原作未来仅供内部参考"),
    };
    let messages = build_planner_messages(&input).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert!(messages[0].content.contains("用户自由文字 > 有效选项答案"));
    assert!(messages[0].content.contains("不得修改历史 Wiki"));
    let payload: Value = serde_json::from_str(&messages[1].content).unwrap();
    assert_eq!(payload["previous_selected_full_text"], full_text);
    assert_eq!(payload["historical_wiki"]["entries"][0]["id"], "hero");
    assert_eq!(payload["valid_selected_option_ids"], json!(["ask"]));
    assert_eq!(payload["valid_selected_answers"], json!([{"question_id":"q2","prompt":"如何交流？","option_id":"ask","label":"直接询问"}]));
    assert!(!messages[1].content.contains("conflicts_with"));
    assert_eq!(payload["user_free_text"], "用户文字要求共同调查");
    assert_eq!(payload["hidden_reference"], "原作未来仅供内部参考");
}

#[test]
fn planner_messages_omit_absent_optional_context_and_reject_invalid_input() {
    let mut input = PlannerInput {
        chapter_idx: 1,
        batch_index: 0,
        historical_wiki: "{}",
        previous_selected_text: "",
        valid_selected_option_ids: &[],
        existing_question_batches: &[],
        user_free_text: None,
        hidden_reference: None,
    };
    let messages = build_planner_messages(&input).unwrap();
    let payload: Value = serde_json::from_str(&messages[1].content).unwrap();
    assert!(payload.get("hidden_reference").is_none());
    assert!(payload.get("user_free_text").is_none());
    input.historical_wiki = "not JSON";
    assert!(build_planner_messages(&input).is_err());
    input.historical_wiki = "{}";
    input.chapter_idx = 0;
    assert!(build_planner_messages(&input).is_err());
}

#[test]
fn next_wiki_decode_is_schema_strict() {
    let valid = json!({"chapter_idx":2,"goals":["继续调查"],"state_directions":[],"open_threads":[],"avoid":[],"ending_direction":"留下疑问","related_entry_ids":["hero"]});
    assert_eq!(decode_next_wiki(valid.clone()).unwrap().chapter_idx, 2);
    let mut invalid = valid.clone();
    invalid["chapter_idx"] = json!(0);
    assert!(decode_next_wiki(invalid).is_err());
    let mut invalid = valid.clone();
    invalid["history_edits"] = json!([]);
    assert!(decode_next_wiki(invalid).is_err());
    let mut invalid = valid;
    invalid.as_object_mut().unwrap().remove("avoid");
    assert!(decode_next_wiki(invalid).is_err());
}

#[test]
fn structured_planner_requests_use_exact_schemas_and_never_the_writer_task() {
    let input = PlannerInput {
        chapter_idx: 2,
        batch_index: 1,
        historical_wiki: "{}",
        previous_selected_text: "完整正文",
        valid_selected_option_ids: &[],
        existing_question_batches: &[],
        user_free_text: None,
        hidden_reference: None,
    };
    let questions = question_batch_request(&input, 1024).unwrap();
    assert_eq!(questions.task, TaskKind::Merge);
    assert_eq!(questions.schema_name, "question_batch");
    assert_eq!(questions.schema, question_batch_schema());
    assert_eq!(questions.temperature, 0.0);
    assert_eq!(questions.max_tokens, 1024);
    let next = next_wiki_request(&input, 2048).unwrap();
    assert_eq!(next.task, TaskKind::Merge);
    assert_eq!(next.schema_name, "next_wiki");
    assert_eq!(next.schema, next_wiki_schema());
    assert!(question_batch_request(&input, 0).is_err());
    assert!(next_wiki_request(&input, 0).is_err());
}

#[test]
fn direct_domain_validation_is_as_strict_as_question_decode() {
    let mut invalid = batch();
    invalid.questions[0].options.truncate(1);
    assert!(validate_question_batch(&invalid).is_err());
    let mut invalid = batch();
    invalid.question_batch_id = " ".into();
    assert!(validate_question_batch(&invalid).is_err());
    let mut invalid = batch();
    invalid.questions[2].options[0].id = "alone".into();
    assert!(validate_question_batch(&invalid).is_err());
}

#[test]
fn malformed_private_compatibility_is_rejected_without_exposing_constraints() {
    for conflicts in [vec!["alone".into()], vec!["ask".into(), "ask".into()], vec!["missing".into()]] {
        let mut invalid = batch();
        invalid.questions[0].options[0].conflicts_with = conflicts;
        let error = resolve_selected_options(&invalid, &["alone".into()], None, &[]).unwrap_err();
        assert_eq!(error, "私有选项兼容性引用无效");
        assert!(!error.contains("missing"));
    }
}

#[test]
fn planner_input_rejects_empty_or_duplicate_effective_ids() {
    for ids in [vec![" ".into()], vec!["ask".into(), "ask".into()]] {
        let input = PlannerInput {
            chapter_idx: 2,
            batch_index: 0,
            historical_wiki: "{}",
            previous_selected_text: "完整正文",
            valid_selected_option_ids: &ids,
            existing_question_batches: &[],
            user_free_text: None,
            hidden_reference: None,
        };
        assert!(build_planner_messages(&input).is_err());
    }
}

#[test]
fn effective_answer_ids_require_unambiguous_public_semantic_context() {
    let selected = vec!["ask".into()];
    let mut input = PlannerInput {
        chapter_idx: 2,
        batch_index: 1,
        historical_wiki: "{}",
        previous_selected_text: "完整正文",
        valid_selected_option_ids: &selected,
        existing_question_batches: &[],
        user_free_text: None,
        hidden_reference: None,
    };
    assert!(build_planner_messages(&input).is_err());
    let mut private_batch = batch();
    private_batch.questions[1].options[0].conflicts_with.push("alone".into());
    let batches = vec![private_batch.clone()];
    input.existing_question_batches = &batches;
    let messages = build_planner_messages(&input).unwrap();
    assert!(messages[1].content.contains("直接询问"));
    assert!(!messages[1].content.contains("conflicts_with"));
    let ambiguous_batches = vec![private_batch.clone(), private_batch];
    input.existing_question_batches = &ambiguous_batches;
    assert!(build_planner_messages(&input).is_err());
}

#[test]
fn generated_batch_identity_cannot_reuse_prior_history() {
    let previous = vec![batch()];
    let input = PlannerInput {
        chapter_idx: 2,
        batch_index: 1,
        historical_wiki: "{}",
        previous_selected_text: "完整正文",
        valid_selected_option_ids: &[],
        existing_question_batches: &previous,
        user_free_text: None,
        hidden_reference: None,
    };
    let mut generated = batch();
    assert!(validate_batch_identity(&generated, &input).is_err());
    generated.batch_index = 1;
    assert!(validate_batch_identity(&generated, &input).is_err());
    generated.question_batch_id = "batch-2".into();
    assert!(validate_batch_identity(&generated, &input).is_err());
    for question in &mut generated.questions {
        for option in &mut question.options {
            option.id = format!("b1-{}", option.id);
        }
    }
    assert!(validate_batch_identity(&generated, &input).is_ok());
    let messages = build_planner_messages(&input).unwrap();
    let payload: Value = serde_json::from_str(&messages[1].content).unwrap();
    assert_eq!(payload["existing_option_ids"], json!(["alone", "together", "ask", "observe", "slow", "fast"]));
    assert_eq!(payload["existing_question_batch_ids"], json!(["batch-1"]));
}
