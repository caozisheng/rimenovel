use crate::commands::AppState;
use crate::core::kg::{
    digest, validate_bundle, ChapterBundle, GraphQuery, GraphView, Identity, IdentityLink,
    RuntimeConfig, Span, StageA, StageB, TextVersion,
};
use crate::llm::{cache, ChatMessage, LlmClient, StructuredRequest, TaskKind};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

const STAGE_A_VERSION: &str = "kg-stage-a/v1";
const STAGE_B_VERSION: &str = "kg-stage-b/v1";
const IDENTITY_VERSION: &str = "kg-identity/v1";
const ADJUDICATION_VERSION: &str = "kg-adjudication/v1";
const SAFETY_RESERVE: usize = 512;

static STAGE_A_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../schemas/kg-stage-a.schema.json"))
        .expect("kg stage A schema is valid JSON")
});
static STAGE_B_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../schemas/kg-stage-b.schema.json"))
        .expect("kg stage B schema is valid JSON")
});
static IDENTITY_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../schemas/kg-identity.schema.json"))
        .expect("kg identity schema is valid JSON")
});
static ADJUDICATION_SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../schemas/kg-adjudication.schema.json"))
        .expect("kg adjudication schema is valid JSON")
});
static REDUCE_SCHEMA: LazyLock<Value> = LazyLock::new(|| json!({
    "type": "object",
    "additionalProperties": false,
    "required": ["synopsis", "themes", "style_guide"],
    "properties": {
        "synopsis": {"type": "string"},
        "themes": {"type": "array", "items": {"type": "string"}},
        "style_guide": {"type": "string"}
    }
}));

pub fn stage_a_schema() -> &'static Value { &STAGE_A_SCHEMA }
pub fn stage_b_schema() -> &'static Value { &STAGE_B_SCHEMA }
pub fn identity_schema() -> &'static Value { &IDENTITY_SCHEMA }
pub fn adjudication_schema() -> &'static Value { &ADJUDICATION_SCHEMA }

pub fn kg_stage_a() -> &'static str { fixtures::KG_STAGE_A }
pub fn kg_stage_b() -> &'static str { fixtures::KG_STAGE_B }
pub fn kg_identity() -> &'static str { fixtures::KG_IDENTITY }
pub fn kg_adjudication() -> &'static str { fixtures::KG_ADJUDICATION }

pub mod fixtures {
    pub const KG_STAGE_A: &str = "kg_stage_a";
    pub const KG_STAGE_B: &str = "kg_stage_b";
    pub const KG_IDENTITY: &str = "kg_identity";
    pub const KG_ADJUDICATION: &str = "kg_adjudication";
}

#[derive(Clone, Debug)]
struct SpanBlock {
    spans: Vec<Span>,
    text: String,
}

