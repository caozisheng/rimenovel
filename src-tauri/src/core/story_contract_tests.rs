use super::story::{apply_text_overrides, Question, QuestionBatch, QuestionOption, SelectedOption};

fn batch() -> QuestionBatch {
    QuestionBatch {
        batch_index: 0,
        question_batch_id: "batch-1".into(),
        questions: vec![
            Question { id: "q1".into(), prompt: "处理线索？".into(), options: vec![QuestionOption { id: "alone".into(), label: "独自调查".into(), conflicts_with: vec!["together".into()] }, QuestionOption { id: "together".into(), label: "共同调查".into(), conflicts_with: vec!["alone".into()] }] },
            Question { id: "q2".into(), prompt: "如何交流？".into(), options: vec![QuestionOption { id: "ask".into(), label: "直接询问".into(), conflicts_with: vec![] }] },
            Question { id: "q3".into(), prompt: "节奏？".into(), options: vec![QuestionOption { id: "slow".into(), label: "谨慎推进".into(), conflicts_with: vec![] }] },
        ],
    }
}

#[test]
fn question_batch_requires_exactly_three_questions() {
    assert!(batch().validate().is_ok());
    let mut invalid = batch();
    invalid.questions.pop();
    assert_eq!(invalid.validate(), Err("question batch must contain exactly three questions".into()));
}

#[test]
fn user_text_overrides_conflicting_selected_options_without_erasing_history() {
    let selected = vec![
        SelectedOption { question_id: "q1".into(), option_id: "alone".into() },
        SelectedOption { question_id: "q2".into(), option_id: "ask".into() },
    ];
    let resolved = apply_text_overrides(selected, &["alone".into()]);
    assert_eq!(resolved.effective, vec![SelectedOption { question_id: "q2".into(), option_id: "ask".into() }]);
    assert_eq!(resolved.overridden_option_ids, vec!["alone"]);
}

#[test]
fn unanswered_questions_do_not_create_interventions() {
    let resolved = apply_text_overrides(Vec::new(), &["alone".into()]);
    assert!(resolved.effective.is_empty());
    assert!(resolved.overridden_option_ids.is_empty());
}
