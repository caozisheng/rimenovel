use crate::commands::AppState;
use crate::core::story::{InterventionInput, QuestionBatch, NextChapterWiki};
use crate::store::story as repo;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoryMapResponse {
    pub entries: Vec<serde_json::Value>,
    pub chapter_cards: Vec<serde_json::Value>,
    pub coverage: serde_json::Value,
    pub wiki_revision: i64,
    pub reading_line_version: i64,
    pub ready: bool,
}

#[tauri::command]
pub fn get_story_map(state: tauri::State<AppState>, book_id: i64) -> Result<StoryMapResponse, String> {
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    let progress: i64 = conn.query_row("SELECT progress FROM kg_lines WHERE book_id=?1", [book_id], |row| row.get(0)).map_err(|e| e.to_string())?;
    let line_version: i64 = conn.query_row("SELECT version FROM kg_lines WHERE book_id=?1", [book_id], |row| row.get(0)).map_err(|e| e.to_string())?;
    let value = crate::store::story_projection::story_map(conn, book_id, progress, line_version)?;
    serde_json::from_value(value).map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Serialize)]
pub struct InterventionResult {
    pub id: i64,
    pub overridden_option_ids: Vec<String>,
}

#[tauri::command]
pub async fn save_intervention(state: tauri::State<'_, AppState>, input: InterventionInput) -> Result<InterventionResult, String> {
    let overridden = input.overridden_option_ids.clone();
    let id = {
        let guard = state.db.lock();
        repo::save_intervention(guard.as_ref().ok_or("数据库未初始化")?, &input)?
    };
    prepare_next_wiki(&state, &input).await?;
    Ok(InterventionResult { id, overridden_option_ids: overridden })
}

#[tauri::command]
pub async fn end_questionnaire(state: tauri::State<'_, AppState>, input: InterventionInput) -> Result<InterventionResult, String> {
    save_intervention(state, input).await
}

#[tauri::command]
pub fn get_active_interventions(state: tauri::State<AppState>, book_id: i64, chapter_idx: i64) -> Result<Vec<InterventionInput>, String> {
    let guard = state.db.lock();
    repo::active_interventions(guard.as_ref().ok_or("数据库未初始化")?, book_id, chapter_idx)
}

fn question_batch_response(batch: &QuestionBatch, interventions: &[InterventionInput]) -> serde_json::Value {
    let answered_count = interventions.iter().map(|item| item.selected_option_ids.len()).sum::<usize>();
    let overridden = interventions.iter().flat_map(|item| item.overridden_option_ids.iter()).map(|id| id.rsplit_once(':').map(|(_, option)| option).unwrap_or(id.as_str())).collect::<std::collections::HashSet<_>>();
    let effective_answer_count = interventions.iter().flat_map(|item| item.selected_option_ids.iter()).filter(|id| {
        let option = id.rsplit_once(':').map(|(_, option)| option).unwrap_or(id.as_str());
        !overridden.contains(option)
    }).count();
    let mut response = serde_json::to_value(batch).expect("QuestionBatch is serializable");
    let object = response.as_object_mut().expect("QuestionBatch serializes to an object");
    object.insert("answered_count".into(), serde_json::json!(answered_count));
    object.insert("effective_answer_count".into(), serde_json::json!(effective_answer_count));
    object.insert("has_more".into(), serde_json::json!(true));
    object.insert("can_end".into(), serde_json::json!(true));
    response
}

#[tauri::command]
pub async fn get_question_batch(state: tauri::State<'_, AppState>, book_id: i64, chapter_idx: i64, batch_index: usize) -> Result<Option<serde_json::Value>, String> {
    {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        if let Some(batch) = repo::question_batch(conn, book_id, chapter_idx, batch_index)? {
            let interventions = repo::active_interventions(conn, book_id, chapter_idx)?;
            return Ok(Some(question_batch_response(&batch, &interventions)));
        }
    }
    let (provider, config) = {
        let guard = state.db.lock();
        crate::commands::kg::runtime_config(guard.as_ref().ok_or("数据库未初始化")?, None)?
    };
    let (historical_wiki, previous_text, interventions, existing_batches, line_version) = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let progress: i64 = conn.query_row("SELECT progress FROM kg_lines WHERE book_id=?1", [book_id], |row| row.get(0)).map_err(|e| e.to_string())?;
        let line_version = crate::store::kg::line_version(conn, book_id)?;
        let wiki = crate::store::story_projection::story_map(conn, book_id, progress, line_version)?;
        let previous = if chapter_idx > 1 { crate::store::kg::selected_text(conn, book_id, chapter_idx - 1)?.map(|text| text.content).unwrap_or_default() } else { String::new() };
        (wiki.to_string(), previous, repo::active_interventions(conn, book_id, chapter_idx)?, repo::question_batches(conn, book_id, chapter_idx)?, line_version)
    };
    let known_option_ids = existing_batches.iter().flat_map(|batch| batch.questions.iter()).flat_map(|question| question.options.iter()).map(|option| option.id.as_str()).collect::<std::collections::HashSet<_>>();
    let normalize = |raw: &str| {
        if known_option_ids.contains(raw) { raw.to_owned() } else { raw.rsplit_once(':').map(|(_, id)| id.to_owned()).unwrap_or_else(|| raw.to_owned()) }
    };
    let overridden = interventions.iter().flat_map(|item| item.overridden_option_ids.iter()).map(|id| normalize(id)).collect::<std::collections::HashSet<_>>();
    let selected = interventions.iter().flat_map(|item| item.selected_option_ids.iter()).map(|id| normalize(id)).filter(|id| known_option_ids.contains(id.as_str()) && !overridden.contains(id)).collect::<Vec<_>>();
    let input = crate::core::story_plan::PlannerInput {
        chapter_idx,
        batch_index,
        historical_wiki: &historical_wiki,
        previous_selected_text: &previous_text,
        valid_selected_option_ids: &selected,
        existing_question_batches: &existing_batches,
        user_free_text: interventions.iter().rev().find_map(|item| item.free_text.as_deref()),
        hidden_reference: None,
    };
    let client = crate::commands::providers::make_client(provider)?;
    let batch = crate::core::story_plan::generate_question_batch(client.as_ref(), &input, config.output_reserve.min(4096) as u32).await?;
    let _commit = client.commit_guard().map_err(|e| e.to_string())?;
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    if crate::store::kg::line_version(conn, book_id)? != line_version { return Err("阅读线已变化，请重新加载问题批次".into()); }
    repo::save_question_batch(conn, book_id, chapter_idx, line_version, &batch)?;
    Ok(Some(question_batch_response(&batch, &interventions)))
}


