use crate::commands::AppState;
use crate::core::story::{InterventionInput, QuestionBatch, NextChapterWiki};
use crate::store::story as repo;
use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

/// One frozen, source-qualified view used both for requests and eligibility.
/// Hidden reference stays backend-only; the public projection is never enriched.
struct PlannerSnapshot {
    chapter_idx: i64,
    previous: crate::core::kg::TextVersion,
    historical_wiki: String,
    hidden_reference: String,
    history_revision: i64,
    line_version: i64,
    quick_read_revision: i64,
    original_target_id: i64,
    original_target_hash: String,
    interventions: Vec<InterventionInput>,
    batches: Vec<QuestionBatch>,
    selected: Vec<String>,
    provider_dependency: Value,
    max_tokens: u32,
}

impl PlannerSnapshot {
    fn input(&self, batch_index: Option<usize>) -> crate::core::story_plan::PlannerInput<'_> {
        crate::core::story_plan::PlannerInput {
            chapter_idx: self.chapter_idx,
            batch_index: batch_index.unwrap_or_else(|| self.interventions.last().and_then(|item| item.batch_index).unwrap_or(0)),
            historical_wiki: &self.historical_wiki,
            previous_selected_text: &self.previous.content,
            valid_selected_option_ids: &self.selected,
            existing_question_batches: &self.batches,
            user_free_text: self.interventions.iter().rev().find_map(|item| item.free_text.as_deref().filter(|text| !text.trim().is_empty())),
            hidden_reference: Some(&self.hidden_reference),
        }
    }

    fn validate_plan_references(&self, wiki: &NextChapterWiki) -> Result<(), String> {
        if wiki.chapter_idx != self.chapter_idx { return Err("生成的下一章 Wiki 序号与请求不匹配".into()); }
        let history: Value = serde_json::from_str(&self.historical_wiki).map_err(|e| e.to_string())?;
        let allowed = history["entries"].as_array().ok_or("选中历史 Wiki 条目缺失")?.iter().filter_map(|entry| entry["id"].as_str()).collect::<std::collections::HashSet<_>>();
        if wiki.related_entry_ids.iter().any(|id| !allowed.contains(id.as_str())) { return Err("下一章规划引用了选中历史以外的条目".into()); }
        Ok(())
    }

    fn dependency_digest(&self, question_batch: Option<usize>) -> Result<String, String> {
        let input = self.input(question_batch);
        let request = if question_batch.is_some() {
            crate::core::story_plan::question_batch_request(&input, self.max_tokens)?
        } else {
            crate::core::story_plan::next_wiki_request(&input, self.max_tokens)?
        };
        // Fingerprint the actual messages/schema rather than a manually maintained
        // prompt version, including selected question prompts and option meanings.
        let messages = request.messages.iter().map(|message| json!({"role":message.role,"content":message.content})).collect::<Vec<_>>();
        let dependency = json!({
            "contract":"source-qualified-planner/v1", "provider":self.provider_dependency,
            "messages":messages, "schema":request.schema, "schema_name":request.schema_name,
            "temperature":request.temperature, "max_tokens":request.max_tokens,
            "line_version":self.line_version, "history_revision":self.history_revision,
            "previous_text_version":self.previous, "quick_read_revision":self.quick_read_revision,
            "original_target_id":self.original_target_id, "original_target_hash":self.original_target_hash,
            "interventions":self.interventions, "question_batches":self.batches,
        });
        Ok(crate::core::kg::digest(dependency.to_string().as_bytes()))
    }
}