#[derive(Clone, Debug)]
pub struct IdentityResolution {
    pub identities: Vec<Identity>,
    pub mention_keys: HashMap<String, String>,
    pub unresolved: Vec<String>,
    pub links: Vec<IdentityLink>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityReply {
    schema_version: String,
    decisions: Vec<IdentityDecision>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityDecision {
    mention_id: String,
    decision: String,
    target_key: Option<String>,
    evidence_refs: Vec<String>,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdjudicationReply {
    schema_version: String,
    clusters: Vec<AdjudicationDecision>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdjudicationDecision {
    cluster_key: String,
    decision: String,
    keep_keys: Vec<String>,
    unresolved: bool,
    reason: String,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReductionReply {
    synopsis: String,
    themes: Vec<String>,
    style_guide: String,
}

fn validate_schema(schema: &Value, value: &Value) -> Result<(), String> {
    crate::llm::compile_schema(schema)
        .validate(value)
        .map_err(|err| format!("structured response violates schema: {err}"))
}

fn valid_refs(refs: &[String], spans: &[Span]) -> Result<(), String> {
    let available: HashSet<&str> = spans.iter().map(|span| span.key.as_str()).collect();
    if refs.is_empty() || refs.iter().any(|key| !available.contains(key.as_str())) {
        return Err("foreign or missing evidence reference".into());
    }
    Ok(())
}

pub fn decode_stage_a(value: Value, text: &TextVersion, spans: &[Span]) -> Result<StageA, String> {
    validate_schema(stage_a_schema(), &value)?;
    let stage: StageA = serde_json::from_value(value).map_err(|err| err.to_string())?;
    if stage.schema_version != STAGE_A_VERSION || stage.chapter_idx != text.chapter_idx {
        return Err("stage A version or chapter does not match text version".into());
    }
    for mention in &stage.mentions {
        valid_refs(&mention.evidence_refs, spans)?;
    }
    for event in &stage.events {
        if event.chapter_idx != text.chapter_idx {
            return Err("stage A event belongs to another chapter".into());
        }
        valid_refs(&event.evidence_refs, spans)?;
    }
    Ok(stage)
}

pub fn decode_stage_b(value: Value, text: &TextVersion, spans: &[Span]) -> Result<StageB, String> {
    validate_schema(stage_b_schema(), &value)?;
    let stage: StageB = serde_json::from_value(value).map_err(|err| err.to_string())?;
    if stage.schema_version != STAGE_B_VERSION || stage.chapter_idx != text.chapter_idx {
        return Err("stage B version or chapter does not match text version".into());
    }
    for fact in &stage.facts { valid_refs(&fact.evidence_refs, spans)?; }
    for relation in &stage.relations { valid_refs(&relation.evidence_refs, spans)?; }
    for change in &stage.changes { valid_refs(&change.evidence_refs, spans)?; }
    for precondition in &stage.preconditions { valid_refs(&precondition.evidence_refs, spans)?; }
    Ok(stage)
}

fn estimate_tokens(text: &str) -> usize {
    // CJK is near one token/character; ASCII prose is usually denser. Deliberately conservative.
    text.chars().count()
}

fn schema_tokens(schema: &Value) -> usize {
    estimate_tokens(&serde_json::to_string(schema).expect("schema serializes"))
}

fn request_fits(config: &RuntimeConfig, schema: &Value, messages: &[ChatMessage]) -> Result<(), String> {
    let reserve = config.output_reserve.max(1);
    let used = schema_tokens(schema)
        + messages.iter().map(|message| estimate_tokens(&message.content)).sum::<usize>()
        + reserve
        + SAFETY_RESERVE;
    if used > config.context_limit {
        return Err(format!("structured request needs {used} tokens but provider context limit is {}", config.context_limit));
    }
    Ok(())
}

fn span_key(chapter_idx: i64, start: usize, end: usize) -> String {
    format!("sp-{chapter_idx}-{start}-{end}")
}

fn last_boundary(text: &str, start: usize, limit: usize) -> usize {
    let mut end = (start + limit).min(text.len());
    while end > start && !text.is_char_boundary(end) { end -= 1; }
    if end <= start { return start; }
    if end == text.len() { return end; }
    let mut selected = start;
    for (offset, ch) in text[start..end].char_indices() {
        let absolute = start + offset + ch.len_utf8();
        if matches!(ch, '\n' | '。' | '！' | '？' | '；' | '.' | '!' | '?' | ';') {
            selected = absolute;
        }
    }
    if selected > start { selected } else { end }
}

/// Preallocates only source-grounded UTF-8 spans. Paragraphs are kept whole whenever possible;
/// an oversized paragraph is split on a natural boundary without changing chapter ownership.
pub fn prepare_spans(text: &TextVersion, max_bytes: usize) -> Result<Vec<Span>, String> {
    if max_bytes == 0 { return Err("evidence span budget must be positive".into()); }
    if text.content.is_empty() { return Err("cannot extract an empty chapter".into()); }
    let mut spans = Vec::new();
    let mut paragraph_start = 0;
    let mut cursor = 0;
    for line in text.content.split_inclusive('\n') {
        cursor += line.len();
        if line.trim().is_empty() {
            if paragraph_start < cursor {
                append_natural_spans(text, paragraph_start, cursor, max_bytes, &mut spans)?;
            }
            paragraph_start = cursor;
        }
    }
    if paragraph_start < text.content.len() {
        append_natural_spans(text, paragraph_start, text.content.len(), max_bytes, &mut spans)?;
    }
    if spans.is_empty() {
        append_natural_spans(text, 0, text.content.len(), max_bytes, &mut spans)?;
    }
    Ok(spans)
}

fn append_natural_spans(
    text: &TextVersion,
    mut start: usize,
    end: usize,
    max_bytes: usize,
    output: &mut Vec<Span>,
) -> Result<(), String> {
    while start < end {
        let next = if end - start <= max_bytes { end } else { last_boundary(&text.content, start, max_bytes) };
        if next <= start || !text.content.is_char_boundary(start) || !text.content.is_char_boundary(next) {
            return Err("unable to form a valid UTF-8 evidence span".into());
        }
        output.push(Span {
            key: span_key(text.chapter_idx, start, next),
            chapter_idx: text.chapter_idx,
            start_byte: start,
            end_byte: next,
            text: text.content[start..next].to_owned(),
        });
        start = next;
    }
    Ok(())
}

fn make_block(spans: Vec<Span>) -> SpanBlock {
    let text = spans.iter().map(|span| format!("[{}]\n{}", span.key, span.text)).collect::<Vec<_>>().join("\n");
    SpanBlock { spans, text }
}

fn budget_blocks(
    spans: &[Span],
    config: &RuntimeConfig,
    schema: &Value,
    prompt: impl Fn(&SpanBlock) -> Vec<ChatMessage>,
) -> Result<Vec<SpanBlock>, String> {
    let mut blocks = Vec::new();
    let mut current: Vec<Span> = Vec::new();
    for span in spans {
        let mut proposed = current.clone();
        proposed.push(span.clone());
        let block = make_block(proposed);
        if request_fits(config, schema, &prompt(&block)).is_ok() {
            current = block.spans;
        } else {
            if current.is_empty() {
                return Err(format!("evidence span {} and its required identity/state dependencies exceed provider context", span.key));
            }
            blocks.push(make_block(std::mem::take(&mut current)));
            let single = make_block(vec![span.clone()]);
            request_fits(config, schema, &prompt(&single))?;
            current.push(span.clone());
        }
    }
    if !current.is_empty() { blocks.push(make_block(current)); }
    Ok(blocks)
}

fn normalize_name(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_whitespace()).flat_map(char::to_lowercase).collect()
}

fn identity_names(identity: &Identity) -> impl Iterator<Item = &str> {
    std::iter::once(identity.name.as_str()).chain(identity.aliases.iter().map(String::as_str))
}

fn eligible_identities(known: &[Identity], name: &str, entity_type: &str) -> Vec<Identity> {
    let needle = normalize_name(name);
    known.iter()
        .filter(|identity| identity.entity_type == entity_type)
        .filter(|identity| identity_names(identity).any(|candidate| normalize_name(candidate) == needle))
        .cloned()
        .collect()
}

fn mention_candidates(known: &[Identity], mention: &crate::core::kg::Mention) -> Vec<Identity> {
    let mut candidates = eligible_identities(known, &mention.name, &mention.entity_type);
    for identity in known {
        if identity.entity_type == mention.entity_type && mention.candidate_keys.contains(&identity.key)
            && !candidates.iter().any(|candidate| candidate.key == identity.key) {
            candidates.push(identity.clone());
        }
    }
    candidates
}

fn local_identity_key(text: &TextVersion, mention_name: &str, entity_type: &str, refs: &[String]) -> String {
    let seed = format!("{}:{}:{}:{}:{}", text.book_id, text.content_hash, entity_type, normalize_name(mention_name), refs.join(","));
    format!("id-{}", &digest(seed.as_bytes())[..24])
}

/// Literal name/type matches are retrieval candidates, never identity proof. Local allocation
/// remains separate until an evidence-grounded identity decision confirms a link.
pub fn resolve_local_identities(
    text: &TextVersion,
    stage: &StageA,
    known: &[Identity],
    _prior: &[Identity],
) -> IdentityResolution {
    let mut identities = BTreeMap::<String, Identity>::new();
    let mut mention_keys = HashMap::new();
    let mut unresolved = Vec::new();
    for mention in &stage.mentions {
        let candidates = mention_candidates(known, mention);
        let key = local_identity_key(text, &mention.name, &mention.entity_type, &mention.evidence_refs);
        identities.entry(key.clone()).or_insert_with(|| Identity {
            key: key.clone(), entity_type: mention.entity_type.clone(), name: mention.name.clone(), aliases: Vec::new(),
        });
        if !candidates.is_empty() {
            unresolved.push(format!("identity:{} requires evidence-based decision", mention.mention_id));
        }
        mention_keys.insert(mention.mention_id.clone(), key);
    }
    IdentityResolution { identities: identities.into_values().collect(), mention_keys, unresolved, links: Vec::new() }
}

fn event_evidence_overlap(left: &[String], right: &[String]) -> usize {
    fn bounds(key:&str) -> Option<(usize,usize)> {
        let mut parts = key.rsplit('-');
        let end = parts.next()?.parse().ok()?;
        let start = parts.next()?.parse().ok()?;
        Some((start,end))
    }
    left.iter().map(|left_key| {
        right.iter().map(|right_key| {
            if left_key == right_key { return 1; }
            match (bounds(left_key), bounds(right_key)) {
                (Some((left_start,left_end)),Some((right_start,right_end))) => left_end.min(right_end).saturating_sub(left_start.max(right_start)),
                _ => 0,
            }
        }).max().unwrap_or(0)
    }).sum()
}

/// Refined records retain an existing event key solely by evidence overlap, never because a model
/// repeated a temporary local key. A persisted event may be claimed only once.
pub fn rewrite_refined_event_keys(
    fresh: Vec<(String, Vec<String>)>,
    previous: Vec<(String, Vec<String>)>,
) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let mut used = HashSet::new();
    for (fresh_key, fresh_refs) in fresh {
        let choice = previous.iter()
            .filter(|(prior_key, _)| !used.contains(prior_key))
            .map(|(prior_key, prior_refs)| (prior_key, event_evidence_overlap(&fresh_refs, prior_refs)))
            .filter(|(_, overlap)| *overlap > 0)
            .max_by_key(|(_, overlap)| *overlap);
        if let Some((prior_key, _)) = choice {
            used.insert(prior_key.clone());
            result.insert(fresh_key, prior_key.clone());
        }
    }
    result
}

fn stable_record_key(kind: &str, text: &TextVersion, fields: &[String], refs: &[String]) -> String {
    let mut refs = refs.to_vec();
    refs.sort();
    let seed = format!("{}:{}:{}:{}:{}", text.book_id, text.content_hash, kind, fields.join("\u{1f}"), refs.join(","));
    format!("{kind}-{}", &digest(seed.as_bytes())[..24])
}

fn stage_a_system(config: &RuntimeConfig) -> String {
    format!("You extract only source-grounded chapter evidence. Prompt version: {}. Return {}. Use only supplied span keys; mention IDs are local and events must reference those mention IDs. Do not invent offsets, final identity keys, or facts outside the supplied block.", config.prompt_version, STAGE_A_VERSION)
}

fn stage_b_system(config: &RuntimeConfig) -> String {
    format!("You extract source-grounded facts, relations, state changes, and evidenced event preconditions. Prompt version: {}. Return {}. Use only supplied stable identity/event keys and span keys. A precondition is necessary for the named event, not mere presence. Mark uncertainty in unresolved; do not emit unsupported hard updates.", config.prompt_version, STAGE_B_VERSION)
}

fn render_identities(identities: &[Identity], body: &str) -> Vec<Identity> {
    let normal = normalize_name(body);
    identities.iter().filter(|identity| identity_names(identity).any(|name| normal.contains(&normalize_name(name)))).cloned().collect()
}

fn render_state(view: &GraphView, relevant: &HashSet<String>) -> String {
    let nodes: Vec<_> = view.nodes.iter().filter(|node| relevant.contains(&node.key)).collect();
    let edges: Vec<_> = view.edges.iter().filter(|edge| relevant.contains(&edge.src) || relevant.contains(&edge.dst)).collect();
    serde_json::to_string(&json!({"nodes": nodes, "edges": edges})).expect("state serializes")
}

async fn structured_cached(
    state: &AppState,
    client: &dyn LlmClient,
    config: &RuntimeConfig,
    book_id: i64,
    task: TaskKind,
    schema: &'static Value,
    schema_name: &'static str,
    messages: Vec<ChatMessage>,
) -> Result<Value, String> {
    client.check_cancelled().map_err(|error| error.to_string())?;
    request_fits(config, schema, &messages)?;
    let max_tokens = u32::try_from(config.output_reserve).map_err(|_| "输出预算过大")?;
    let request = StructuredRequest { task, messages, schema, schema_name, temperature: 0.0, max_tokens };
    let key = cache::cache_key(config.provider_id, &config.model_extract, &request)
        .ok_or("deterministic structured request unexpectedly bypassed cache")?;
    {
        let db = state.db.lock();
        let conn = db.as_ref().ok_or("database is unavailable")?;
        if let Some(value) = cache::cache_get(conn, &key).map_err(|err| err.to_string())? {
            return Ok(value);
        }
    }
    let response = client.structured_with_usage(request).await.map_err(|err| err.to_string())?;
    {
        let _commit = client.commit_guard().map_err(|error| error.to_string())?;
        let db = state.db.lock();
        let conn = db.as_ref().ok_or("database is unavailable")?;
        cache::cache_put(conn, &key, &response.value, response.usage).map_err(|err| err.to_string())?;
        cache::record_usage(conn, config.provider_id, Some(book_id), task, response.usage).map_err(|err| err.to_string())?;
    }
    Ok(response.value)
}

async fn resolve_ambiguous_identities(
    state: &AppState,
    client: &dyn LlmClient,
    text: &TextVersion,
    config: &RuntimeConfig,
    stage: &StageA,
    known: &[Identity],
    spans: &[Span],
    resolution: &mut IdentityResolution,
) -> Result<(), String> {
    for mention in &stage.mentions {
        let candidates = mention_candidates(known, mention);
        if candidates.is_empty() { continue; }
        let local_spans: Vec<_> = spans.iter().filter(|span| mention.evidence_refs.contains(&span.key)).cloned().collect();
        let messages = vec![
            ChatMessage { role: "system".into(), content: format!("Resolve one entity identity cluster only; return {}. A literal name match is not proof. Choose same only when supplied context proves identity; otherwise separate or unresolved. Exactly one decision for the requested mention.", IDENTITY_VERSION) },
            ChatMessage { role: "user".into(), content: serde_json::to_string(&json!({"chapter_idx": text.chapter_idx, "source": text.source, "ambiguous_mentions": [{"mention": mention, "candidates": candidates}], "spans": local_spans})).expect("identity prompt serializes") },
        ];
        let value = structured_cached(state, client, config, text.book_id, TaskKind::Extract, identity_schema(), fixtures::KG_IDENTITY, messages).await?;
        validate_schema(identity_schema(), &value)?;
        let reply: IdentityReply = serde_json::from_value(value).map_err(|err| err.to_string())?;
        if reply.schema_version != IDENTITY_VERSION || reply.decisions.len() != 1 { return Err("identity must decide exactly one requested mention".into()); }
        let decision = reply.decisions.into_iter().next().expect("one identity decision");
        if decision.mention_id != mention.mention_id { return Err("identity decision references a different mention".into()); }
        valid_refs(&decision.evidence_refs, &local_spans)?;
        resolution.unresolved.retain(|item| !item.starts_with(&format!("identity:{} requires", mention.mention_id)));
        match decision.decision.as_str() {
            "same" => {
                let key = decision.target_key.as_deref().ok_or("identity same decision lacks target")?;
                if !candidates.iter().any(|candidate| candidate.key == key) { return Err("identity decision selected ineligible target".into()); }
                let local = resolution.mention_keys[&mention.mention_id].clone();
                resolution.mention_keys.insert(mention.mention_id.clone(), key.to_owned());
                if local != key { resolution.links.push(IdentityLink { from_key: local, to_key: key.to_owned(), observed_chapter: text.chapter_idx, observed_order: 0, evidence_refs: decision.evidence_refs }); }
            }
            "separate" => { if decision.target_key.is_some() { return Err("separate identity decision must have null target".into()); } }
            "unresolved" => {
                if decision.target_key.is_some() { return Err("unresolved identity decision must have null target".into()); }
                resolution.unresolved.push(format!("identity:{}: {}", mention.mention_id, decision.reason));
            }
            _ => return Err("unknown identity decision".into()),
        }
    }
    Ok(())
}

fn resolve_stage_a(mut stage: StageA, resolution: &IdentityResolution) -> Result<StageA, String> {
    for mention in &mut stage.mentions {
        let key = resolution.mention_keys.get(&mention.mention_id).ok_or("unresolved mention key")?.clone();
        mention.candidate_keys = vec![key];
    }
    for event in &mut stage.events {
        event.participants = event.participants.iter().map(|key| resolution.mention_keys.get(key).cloned().ok_or_else(|| "event uses unknown mention".to_owned())).collect::<Result<_,_>>()?;
        event.location = event.location.as_ref().map(|key| resolution.mention_keys.get(key).cloned().ok_or_else(|| "event uses unknown location mention".to_owned())).transpose()?;
    }
    Ok(stage)
}

fn rekey_stage_a(text: &TextVersion, stage: &mut StageA, old: Option<&ChapterBundle>) -> Result<(),String> {
    let local_to_stable: HashMap<_, _> = stage.events.iter().map(|event| {
        (event.key.clone(), stable_record_key("ev", text, &[event.title.clone(), event.category.clone(), event.summary.clone()], &event.evidence_refs))
    }).collect();
    for event in &mut stage.events {
        event.key = local_to_stable[&event.key].clone();
        event.cause_keys = event.cause_keys.iter().map(|key| local_to_stable.get(key).cloned().ok_or_else(|| "event cause uses foreign local key".to_owned())).collect::<Result<_,_>>()?;
    }
    if let Some(old) = old {
        let preserved = rewrite_refined_event_keys(
            stage.events.iter().map(|event| (event.key.clone(), event.evidence_refs.clone())).collect(),
            old.stage_a.events.iter().map(|event| (event.key.clone(), event.evidence_refs.clone())).collect(),
        );
        let mut replacements = Vec::new();
        for event in &mut stage.events {
            if let Some(old_key) = preserved.get(&event.key) {
                let current = event.key.clone();
                event.key = old_key.clone();
                replacements.push((current, old_key.clone()));
            }
        }
        for peer in &mut stage.events {
            for cause in &mut peer.cause_keys {
                if let Some((_, replacement)) = replacements.iter().find(|(current, _)| current == cause) {
                    *cause = replacement.clone();
                }
            }
        }
    }
    Ok(())
}

fn stage_a_prompt(block: &SpanBlock, known: &[Identity], config: &RuntimeConfig, chapter_idx: i64) -> Vec<ChatMessage> {
    let relevant = render_identities(known, &block.text);
    vec![
        ChatMessage { role: "system".into(), content: stage_a_system(config) },
        ChatMessage { role: "user".into(), content: serde_json::to_string(&json!({"chapter_idx": chapter_idx, "source": "immutable_chapter_text", "identity_registry": relevant, "source_spans": block.spans, "source_text": block.text, "seam": "context only; do not output records for it"})).expect("stage A prompt serializes") },
    ]
}

fn merge_stage_a(text: &TextVersion, stages: Vec<StageA>) -> Result<StageA, String> {
    let mut result = StageA { schema_version: STAGE_A_VERSION.into(), chapter_idx: text.chapter_idx, summary: String::new(), open_threads: Vec::new(), mentions: Vec::new(), events: Vec::new() };
    let mut mention_ids = HashSet::new();
    let mut event_ids = HashSet::new();
    for mut stage in stages {
        if !result.summary.is_empty() && !stage.summary.is_empty() { result.summary.push('\n'); }
        result.summary.push_str(&stage.summary);
        result.open_threads.append(&mut stage.open_threads);
        let mut mention_rewrites = HashMap::new();
        for mut mention in stage.mentions {
            let original = mention.mention_id.clone();
            if !mention_ids.insert(original.clone()) {
                mention.mention_id = format!("m-{}", &digest(format!("{}:{}", original, mention.evidence_refs.join(",")).as_bytes())[..16]);
                mention_rewrites.insert(original, mention.mention_id.clone());
            }
            result.mentions.push(mention);
        }
        for mut event in stage.events {
            event.participants = event.participants.into_iter().map(|key| mention_rewrites.get(&key).cloned().unwrap_or(key)).collect();
            event.location = event.location.map(|key| mention_rewrites.get(&key).cloned().unwrap_or(key));
            if !event_ids.insert(event.key.clone()) {
                event.key = format!("event-{}", &digest(format!("{}:{}", event.key, event.evidence_refs.join(",")).as_bytes())[..16]);
            }
            result.events.push(event);
        }
    }
    result.open_threads.sort(); result.open_threads.dedup();
    Ok(result)
}

fn stage_b_prompt(
    text: &TextVersion,
    block: &SpanBlock,
    stage_a: &StageA,
    identities: &[Identity],
    prior: &GraphView,
    hard_prior: &crate::core::kg::ReducedState,
    config: &RuntimeConfig,
) -> Vec<ChatMessage> {
    let refs: HashSet<_> = block.spans.iter().map(|span| span.key.as_str()).collect();
    let events: Vec<_> = stage_a.events.iter().filter(|event| event.evidence_refs.iter().any(|reference| refs.contains(reference.as_str()))).collect();
    let mut relevant: HashSet<_> = events.iter().flat_map(|event| event.participants.iter().cloned().chain(event.location.iter().cloned())).collect();
    relevant.extend(stage_a.mentions.iter().filter(|mention| mention.evidence_refs.iter().any(|reference| refs.contains(reference.as_str()))).flat_map(|mention| mention.candidate_keys.iter().cloned()));
    let identities: Vec<_> = identities.iter().filter(|identity| relevant.contains(&identity.key)).collect();
    let hard_values: Vec<_> = hard_prior.values.iter().filter(|((subject, _), _)| relevant.contains(subject)).map(|((subject, predicate), value)| json!({"subject_key":subject,"predicate":predicate,"value":value})).collect();
    let relation_registry: Vec<_> = hard_prior.relations.values().filter(|relation| relevant.contains(&relation.src) || relevant.contains(&relation.dst)).collect();
    vec![
        ChatMessage { role: "system".into(), content: stage_b_system(config) },
        ChatMessage { role: "user".into(), content: serde_json::to_string(&json!({"chapter_idx": text.chapter_idx, "source": text.source, "source_spans": block.spans, "mention_identity_map": stage_a.mentions.iter().filter_map(|mention| mention.candidate_keys.first().map(|key| (&mention.mention_id, key))).collect::<BTreeMap<_, _>>(), "events": events, "identities": identities, "prior_state": render_state(prior, &relevant), "hard_values":hard_values,"relation_registry":relation_registry})).expect("stage B prompt serializes") },
    ]
}

fn rekey_stage_b(text: &TextVersion, stage: &mut StageB, hard_prior: &crate::core::kg::ReducedState) -> Result<(),String> {
    let mut relation_map = HashMap::new();
    for relation in &mut stage.relations {
        let local_key = relation.key.clone();
        if let Some(prior) = hard_prior.relations.get(&local_key) {
            if prior.src != relation.src || prior.dst != relation.dst || prior.rel_category != relation.rel_category || prior.rel_type != relation.rel_type {
                return Err("existing relation identity endpoint/category/type mutation".into());
            }
        } else {
            relation.key = format!("rel-{}", &digest(format!("{}:{}:{}:{}:{}:{}", text.book_id, local_key, relation.src, relation.dst, relation.rel_category, relation.rel_type).as_bytes())[..24]);
        }
        relation_map.insert(local_key, relation.key.clone());
    }
    for fact in &mut stage.facts { fact.key = stable_record_key("fact", text, &[fact.subject_key.clone(), fact.predicate.clone(), fact.value.kind.clone(), fact.value.value.clone().unwrap_or_default()], &fact.evidence_refs); }
    for change in &mut stage.changes {
        if change.predicate == "status" { if let Some(key) = relation_map.get(&change.subject_key) { change.subject_key = key.clone(); } }
        change.key = stable_record_key("chg", text, &[change.event_key.clone(), change.subject_key.clone(), change.predicate.clone(), change.operation.clone()], &change.evidence_refs);
    }
    for precondition in &mut stage.preconditions {
        if precondition.predicate == "status" { if let Some(key) = relation_map.get(&precondition.subject_key) { precondition.subject_key = key.clone(); } }
        precondition.key = stable_record_key("pre", text, &[precondition.event_key.clone(), precondition.subject_key.clone(), precondition.predicate.clone(), precondition.value.kind.clone(), precondition.value.value.clone().unwrap_or_default()], &precondition.evidence_refs);
    }
    Ok(())
}

fn merge_stage_b(text: &TextVersion, stages: Vec<StageB>) -> Result<StageB,String> {
    let mut result = StageB { schema_version: STAGE_B_VERSION.into(), chapter_idx: text.chapter_idx, facts: Vec::new(), relations: Vec::new(), changes: Vec::new(), preconditions: Vec::new(), unresolved: Vec::new() };
    let mut records = HashMap::<String,Value>::new();
    fn fresh<T:serde::Serialize>(records:&mut HashMap<String,Value>,kind:&str,key:&str,record:&T)->Result<bool,String> {
        let value = serde_json::to_value(record).map_err(|error| error.to_string())?;
        let owned_key = format!("{kind}:{key}");
        if let Some(prior)=records.get(&owned_key) {
            if prior != &value { return Err("conflicting records share a stable key across blocks".into()); }
            Ok(false)
        } else { records.insert(owned_key,value); Ok(true) }
    }
    for stage in stages {
        for record in stage.facts { if fresh(&mut records,"fact",&record.key,&record)? { result.facts.push(record); } }
        for record in stage.relations { if fresh(&mut records,"relation",&record.key,&record)? { result.relations.push(record); } }
        for record in stage.changes { if fresh(&mut records,"change",&record.key,&record)? { result.changes.push(record); } }
        for record in stage.preconditions { if fresh(&mut records,"precondition",&record.key,&record)? { result.preconditions.push(record); } }
        result.unresolved.extend(stage.unresolved);
    }
    result.unresolved.sort(); result.unresolved.dedup();
    Ok(result)
}

pub fn semantic_conflict_clusters(stage: &StageB, prior: &crate::core::kg::ReducedState) -> Vec<(String, Vec<String>, Vec<String>)> {
    let mut groups: BTreeMap<(String, String), (Vec<String>, Vec<String>)> = BTreeMap::new();
    let mut values = prior.values.clone();
    for relation in prior.relations.values() {
        values.insert((relation.key.clone(), "status".into()), crate::core::kg::TypedValue { kind:"scalar".into(), value:Some(relation.status.clone()) });
    }
    let mut observations = Vec::new();
    for (index, fact) in stage.facts.iter().enumerate() {
        if fact.temporal_scope == "current" && fact.assertion_kind == "confirmed" {
            observations.push((fact.observed_order, 1u8, index));
        }
    }
    for (index, change) in stage.changes.iter().enumerate() {
        if change.temporal_scope == "current" && change.assertion_kind == "confirmed" {
            observations.push((change.observed_order, 0u8, index));
        }
    }
    observations.sort();
    let mut same_position: HashMap<(String,String,i64),(crate::core::kg::TypedValue,String,Vec<String>)> = HashMap::new();
    let mut discontinuities = HashSet::new();
    for (order, kind, index) in observations {
        if kind == 0 {
            let change = &stage.changes[index];
            let slot = (change.subject_key.clone(), change.predicate.clone());
            if !matches!(change.predicate.as_str(), "alias" | "affiliation") {
                if change.before.kind != "unknown" && values.get(&slot).is_some_and(|value| value != &change.before) {
                    let cluster = groups.entry(slot.clone()).or_default();
                    cluster.0.push(change.key.clone()); cluster.1.extend(change.evidence_refs.clone());
                    discontinuities.insert((slot.0.clone(), slot.1.clone(), order));
                } else { values.insert(slot.clone(), change.after.clone()); }
                same_position.insert((slot.0, slot.1, order), (change.after.clone(), change.key.clone(), change.evidence_refs.clone()));
            }
        } else {
            let fact = &stage.facts[index];
            if matches!(fact.predicate.as_str(), "alias" | "affiliation") { continue; }
            let slot = (fact.subject_key.clone(), fact.predicate.clone());
            if discontinuities.contains(&(slot.0.clone(),slot.1.clone(),order)) {
                let cluster = groups.entry(slot.clone()).or_default();
                cluster.0.push(fact.key.clone()); cluster.1.extend(fact.evidence_refs.clone());
                continue;
            }
            if let Some((previous, key, refs)) = same_position.get(&(slot.0.clone(), slot.1.clone(), order)) {
                if previous != &fact.value {
                    let cluster = groups.entry(slot.clone()).or_default();
                    cluster.0.extend([key.clone(), fact.key.clone()]);
                    cluster.1.extend(refs.clone()); cluster.1.extend(fact.evidence_refs.clone());
                }
            }
            same_position.insert((slot.0.clone(), slot.1.clone(), order), (fact.value.clone(), fact.key.clone(), fact.evidence_refs.clone()));
            values.insert(slot, fact.value.clone());
        }
    }
    groups.into_iter().map(|((subject, predicate), (mut keys, mut refs))| {
        keys.sort(); keys.dedup(); refs.sort(); refs.dedup();
        (format!("{subject}:{predicate}"), keys, refs)
    }).collect()
}

async fn adjudicate_conflicts(
    state: &AppState,
    client: &dyn LlmClient,
    text: &TextVersion,
    config: &RuntimeConfig,
    spans: &[Span],
    prior: &crate::core::kg::ReducedState,
    stage: &mut StageB,
) -> Result<(), String> {
    let clusters = semantic_conflict_clusters(stage, prior);
    for (cluster_key, keys, refs) in clusters {
        let facts: Vec<_> = stage.facts.iter().filter(|fact| keys.contains(&fact.key)).collect();
        let changes: Vec<_> = stage.changes.iter().filter(|change| keys.contains(&change.key)).collect();
        let relevant_values: Vec<_> = prior.values.iter().filter(|((subject, predicate), _)| format!("{subject}:{predicate}") == cluster_key).map(|((subject, predicate), value)| json!({"subject_key":subject,"predicate":predicate,"value":value})).collect();
        let messages = vec![
            ChatMessage { role: "system".into(), content: format!("Adjudicate one semantic conflict cluster from source evidence. Return {}. Keep only directly supported confirmed records; unresolved means no hard update. Do not rewrite before-values to make the draft pass.", ADJUDICATION_VERSION) },
            ChatMessage { role: "user".into(), content: serde_json::to_string(&json!({"cluster_key": cluster_key, "facts": facts, "changes":changes,"prior_hard_values":relevant_values, "evidence": spans.iter().filter(|span| refs.contains(&span.key)).collect::<Vec<_>>() })).expect("adjudication prompt serializes") },
        ];
        let value = structured_cached(state, client, config, text.book_id, TaskKind::Extract, adjudication_schema(), fixtures::KG_ADJUDICATION, messages).await?;
        validate_schema(adjudication_schema(), &value)?;
        let reply: AdjudicationReply = serde_json::from_value(value).map_err(|err| err.to_string())?;
        if reply.schema_version != ADJUDICATION_VERSION || reply.clusters.len() != 1 { return Err("adjudication must decide exactly one requested cluster".into()); }
        let decision = &reply.clusters[0];
        if decision.cluster_key != cluster_key || decision.reason.is_empty() { return Err("adjudication returned a different cluster".into()); }
        valid_refs(&decision.evidence_refs, spans)?;
        if decision.keep_keys.iter().any(|key| !keys.contains(key)) { return Err("adjudication kept a foreign record".into()); }
        let allowed: HashSet<_> = decision.keep_keys.iter().collect();
        for key in &keys {
            if decision.unresolved || decision.decision != "keep" || !allowed.contains(key) {
                stage.unresolved.push(key.clone());
            }
        }
    }
    // Still-discontinuous results remain evidenced but explicitly excluded from hard-state replay.
    let mut eligible = stage.clone();
    eligible.facts.retain(|record| !stage.unresolved.contains(&record.key));
    eligible.changes.retain(|record| !stage.unresolved.contains(&record.key));
    for (_, keys, _) in semantic_conflict_clusters(&eligible, prior) { stage.unresolved.extend(keys); }
    stage.unresolved.sort(); stage.unresolved.dedup();
    Ok(())
}

fn extraction_digest(text: &TextVersion, config: &RuntimeConfig, known: &[Identity], state: &GraphView, spans: &[Span]) -> String {
    digest(serde_json::to_string(&json!({
        "prompt_version": config.prompt_version,
        "model": config.model_extract,
        "stage_a_schema": stage_a_schema(), "identity_schema": identity_schema(), "stage_b_schema": stage_b_schema(), "adjudication_schema": adjudication_schema(),
        "text_hash": text.content_hash, "known": known, "prior_state": state, "spans": spans,
    })).expect("extraction dependency serializes").as_bytes())
}

pub async fn extract(
    state: &AppState,
    client: &dyn LlmClient,
    text: TextVersion,
    config: &RuntimeConfig,
    job_id: Option<i64>,
) -> Result<i64, String> {
    client.check_cancelled().map_err(|error| error.to_string())?;
    let (expected_line_version, known, old, prior, hard_prior) = {
        let db = state.db.lock();
        let conn = db.as_ref().ok_or("database is unavailable")?;
        if crate::store::kg::extraction_matches(conn, text.id, config)? {
            return crate::store::kg::active_revision(conn, text.id)?.ok_or_else(|| "matching extraction lacks an active revision".to_owned());
        }
        let version = crate::store::kg::line_version(conn, text.book_id)?;
        let known = crate::store::kg::identities(conn, text.book_id, &text.source, text.chapter_idx)?;
        let old = crate::store::kg::active_bundle(conn, text.id)?;
        let purpose = if text.source == "original" { "reference" } else { "actual" };
        let prior = crate::store::kg::internal_view(conn, &GraphQuery { book_id: text.book_id, scope: "global".into(), purpose: purpose.into(), chapter_idx: Some(text.chapter_idx.saturating_sub(1)) })?;
        let hard_prior = crate::store::kg::extraction_state(conn, text.book_id, &text.source, text.chapter_idx.saturating_sub(1))?;
        (version, known, old, prior, hard_prior)
    };
    let fixed_overhead = schema_tokens(stage_a_schema()) + estimate_tokens(&stage_a_system(config)) + config.output_reserve.max(1) + SAFETY_RESERVE;
    let body_tokens = config.context_limit.checked_sub(fixed_overhead).ok_or("stage A schema/system/output reserve exhaust provider context")?;
    let span_capacity = (body_tokens / 8).clamp(4, 512);
    let evidence_spans = prepare_spans(&text, span_capacity)?;
    let blocks = budget_blocks(&evidence_spans, config, stage_a_schema(), |block| stage_a_prompt(block, &known, config, text.chapter_idx))?;
    let mut raw_a = Vec::new();
    for block in &blocks {
        let messages = stage_a_prompt(block, &known, config, text.chapter_idx);
        let value = structured_cached(state, client, config, text.book_id, TaskKind::Extract, stage_a_schema(), fixtures::KG_STAGE_A, messages).await?;
        raw_a.push(decode_stage_a(value, &text, &block.spans)?);
    }
    let mut stage_a = merge_stage_a(&text, raw_a)?;
    let mut resolution = resolve_local_identities(&text, &stage_a, &known, &old.as_ref().map(|bundle| bundle.identities.clone()).unwrap_or_default());
    resolve_ambiguous_identities(state, client, &text, config, &stage_a, &known, &evidence_spans, &mut resolution).await?;
    stage_a = resolve_stage_a(stage_a, &resolution)?;
    rekey_stage_a(&text, &mut stage_a, old.as_ref())?;

    let all_identities: Vec<_> = known.iter().cloned().chain(resolution.identities.iter().cloned()).collect();
    let b_blocks = budget_blocks(&evidence_spans, config, stage_b_schema(), |block| stage_b_prompt(&text, block, &stage_a, &all_identities, &prior, &hard_prior, config))?;
    let mut stages_b = Vec::new();
    for block in &b_blocks {
        let messages = stage_b_prompt(&text, block, &stage_a, &all_identities, &prior, &hard_prior, config);
        let value = structured_cached(state, client, config, text.book_id, TaskKind::Extract, stage_b_schema(), fixtures::KG_STAGE_B, messages).await?;
        let mut stage = decode_stage_b(value, &text, &block.spans)?;
        rekey_stage_b(&text, &mut stage, &hard_prior)?;
        stages_b.push(stage);
    }
    let mut stage_b = merge_stage_b(&text, stages_b)?;
    stage_b.unresolved.extend(resolution.unresolved);
    let draft = ChapterBundle { stage_a:stage_a.clone(), stage_b:stage_b.clone(), identities:resolution.identities.clone(), spans:evidence_spans.clone(), identity_links:resolution.links.clone() };
    let prior_relations: Vec<_> = hard_prior.relations.values().cloned().collect();
    validate_bundle(&text, &draft, &known, &prior_relations)?;
    let current_identity_map = crate::core::kg::revealed_identity_map(&[draft.clone()], text.chapter_idx);
    let canonical_prior = crate::core::kg::canonicalize_state(&hard_prior, &current_identity_map);
    adjudicate_conflicts(state, client, &text, config, &evidence_spans, &canonical_prior, &mut stage_b).await?;
    let bundle = ChapterBundle { stage_a, stage_b, identities: resolution.identities, spans: evidence_spans, identity_links: resolution.links };
    validate_bundle(&text, &bundle, &known, &prior_relations)?;
    let dependencies = extraction_digest(&text, config, &known, &prior, &bundle.spans);
    let revision = {
        let _commit = client.commit_guard().map_err(|error| error.to_string())?;
        let db = state.db.lock();
        let conn = db.as_ref().ok_or("database is unavailable")?;
        crate::store::kg::commit_bundle_with_config(conn, &text, &bundle, &dependencies, expected_line_version, job_id, config)?
    };
    Ok(revision)
}

fn reduction_messages(config: &RuntimeConfig, summaries: &[(i64, String)]) -> Vec<ChatMessage> {
    vec![
        ChatMessage { role: "system".into(), content: format!("Produce an original-reference synopsis, themes, and a spoiler-free style guide. Prompt version: {}. Do not turn reference future plot into current fact.", config.prompt_version) },
        ChatMessage { role: "user".into(), content: serde_json::to_string(&json!({"chapter_summaries": summaries})).expect("reduction prompt serializes") },
    ]
}

fn reduction_groups(config: &RuntimeConfig, summaries: Vec<(i64, String)>) -> Result<Vec<Vec<(i64, String)>>, String> {
    let overhead = schema_tokens(&REDUCE_SCHEMA) + config.output_reserve.max(1) + SAFETY_RESERVE + 256;
    let capacity = config.context_limit.checked_sub(overhead).ok_or("reduction schema/output reserve exhaust provider context")?;
    let mut groups = Vec::new();
    let mut current = Vec::new();
    let mut used = 0;
    for summary in summaries {
        let cost = estimate_tokens(&summary.1);
        if cost > capacity { return Err(format!("chapter {} summary exceeds reduction context budget", summary.0)); }
        if !current.is_empty() && used + cost > capacity { groups.push(std::mem::take(&mut current)); used = 0; }
        used += cost;
        current.push(summary);
    }
    if !current.is_empty() { groups.push(current); }
    Ok(groups)
}

/// Bounded hierarchical reduction of already extracted original chapter summaries. It does not
/// inspect generated text and cannot expose unrevealed reference facts as current-world state.
pub async fn reduce_book(
    state: &AppState,
    client: &dyn LlmClient,
    config: &RuntimeConfig,
    book_id: i64,
) -> Result<(), String> {
    let mut summaries = {
        let db = state.db.lock();
        let conn = db.as_ref().ok_or("database is unavailable")?;
        crate::store::kg::original_summaries(conn, book_id)?
    };
    if summaries.is_empty() { return Err("cannot reduce a book without committed original chapter summaries".into()); }
    while summaries.len() > 1 {
        let previous_count = summaries.len();
        let groups = reduction_groups(config, summaries)?;
        let mut next = Vec::with_capacity(groups.len());
        for group in groups {
            let messages = reduction_messages(config, &group);
            let value = structured_cached(state, client, config, book_id, TaskKind::Extract, &REDUCE_SCHEMA, "kg_reduce", messages).await?;
            validate_schema(&REDUCE_SCHEMA, &value)?;
            let reduced: ReductionReply = serde_json::from_value(value).map_err(|err| err.to_string())?;
            let first = group.first().map(|item| item.0).unwrap_or_default();
            next.push((first, reduced.synopsis));
        }
        if next.len() >= previous_count { return Err("reduction did not shrink; provider context budget is too small".into()); }
        summaries = next;
    }
    let messages = reduction_messages(config, &summaries);
    let value = structured_cached(state, client, config, book_id, TaskKind::Extract, &REDUCE_SCHEMA, "kg_reduce", messages).await?;
    validate_schema(&REDUCE_SCHEMA, &value)?;
    let reduced: ReductionReply = serde_json::from_value(value).map_err(|err| err.to_string())?;
    let _commit = client.commit_guard().map_err(|error| error.to_string())?;
    let db = state.db.lock();
    let conn = db.as_ref().ok_or("database is unavailable")?;
    crate::store::kg::save_original_summary(conn, book_id, &reduced.synopsis, &reduced.themes, &reduced.style_guide)
}