async fn prepare_next_wiki(state: &tauri::State<'_, AppState>, input: &InterventionInput) -> Result<(), String> {
    if input.chapter_idx < 2 { return Err("下一章规划必须有已选中的前章正文".into()); }
    let (provider, config, historical_wiki, previous, interventions, existing_batches, line_version) = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let (provider, config) = crate::commands::kg::runtime_config(conn, None)?;
        let progress: i64 = conn.query_row("SELECT progress FROM kg_lines WHERE book_id=?1", [input.book_id], |row| row.get(0)).map_err(|e| e.to_string())?;
        let line_version = crate::store::kg::line_version(conn, input.book_id)?;
        let historical_wiki = crate::store::story_projection::story_map(conn, input.book_id, progress, line_version)?.to_string();
        let previous = crate::store::kg::selected_text(conn, input.book_id, input.chapter_idx - 1)?.ok_or("选中的前章正文缺失")?;
        (provider, config, historical_wiki, previous, repo::active_interventions(conn, input.book_id, input.chapter_idx)?, repo::question_batches(conn, input.book_id, input.chapter_idx)?, line_version)
    };
    let known_option_ids = existing_batches.iter().flat_map(|batch| batch.questions.iter()).flat_map(|question| question.options.iter()).map(|option| option.id.as_str()).collect::<std::collections::HashSet<_>>();
    let normalize = |raw: &str| {
        if known_option_ids.contains(raw) { raw.to_owned() } else { raw.rsplit_once(':').map(|(_, id)| id.to_owned()).unwrap_or_else(|| raw.to_owned()) }
    };
    let overridden = interventions.iter().flat_map(|item| item.overridden_option_ids.iter()).map(|id| normalize(id)).collect::<std::collections::HashSet<_>>();
    let selected = interventions.iter().flat_map(|item| item.selected_option_ids.iter()).map(|id| normalize(id)).filter(|id| known_option_ids.contains(id.as_str()) && !overridden.contains(id)).collect::<Vec<_>>();
    let user_free_text = interventions.iter().rev().find_map(|item| item.free_text.as_deref());
    let planner_input = crate::core::story_plan::PlannerInput {
        chapter_idx: input.chapter_idx,
        batch_index: input.batch_index.unwrap_or(0),
        historical_wiki: &historical_wiki,
        previous_selected_text: &previous.content,
        valid_selected_option_ids: &selected,
        existing_question_batches: &existing_batches,
        user_free_text,
        hidden_reference: None,
    };
    let client = crate::commands::providers::make_client(provider)?;
    let wiki = crate::core::story_plan::plan_next_chapter(client.as_ref(), &planner_input, config.output_reserve.min(4096) as u32).await?;
    let dependency = crate::core::kg::digest(serde_json::to_string(&planner_input.valid_selected_option_ids).map_err(|e| e.to_string())?.as_bytes());
    let _commit = client.commit_guard().map_err(|e| e.to_string())?;
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    if crate::store::kg::line_version(conn, input.book_id)? != line_version { return Err("阅读线已变化，规划结果未激活".into()); }
    repo::save_next_wiki(conn, input.book_id, input.chapter_idx, previous.id, line_version, &dependency, &wiki)?;
    Ok(())
}
#[tauri::command]
pub fn get_next_wiki(state: tauri::State<AppState>, book_id: i64, chapter_idx: i64) -> Result<Option<NextChapterWiki>, String> {
    let guard = state.db.lock();
    repo::active_next_wiki(guard.as_ref().ok_or("数据库未初始化")?, book_id, chapter_idx)
}

#[cfg(test)]
mod tests {
    use super::question_batch_response;
    use crate::core::story::{InterventionInput, Question, QuestionBatch, QuestionOption};

    #[test]
    fn question_batch_response_includes_progress_fields() {
        let batch = QuestionBatch {
            batch_index: 0,
            question_batch_id: "batch-1".into(),
            questions: (0..3).map(|index| Question {
                id: format!("q-{index}"),
                prompt: format!("问题 {index}"),
                options: vec![QuestionOption { id: format!("o-{index}"), label: "选项".into(), conflicts_with: Vec::new() }, QuestionOption { id: format!("p-{index}"), label: "另一项".into(), conflicts_with: Vec::new() }],
            }).collect(),
        };
        let input = InterventionInput { book_id: 1, chapter_idx: 2, batch_index: Some(0), question_batch_id: Some("batch-1".into()), selected_option_ids: vec!["q-0:o-0".into(), "q-1:o-1".into()], free_text: Some("保留第二个方向".into()), overridden_option_ids: vec!["q-1:o-1".into()], reading_line_version: 1 };
        let response = question_batch_response(&batch, &[input]);
        assert_eq!(response["answered_count"], 2);
        assert_eq!(response["effective_answer_count"], 1);
        assert_eq!(response["has_more"], true);
        assert_eq!(response["can_end"], true);
    }
}