fn load_planner_snapshot(conn: &Connection, book: i64, idx: i64) -> Result<Option<PlannerSnapshot>, String> {
    if idx < 2 { return Err("下一章规划必须有已选中的前章正文".into()); }
    let chapter_count:i64=conn.query_row("SELECT COUNT(*) FROM chapters WHERE book_id=?1",[book],|row|row.get(0)).map_err(|error|error.to_string())?;
    if idx>chapter_count{return Ok(None);}
    let (runtime_provider,runtime_config)=match crate::commands::kg::runtime_config(conn,None){Ok(value)=>value,Err(_)=>return Ok(None)};
    let Some(reference) = crate::store::quick_read::active_reference(conn, book)? else { return Ok(None); };
    if reference.provider_id!=runtime_provider.id||reference.model!=runtime_config.model_extract{return Ok(None);}
    // Local reads require all durable slices; a web outline is complete only as
    // its zero-slice, provenance-checked source kind and current search key permit.
    let (quick_read_revision, source_kind, complete): (i64, String, bool) = conn.query_row("SELECT id,source_kind,CASE source_kind WHEN 'local_full_text' THEN completed_chars=total_chars AND completed_slices=total_slices AND total_slices=(SELECT COUNT(*) FROM quick_read_slices s WHERE s.revision_id=quick_read_revisions.id AND s.status='ready') WHEN 'web_outline' THEN total_chars=0 AND completed_chars=0 AND total_slices=0 AND completed_slices=0 ELSE 0 END FROM quick_read_revisions WHERE book_id=?1 AND active=1 AND status='ready'", [book], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).map_err(|e| e.to_string())?;
    if !complete || (source_kind=="web_outline"&&!crate::commands::quick_read::web_reference_source_ready(conn,book)) { return Ok(None); }
    // Original reading and intervention persistence do not invoke this gate.
    for chapter in 1..=chapter_count.min(2) {
        let text=crate::store::kg::text_version(conn,book,chapter,"original",None)?;
        if !crate::store::kg::extraction_matches(conn,text.id,&runtime_config)?||crate::store::story_projection::original_reference_wiki(conn, book, chapter)?.is_none() { return Ok(None); }
    }
    let progress = crate::store::kg::progress(conn, book)?;
    if progress < idx - 1 { return Ok(None); }
    let Some(previous) = crate::store::kg::selected_text(conn, book, idx - 1)? else { return Ok(None); };
    let line_version = crate::store::kg::line_version(conn, book)?;
    let history = crate::store::story_projection::story_map(conn, book, idx - 1, line_version)?;
    if history["ready"] != true { return Ok(None); }
    let history_revision = history["wiki_revision"].as_i64().ok_or("历史 Wiki 版本缺失")?;
    let target = crate::store::kg::text_version(conn, book, idx, "original", None)?;
    let target_wiki = crate::store::story_projection::original_reference_wiki(conn, book, idx)?;
    let hidden_reference = json!({
        "eligibility":"original_source_reference_only",
        "usage_policy":"低优先级原作素材，不是选中阅读线已发生事实，不是角色已知信息。用户自由文字与有效答案优先；原作未来事件可改变。未被干预否定的核心身份、动机和设定仅作为连续性默认值。不得向公开问题或历史 Wiki 泄露尚未发生的原作信息。",
        "quick_read_revision":quick_read_revision, "quick_read_reference":reference,
        "original_target_text_version_id":target.id, "original_target_full_text":target.content,
        "original_target_wiki":target_wiki,
    }).to_string();
    let interventions = repo::active_interventions(conn, book, idx)?;
    // Earlier batches on this line retain the meanings of already answered
    // questions even though only the newest batch is marked active for display.
    let mut statement = conn.prepare("SELECT data_json FROM story_question_batches WHERE book_id=?1 AND chapter_idx=?2 AND reading_line_version=?3 ORDER BY batch_index,id").map_err(|e| e.to_string())?;
    let rows = statement.query_map(params![book,idx,line_version], |row| row.get::<_, String>(0)).map_err(|e| e.to_string())?;
    let batches = rows.map(|row| serde_json::from_str::<QuestionBatch>(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>()?;
    let known = batches.iter().flat_map(|batch| &batch.questions).flat_map(|question| &question.options).map(|option| option.id.as_str()).collect::<std::collections::HashSet<_>>();
    let normalize = |raw: &str| if known.contains(raw) { raw.to_owned() } else { raw.rsplit_once(':').map(|(_, id)| id.to_owned()).unwrap_or_else(|| raw.to_owned()) };
    let overridden = interventions.iter().flat_map(|item| &item.overridden_option_ids).map(|id| normalize(id)).collect::<std::collections::HashSet<_>>();
    let mut seen = std::collections::HashSet::new();
    let selected = interventions.iter().flat_map(|item| &item.selected_option_ids).map(|id| normalize(id)).filter(|id| known.contains(id.as_str()) && !overridden.contains(id) && seen.insert(id.clone())).collect();
    // Eligibility never resolves vault credentials or creates a client.
    let provider: Option<(i64, String, String, String, Option<String>, Option<String>, Option<String>, Option<String>)> = conn.query_row(
        "SELECT id,name,protocol,base_url,model_extract,model_write,model_merge,params_json FROM llm_providers WHERE model_extract IS NOT NULL AND trim(model_extract)<>'' ORDER BY id LIMIT 1", [],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
    ).optional().map_err(|e| e.to_string())?;
    let Some((id,name,protocol,base_url,model_extract,model_write,model_merge,raw_params)) = provider else { return Ok(None); };
    if model_merge.as_deref().is_none_or(|model| model.trim().is_empty()) { return Ok(None); }
    let provider_params: Value = raw_params.map(|raw| serde_json::from_str(&raw)).transpose().map_err(|e| format!("Provider 参数无效: {e}"))?.unwrap_or_else(|| json!({}));
    let output_reserve = match provider_params.get("output_reserve") {
        None => 4096,
        Some(value) => value.as_u64().filter(|value| *value > 0).ok_or("Provider output_reserve 必须为正整数")?,
    };
    let provider_dependency = json!({"id":id,"name":name,"protocol":protocol,"base_url":base_url,"model_extract":model_extract,"model_write":model_write,"model_merge":model_merge,"params":provider_params});
    Ok(Some(PlannerSnapshot { chapter_idx:idx, previous, historical_wiki:history.to_string(), hidden_reference, history_revision, line_version, quick_read_revision, original_target_id:target.id, original_target_hash:target.content_hash, interventions, batches, selected, provider_dependency, max_tokens:output_reserve.min(4096) as u32 }))
}

/// Stale plans never qualify merely because their database status is active.
pub(crate) fn eligible_next_wiki(conn: &Connection, book: i64, idx: i64) -> Result<Option<NextChapterWiki>, String> {
    let saved: Option<(i64, i64, String, String)> = conn.query_row(
        "SELECT previous_text_version_id,history_revision,dependency_digest,data_json FROM story_next_wiki_revisions WHERE book_id=?1 AND chapter_idx=?2 AND active=1 AND status='ready'",
        params![book,idx], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).optional().map_err(|e| e.to_string())?;
    let Some((previous_id, history_revision, digest, raw)) = saved else { return Ok(None); };
    let Some(snapshot) = load_planner_snapshot(conn, book, idx)? else { return Ok(None); };
    if previous_id != snapshot.previous.id || history_revision != snapshot.history_revision || digest != snapshot.dependency_digest(None)? { return Ok(None); }
    let wiki = crate::core::story_plan::decode_next_wiki(serde_json::from_str(&raw).map_err(|e| e.to_string())?)?;
    if snapshot.validate_plan_references(&wiki).is_err() { return Ok(None); }
    Ok(Some(wiki))
}

#[cfg(test)]
pub(crate) fn save_test_next_wiki(conn: &Connection, book: i64, idx: i64, wiki: &NextChapterWiki) -> Result<i64, String> {
    let snapshot = load_planner_snapshot(conn, book, idx)?.ok_or("速读、原作记忆或选中前章尚未就绪")?;
    snapshot.validate_plan_references(wiki)?;
    repo::save_next_wiki(conn, book, idx, snapshot.previous.id, snapshot.history_revision, &snapshot.dependency_digest(None)?, wiki)
}

fn cached_question_batch(conn: &Connection, book: i64, idx: i64, batch_index: usize, snapshot: &PlannerSnapshot) -> Result<Option<QuestionBatch>, String> {
    let saved: Option<(i64, Option<String>, String)> = conn.query_row(
        "SELECT reading_line_version,dependency_digest,data_json FROM story_question_batches WHERE book_id=?1 AND chapter_idx=?2 AND batch_index=?3 AND active=1",
        params![book,idx,batch_index as i64], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).optional().map_err(|e| e.to_string())?;
    let Some((line, Some(digest), raw)) = saved else { return Ok(None); };
    if line != snapshot.line_version || digest != snapshot.dependency_digest(Some(batch_index))? { return Ok(None); }
    serde_json::from_str(&raw).map(Some).map_err(|e| e.to_string())
}

fn save_question_batch_with_dependency(conn: &Connection, book: i64, idx: i64, line: i64, batch: &QuestionBatch) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    repo::save_question_batch(&tx, book, idx, line, batch)?;
    // The saved batch joins questionnaire history. Cache reuse compares the
    // resulting snapshot, not the pre-request snapshot which omitted this batch.
    let saved = load_planner_snapshot(&tx, book, idx)?.ok_or("规划依赖已失效，问题批次未激活")?;
    let digest = saved.dependency_digest(Some(batch.batch_index))?;
    tx.execute("UPDATE story_question_batches SET dependency_digest=?4 WHERE book_id=?1 AND chapter_idx=?2 AND question_batch_id=?3", params![book,idx,batch.question_batch_id,digest]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}
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
    let (snapshot, provider) = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let snapshot = load_planner_snapshot(conn, book_id, chapter_idx)?.ok_or("全书速读、前两章原作记忆及选中前章尚未就绪；仍可阅读原作和保存干预")?;
        if let Some(batch) = cached_question_batch(conn, book_id, chapter_idx, batch_index, &snapshot)? {
            return Ok(Some(question_batch_response(&batch, &snapshot.interventions)));
        }
        let (provider, _) = crate::commands::kg::runtime_config(conn, None)?;
        (snapshot, provider)
    };
    let dependency = snapshot.dependency_digest(Some(batch_index))?;
    let client = crate::commands::providers::make_client(provider)?;
    let batch = crate::core::story_plan::generate_question_batch(client.as_ref(), &snapshot.input(Some(batch_index)), snapshot.max_tokens).await?;
    let _commit = client.commit_guard().map_err(|e| e.to_string())?;
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    let current = load_planner_snapshot(conn, book_id, chapter_idx)?.ok_or("规划依赖已失效，请重新加载问题批次")?;
    if current.dependency_digest(Some(batch_index))? != dependency { return Err("规划依赖已变化，请重新加载问题批次".into()); }
    save_question_batch_with_dependency(conn, book_id, chapter_idx, snapshot.line_version, &batch)?;
    Ok(Some(question_batch_response(&batch, &current.interventions)))
}


