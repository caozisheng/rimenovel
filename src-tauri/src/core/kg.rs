//! Lightweight writing graphs. Runtime ownership and coverage never come from model output.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub const ENTITY_TYPES: &[&str] = &["character", "location", "item", "faction", "concept"];
pub const GRAPH_FORMAT: &str = "kg-lite/v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TextVersion { pub id:i64, pub book_id:i64, pub chapter_idx:i64, pub source:String, pub content:String, pub content_hash:String, pub generated_id:Option<i64> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig { pub provider_id:i64, pub model_extract:String, pub context_limit:usize, pub output_reserve:usize, pub prompt_version:String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphQuery { pub book_id:i64, pub scope:String, pub purpose:String, pub chapter_idx:Option<i64> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    pub key:String, #[serde(rename="type")] pub entity_type:String, pub name:String,
    pub aliases:Vec<String>, pub summary:Option<String>, pub life_status:Option<String>,
    pub location:Option<String>, pub owner:Option<String>, pub chapter_idx:i64, pub provenance:String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphEdge { pub key:String, pub src:String, pub dst:String, pub rel_type:String, pub description:Option<String>, pub chapter_idx:i64, pub provenance:String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphEvent { pub key:String, pub title:String, pub chapter_idx:i64, pub order:i64, pub participants:Vec<String>, pub location:Option<String>, pub time:Option<String>, pub summary:String, pub provenance:String }
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChapterSnapshot { pub summary:String, pub nodes:Vec<GraphNode>, pub edges:Vec<GraphEdge>, pub events:Vec<GraphEvent>, pub open_threads:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphCoverage {
    pub mode:String, pub total_chapters:usize, pub selected_chapters:Vec<i64>, pub completed_chapters:Vec<i64>,
    pub missing_chapters:Vec<i64>, pub legacy_chapters:Vec<i64>, pub calls_used:usize, pub elapsed_ms:u64,
    pub target_complete:bool,
}
impl Default for GraphCoverage {
    fn default()->Self { Self { mode:"partial".into(),total_chapters:0,selected_chapters:vec![],completed_chapters:vec![],missing_chapters:vec![],legacy_chapters:vec![],calls_used:0,elapsed_ms:0,target_complete:false } }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphView {
    pub nodes:Vec<GraphNode>, pub edges:Vec<GraphEdge>, pub events:Vec<GraphEvent>, pub summary:String,
    pub open_threads:Vec<String>, pub coverage:GraphCoverage, pub purpose:String, pub reader_progress:i64,
    pub chapter_idx:Option<i64>, pub fog:Option<FogCounts>, pub revealed:bool, pub ready:bool,
    pub revision:Option<i64>, pub drift_score:Option<f64>, pub reading_line_version:i64, pub divergence_idx:Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FogCounts { pub entities:usize, pub relations:usize, pub events:usize }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EditInput { pub book_id:i64, pub scope:String, pub chapter_idx:Option<i64>, pub kind:String, pub action:String, pub key:Option<String>, pub data:Value }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EditRecord { pub id:i64, pub target_type:String, pub target_id:String, pub action:String, pub after_json:Option<String>, pub effective_from:i64, pub active:bool }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GenerationContext { pub book_id:i64, pub chapter_idx:i64, pub reading_line_version:i64, pub digest:String, pub actual:GraphView, pub spec:GraphView, pub previous_text:String, pub reference_text:Option<String>, pub style_guide:Option<String>, pub directives:Vec<String>, pub drift_pref:f64 }

pub fn empty_view(purpose:&str)->GraphView { GraphView {purpose:purpose.into(),..GraphView::default()} }

/// Snapshot references are self-contained; known nodes used by the model are imported before validation.
pub fn validate_snapshot(snapshot:&ChapterSnapshot)->Result<(),String> {
    let mut nodes=HashMap::with_capacity(snapshot.nodes.len());
    for node in &snapshot.nodes {
        if node.key.is_empty() || node.name.trim().is_empty() || node.chapter_idx<1 || !ENTITY_TYPES.contains(&node.entity_type.as_str()) || nodes.insert(node.key.as_str(),node).is_some() {
            return Err("invalid or duplicate graph entity".into());
        }
        if node.life_status.as_deref().is_some_and(|s| !["alive","dead"].contains(&s)) || (node.entity_type!="character" && node.life_status.is_some()) || (node.entity_type!="item" && node.owner.is_some()) {
            return Err("invalid entity life or owner state".into());
        }
    }
    let location=|key:&str| nodes.get(key).is_some_and(|node|node.entity_type=="location");
    for node in &snapshot.nodes {
        if node.location.as_deref().is_some_and(|key|!location(key)) {return Err("invalid entity location reference".into());}
        if node.owner.as_deref().is_some_and(|key|!nodes.get(key).is_some_and(|owner|["character","faction"].contains(&owner.entity_type.as_str()))) {return Err("invalid item owner reference".into());}
    }
    let mut keys=HashSet::with_capacity(snapshot.edges.len());
    for edge in &snapshot.edges {
        if edge.key.is_empty() || !keys.insert(edge.key.as_str()) || edge.rel_type.trim().is_empty() || edge.chapter_idx<1 || !nodes.contains_key(edge.src.as_str()) || !nodes.contains_key(edge.dst.as_str()) {return Err("invalid graph relation reference".into());}
    }
    keys.clear();
    for event in &snapshot.events {
        if event.key.is_empty() || !keys.insert(event.key.as_str()) || event.title.trim().is_empty() || event.chapter_idx<1 || event.order<0 || event.participants.iter().any(|key|!nodes.contains_key(key.as_str())) || event.location.as_deref().is_some_and(|key|!location(key)) {return Err("invalid graph event reference".into());}
    }
    Ok(())
}

pub fn digest(bytes:&[u8])->String {
    use sha2::{Digest,Sha256}; use std::fmt::Write;
    let mut encoded=String::with_capacity(64);
    for byte in Sha256::digest(bytes) {write!(&mut encoded,"{byte:02x}").expect("writing digest to String");}
    encoded
}
pub fn public_key(book_id:i64,kind:&str,internal:&str)->String {format!("{}-{}",kind,&digest(format!("{book_id}:{kind}:{internal}").as_bytes())[..24])}
