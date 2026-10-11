use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    #[serde(skip)]
    pub conflicts_with: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub id: String,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionBatch {
    pub batch_index: usize,
    pub question_batch_id: String,
    pub questions: Vec<Question>,
}

impl QuestionBatch {
    pub fn validate(&self) -> Result<(), String> {
        if self.questions.len() != 3 {
            return Err("question batch must contain exactly three questions".into());
        }
        if self.questions.iter().any(|q| q.id.trim().is_empty() || q.prompt.trim().is_empty()) {
            return Err("question id and prompt cannot be empty".into());
        }
        let mut ids = std::collections::HashSet::new();
        for question in &self.questions {
            if !ids.insert(question.id.as_str()) {
                return Err("question ids must be unique".into());
            }
            let mut option_ids = std::collections::HashSet::new();
            for option in &question.options {
                if option.id.trim().is_empty() || option.label.trim().is_empty() || !option_ids.insert(option.id.as_str()) {
                    return Err("option ids and labels must be non-empty and unique".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SelectedOption {
    pub question_id: String,
    pub option_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ResolvedIntervention {
    pub effective: Vec<SelectedOption>,
    pub overridden_option_ids: Vec<String>,
}

/// User free text has higher precedence than selected choices. The conflict IDs
/// come from a local compatibility result; free text is not semantically guessed here.
pub fn apply_text_overrides(selected: Vec<SelectedOption>, overridden: &[String]) -> ResolvedIntervention {
    if selected.is_empty() {
        return ResolvedIntervention { effective: Vec::new(), overridden_option_ids: Vec::new() };
    }
    let overridden_set: std::collections::HashSet<&str> = overridden.iter().map(String::as_str).collect();
    let effective = selected.into_iter().filter(|item| !overridden_set.contains(item.option_id.as_str())).collect();
    ResolvedIntervention { effective, overridden_option_ids: overridden.to_vec() }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InterventionInput {
    pub book_id: i64,
    pub chapter_idx: i64,
    pub batch_index: Option<usize>,
    pub question_batch_id: Option<String>,
    pub selected_option_ids: Vec<String>,
    pub free_text: Option<String>,
    pub overridden_option_ids: Vec<String>,
    pub reading_line_version: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NextChapterWiki {
    pub chapter_idx: i64,
    pub goals: Vec<String>,
    pub state_directions: Vec<String>,
    pub open_threads: Vec<String>,
    pub avoid: Vec<String>,
    pub ending_direction: String,
    pub related_entry_ids: Vec<String>,
}