async fn prepare_next_wiki(state: &tauri::State<'_, AppState>, input: &InterventionInput) -> Result<(), String> {
    let (snapshot, provider) = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        // Saving intent is allowed while preparation runs, but no premature plan
        // is requested or activated. The next questionnaire submission retries it.
        let Some(snapshot) = load_planner_snapshot(conn, input.book_id, input.chapter_idx)? else { return Ok(()); };
        let (provider, _) = crate::commands::kg::runtime_config(conn, None)?;
        (snapshot, provider)
    };
    let dependency = snapshot.dependency_digest(None)?;
    let client = crate::commands::providers::make_client(provider)?;
    let wiki = crate::core::story_plan::plan_next_chapter(client.as_ref(), &snapshot.input(None), snapshot.max_tokens).await?;
    snapshot.validate_plan_references(&wiki)?;
    let _commit = client.commit_guard().map_err(|e| e.to_string())?;
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    let current = load_planner_snapshot(conn, input.book_id, input.chapter_idx)?.ok_or("规划依赖已失效，规划结果未激活")?;
    if current.dependency_digest(None)? != dependency { return Err("规划依赖已变化，规划结果未激活".into()); }
    repo::save_next_wiki(conn, input.book_id, input.chapter_idx, snapshot.previous.id, snapshot.history_revision, &dependency, &wiki)?;
    Ok(())
}
#[tauri::command]
pub fn get_next_wiki(state: tauri::State<AppState>, book_id: i64, chapter_idx: i64) -> Result<Option<NextChapterWiki>, String> {
    let guard = state.db.lock();
    eligible_next_wiki(guard.as_ref().ok_or("数据库未初始化")?, book_id, chapter_idx)
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

    fn planner_database() -> (rusqlite::Connection, i64) {
        use crate::core::quick_read::*;
        use crate::store::{books::{create_book_with_chapters, NewChapter}, quick_read, Db};
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        Db::apply_pragmas(&conn).unwrap();
        Db::new(&conn).migrate().unwrap();
        conn.execute("INSERT INTO llm_providers(name,protocol,base_url,key_ref,model_extract,model_merge) VALUES ('planner','openai','https://example.test/v1','unused','reader','planner')", []).unwrap();crate::creds::save("unused","test-key").unwrap();
        let chapters = (1..=3).map(|idx| NewChapter { idx, title: format!("Chapter {idx}"), text: format!("Original {idx}"), token_est: 8 }).collect::<Vec<_>>();
        let book = create_book_with_chapters(&conn, "Planning", None, "txt", "memory", &chapters).unwrap();
        crate::store::kg::mark_read(&conn, book, 1).unwrap();
        let sources = quick_read::source_chapters(&conn, book).unwrap();
        let spans = sources.iter().map(|source| vec![SourceSpan { chapter_idx: source.chapter_idx, start_char: 0, end_char: source.char_count, content_hash: source.content_hash.clone() }]).collect::<Vec<_>>();
        let revision = quick_read::prepare_revision(&conn, book, &sources, "read/v1", 1, "reader", "read/v1", "read/v1", &spans).unwrap();
        let slices = sources.iter().enumerate().map(|(idx, source)| QuickReadSlice {
            slice_idx: idx, source_spans: spans[idx].clone(),
            chapter_summaries: vec![ChapterQuickRead { chapter_idx: source.chapter_idx, summary: format!("Original summary {}", source.chapter_idx), character_refs: vec![] }],
            characters: vec![], plot_notes: vec![],
        }).collect::<Vec<_>>();
        for slice in &slices { quick_read::save_slice(&conn, revision, slice, (1, 1)).unwrap(); }
        let reference = QuickReadReference {
            schema_version: "read/v1".into(), source_digest: quick_read::source_digest(&sources), source_kind: crate::core::quick_read::QuickReadSourceKind::LocalFullText, provider_id: 1, model: "reader".into(), prompt_version: "read/v1".into(),
            chapter_summaries: canonical_chapter_summaries(&slices),
            characters: vec![QuickReadCharacterCard { id: "original-person".into(), name: "Future identity".into(), aliases: vec![], identity_evidence: vec!["Original evidence".into()], motivations: vec!["Help".into()], narrative_roles: vec!["Lead".into()], first_appearance_chapter: 2, revelations: vec![], source_chapters: vec![2] }],
            plot_summary: "Original future plot".into(), plot_notes: vec![], completed_spans: spans.into_iter().flatten().collect(), source_provenance: vec![],
        };
        quick_read::publish_reference(&conn, revision, &reference).unwrap();
        original_memory(&conn, book, 1, "Actual first chapter");
        original_memory(&conn, book, 2, "Hidden original future");
        (conn, book)
    }

    fn original_memory(conn: &rusqlite::Connection, book: i64, idx: i64, summary: &str) {
        let text = crate::store::kg::text_version(conn, book, idx, "original", None).unwrap();
        let snapshot = crate::core::kg::ChapterSnapshot { summary: summary.into(), ..Default::default() };
        let (_,config)=crate::commands::kg::runtime_config(conn,None).unwrap();let line=crate::store::kg::line_version(conn,book).unwrap();
        crate::store::kg::commit_snapshot(conn,&text,&snapshot,"bootstrap",line,None,&config).unwrap();
        crate::store::story_projection::save_chapter_memory(conn, &text, &snapshot, summary).unwrap();
    }

    fn save_current_plan(conn: &rusqlite::Connection, book: i64) {
        let snapshot = super::load_planner_snapshot(conn, book, 2).unwrap().unwrap();
        let wiki = crate::core::story::NextChapterWiki { chapter_idx: 2, goals: vec!["Continue".into()], state_directions: vec![], open_threads: vec![], avoid: vec![], ending_direction: "Continue".into(), related_entry_ids: vec![] };
        super::repo::save_next_wiki(conn, book, 2, snapshot.previous.id, snapshot.history_revision, &snapshot.dependency_digest(None).unwrap(), &wiki).unwrap();
        assert!(super::eligible_next_wiki(conn, book, 2).unwrap().is_some());
    }

    #[test]
    fn planner_separates_actual_history_from_full_hidden_original_inputs() {
        let (conn, book) = planner_database();
        let snapshot = super::load_planner_snapshot(&conn, book, 2).unwrap().unwrap();
        let messages = crate::core::story_plan::build_planner_messages(&snapshot.input(None)).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&messages[1].content).unwrap();
        assert_eq!(payload["previous_selected_full_text"], "Original 1");
        assert!(!payload["historical_wiki"].to_string().contains("Hidden original future"));
        let hidden: serde_json::Value = serde_json::from_str(payload["hidden_reference"].as_str().unwrap()).unwrap();
        assert_eq!(hidden["original_target_full_text"], "Original 2");
        assert!(hidden["original_target_wiki"].to_string().contains("Hidden original future"));
        assert_eq!(hidden["quick_read_reference"]["characters"][0]["name"], "Future identity");
        let public = crate::store::story_projection::story_map(&conn, book, 1, 0).unwrap();
        assert!(!public.to_string().contains("Future identity"));
        assert!(!public.to_string().contains("Hidden original future"));
    }

    #[test]
    fn missing_quick_read_or_original_bootstrap_blocks_planning_not_original_read() {
        let (conn, book) = planner_database();
        conn.execute("UPDATE quick_read_revisions SET status='failed' WHERE book_id=?1", [book]).unwrap();
        assert!(super::load_planner_snapshot(&conn, book, 2).unwrap().is_none());
        assert_eq!(crate::store::kg::selected_text(&conn, book, 1).unwrap().unwrap().content, "Original 1");
        conn.execute("UPDATE quick_read_revisions SET status='ready' WHERE book_id=?1", [book]).unwrap();
        let target = crate::store::kg::text_version(&conn, book, 2, "original", None).unwrap();
        conn.execute("UPDATE story_chapter_revisions SET status='stale' WHERE text_version_id=?1", [target.id]).unwrap();
        assert!(super::load_planner_snapshot(&conn, book, 2).unwrap().is_none());
    }

    #[test]
    fn planner_rejects_current_provider_or_original_extraction_mismatch() {
        let (conn, book) = planner_database();
        conn.execute("UPDATE llm_providers SET model_extract='reader-v2'", []).unwrap();
        assert!(super::load_planner_snapshot(&conn, book, 2).unwrap().is_none());
        conn.execute("UPDATE llm_providers SET model_extract='reader',params_json='{\"context_limit\":65536,\"output_reserve\":4096}'", []).unwrap();
        assert!(super::load_planner_snapshot(&conn, book, 2).unwrap().is_none());
    }

    #[test]
    fn planner_does_not_lookup_nonexistent_target_chapter() {
        let (conn, book) = planner_database();
        assert!(super::load_planner_snapshot(&conn, book, 4).unwrap().is_none());
    }

    #[test]
    fn incomplete_quick_read_cannot_qualify_a_saved_plan_despite_ready_label() {
        let (conn, book) = planner_database();
        save_current_plan(&conn, book);
        conn.execute("UPDATE quick_read_slices SET status='failed' WHERE slice_idx=1", []).unwrap();
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_none());
        assert_eq!(crate::store::kg::selected_text(&conn, book, 1).unwrap().unwrap().content, "Original 1");
    }

    #[test]
    fn active_plan_is_ineligible_after_intervention_free_text_changes() {
        let (conn, book) = planner_database();
        save_current_plan(&conn, book);
        let input = InterventionInput { book_id: book, chapter_idx: 2, batch_index: None, question_batch_id: None, selected_option_ids: vec![], free_text: Some("Change the next direction".into()), overridden_option_ids: vec![], reading_line_version: 1 };
        super::repo::save_intervention(&conn, &input).unwrap();
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_none());
    }

    #[test]
    fn active_plan_is_ineligible_after_target_reference_revision_changes() {
        let (conn, book) = planner_database();
        save_current_plan(&conn, book);
        original_memory(&conn, book, 2, "Revised target memory");
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_none());
    }

    #[test]
    fn active_plan_is_ineligible_after_quick_read_reference_changes() {
        let (conn, book) = planner_database();
        save_current_plan(&conn, book);
        conn.execute("UPDATE quick_read_revisions SET plot_summary='Changed future reference' WHERE book_id=?1", [book]).unwrap();
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_none());
    }

    #[test]
    fn active_plan_is_ineligible_after_actual_history_changes_at_same_progress() {
        let (conn, book) = planner_database();
        save_current_plan(&conn, book);
        original_memory(&conn, book, 1, "Revised actual history");
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_none());
    }

    #[test]
    fn original_target_cannot_be_mutated_after_a_plan_is_saved() {
        let (conn, book) = planner_database();
        save_current_plan(&conn, book);
        let error = conn.execute("UPDATE chapters SET original_text='Changed target prose' WHERE book_id=?1 AND idx=2", [book]).unwrap_err();
        assert!(error.to_string().contains("immutable original text"));
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_some());
    }

    #[test]
    fn active_plan_is_ineligible_after_provider_model_changes() {
        let (conn, book) = planner_database();
        save_current_plan(&conn, book);
        conn.execute("UPDATE llm_providers SET model_merge='planner-v2'", []).unwrap();
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_none());
    }

    #[test]
    fn selected_question_meaning_change_invalidates_plan_even_with_same_option_id() {
        let (conn, book) = planner_database();
        let mut batch = QuestionBatch {
            batch_index: 0, question_batch_id: "semantic-batch".into(),
            questions: (0..3).map(|idx| Question { id: format!("q{idx}"), prompt: "Choose direction".into(), options: vec![QuestionOption { id: format!("yes{idx}"), label: "Help".into(), conflicts_with: vec![] }, QuestionOption { id: format!("no{idx}"), label: "Leave".into(), conflicts_with: vec![] }] }).collect(),
        };
        let line = crate::store::kg::line_version(&conn, book).unwrap();
        super::repo::save_question_batch(&conn, book, 2, line, &batch).unwrap();
        super::repo::save_intervention(&conn, &InterventionInput { book_id: book, chapter_idx: 2, batch_index: Some(0), question_batch_id: Some(batch.question_batch_id.clone()), selected_option_ids: vec!["q0:yes0".into()], free_text: None, overridden_option_ids: vec![], reading_line_version: line }).unwrap();
        save_current_plan(&conn, book);
        batch.questions[0].options[0].label = "Betray".into();
        super::repo::save_question_batch(&conn, book, 2, line, &batch).unwrap();
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_none());
    }

    #[test]
    fn third_chapter_planner_uses_exact_generated_predecessor_and_rejects_selection_switch() {
        let (conn, book) = planner_database();
        save_current_plan(&conn, book);
        let line = crate::store::kg::line_version(&conn, book).unwrap();
        let wiki = serde_json::json!({"headline":"Changed actual second chapter","narrative":"Rewritten events","ending":{"open_threads":[]},"entries":[],"open_threads":[]});
        let generated = super::repo::save_rewritten_chapter(&conn, book, 2, "Selected rewritten second chapter", "writer", "pair", line, &wiki).unwrap();
        crate::store::kg::mark_read(&conn, book, 2).unwrap();
        let snapshot = super::load_planner_snapshot(&conn, book, 3).unwrap().unwrap();
        assert_eq!(snapshot.previous.id, generated.id);
        assert_eq!(snapshot.input(None).previous_selected_text, "Selected rewritten second chapter");
        assert!(snapshot.historical_wiki.contains("Rewritten events"));
        assert!(!snapshot.historical_wiki.contains("Hidden original future"));
        assert!(snapshot.hidden_reference.contains("Original 3"));
        let plan = crate::core::story::NextChapterWiki { chapter_idx: 3, goals: vec![], state_directions: vec![], open_threads: vec![], avoid: vec![], ending_direction: "Continue".into(), related_entry_ids: vec![] };
        super::save_test_next_wiki(&conn, book, 3, &plan).unwrap();
        assert!(super::eligible_next_wiki(&conn, book, 3).unwrap().is_some());
        let original = crate::store::kg::text_version(&conn, book, 2, "original", None).unwrap();
        conn.execute("UPDATE kg_read_selection SET text_version_id=?3 WHERE book_id=?1 AND chapter_idx=?2", rusqlite::params![book,2,original.id]).unwrap();
        assert!(super::eligible_next_wiki(&conn, book, 3).unwrap().is_none());
    }

    #[test]
    fn pending_planner_commit_digest_detects_reference_change_without_line_change() {
        let (conn, book) = planner_database();
        let before = super::load_planner_snapshot(&conn, book, 2).unwrap().unwrap();
        let requested_plan = before.dependency_digest(None).unwrap();
        let requested_questions = before.dependency_digest(Some(0)).unwrap();
        original_memory(&conn, book, 2, "Target changed during model await");
        let after = super::load_planner_snapshot(&conn, book, 2).unwrap().unwrap();
        assert_eq!(before.line_version, after.line_version);
        assert_ne!(requested_plan, after.dependency_digest(None).unwrap());
        assert_ne!(requested_questions, after.dependency_digest(Some(0)).unwrap());
    }

    fn public_batch(index: usize) -> QuestionBatch {
        QuestionBatch { batch_index:index, question_batch_id:format!("b{index}"), questions:(0..3).map(|question| Question { id:format!("q{index}-{question}"), prompt:"Choose a direction".into(), options:vec![QuestionOption { id:format!("yes{index}-{question}"), label:"Help".into(), conflicts_with:vec![] },QuestionOption { id:format!("no{index}-{question}"), label:"Leave".into(), conflicts_with:vec![] }] }).collect() }
    }

    #[test]
    fn cached_questions_reject_same_line_reference_changes_and_legacy_missing_digest() {
        let (conn, book) = planner_database();
        let line = crate::store::kg::line_version(&conn, book).unwrap();
        let batch = public_batch(0);
        super::repo::save_question_batch(&conn, book, 2, line, &batch).unwrap();
        let legacy = super::load_planner_snapshot(&conn, book, 2).unwrap().unwrap();
        assert!(super::cached_question_batch(&conn, book, 2, 0, &legacy).unwrap().is_none());
        super::save_question_batch_with_dependency(&conn, book, 2, line, &batch).unwrap();
        let saved = super::load_planner_snapshot(&conn, book, 2).unwrap().unwrap();
        assert!(super::cached_question_batch(&conn, book, 2, 0, &saved).unwrap().is_some());
        original_memory(&conn, book, 2, "Changed target after cached questions");
        let changed = super::load_planner_snapshot(&conn, book, 2).unwrap().unwrap();
        assert_eq!(saved.line_version, changed.line_version);
        assert!(super::cached_question_batch(&conn, book, 2, 0, &changed).unwrap().is_none());
    }

    #[test]
    fn same_line_staged_answer_meanings_survive_but_prior_line_questions_are_excluded() {
        let (conn, book) = planner_database();
        let line = crate::store::kg::line_version(&conn, book).unwrap();
        super::repo::save_question_batch(&conn, book, 2, line, &public_batch(0)).unwrap();
        super::repo::save_question_batch(&conn, book, 2, line, &public_batch(1)).unwrap();
        let snapshot = super::load_planner_snapshot(&conn, book, 2).unwrap().unwrap();
        assert_eq!(snapshot.batches.len(), 2);
        conn.execute("UPDATE story_question_batches SET reading_line_version=?2 WHERE book_id=?1", rusqlite::params![book,line-1]).unwrap();
        let changed = super::load_planner_snapshot(&conn, book, 2).unwrap().unwrap();
        assert!(changed.batches.is_empty());
    }

    #[test]
    fn plans_cannot_reference_hidden_original_or_invented_history_ids() {
        let (conn, book) = planner_database();
        let snapshot = super::load_planner_snapshot(&conn, book, 2).unwrap().unwrap();
        let wiki = crate::core::story::NextChapterWiki { chapter_idx:2, goals:vec![], state_directions:vec![], open_threads:vec![], avoid:vec![], ending_direction:"Continue".into(), related_entry_ids:vec!["original-person".into()] };
        assert!(super::save_test_next_wiki(&conn, book, 2, &wiki).unwrap_err().contains("选中历史"));
        // Legacy/unvalidated storage is not sufficient to bypass public eligibility.
        super::repo::save_next_wiki(&conn, book, 2, snapshot.previous.id, snapshot.history_revision, &snapshot.dependency_digest(None).unwrap(), &wiki).unwrap();
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_none());
    }

    #[test]
    fn extract_only_provider_does_not_qualify_plans_or_block_intent_persistence() {
        let (conn, book) = planner_database();
        save_current_plan(&conn, book);
        conn.execute("UPDATE llm_providers SET model_merge=NULL", []).unwrap();
        assert!(super::load_planner_snapshot(&conn, book, 2).unwrap().is_none());
        assert!(super::eligible_next_wiki(&conn, book, 2).unwrap().is_none());
        let line = crate::store::kg::line_version(&conn, book).unwrap();
        let input = InterventionInput { book_id:book, chapter_idx:2, batch_index:None, question_batch_id:None, selected_option_ids:vec![], free_text:Some("Keep intent while planner is unavailable".into()), overridden_option_ids:vec![], reading_line_version:line };
        super::repo::save_intervention(&conn, &input).unwrap();
        assert_eq!(super::repo::active_interventions(&conn, book, 2).unwrap(), vec![input]);
    }
}
