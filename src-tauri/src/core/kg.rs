use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TextVersion { pub id:i64, pub book_id:i64, pub chapter_idx:i64, pub source:String, pub content:String, pub content_hash:String, pub generated_id:Option<i64> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Span { pub key:String, pub chapter_idx:i64, pub start_byte:usize, pub end_byte:usize, pub text:String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Identity { pub key:String, pub entity_type:String, pub name:String, pub aliases:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Mention { pub mention_id:String, pub name:String, pub entity_type:String, pub candidate_keys:Vec<String>, pub evidence_refs:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExtractEvent { pub key:String, pub title:String, pub category:String, pub chapter_idx:i64, pub observed_chapter:i64, pub observed_order:i64, pub temporal_scope:String, pub assertion_kind:String, pub participants:Vec<String>, pub location:Option<String>, pub summary:String, pub outcome:String, pub cause_keys:Vec<String>, pub evidence_refs:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StageA { pub schema_version:String, pub chapter_idx:i64, pub summary:String, pub open_threads:Vec<String>, pub mentions:Vec<Mention>, pub events:Vec<ExtractEvent> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TypedValue { pub kind:String, pub value:Option<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Fact { pub key:String, pub subject_key:String, pub predicate:String, pub value:TypedValue, pub chapter_idx:i64, pub observed_chapter:i64, pub observed_order:i64, pub temporal_scope:String, pub assertion_kind:String, pub speaker_key:Option<String>, pub evidence_refs:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RelationObservation { pub key:String, pub src:String, pub dst:String, pub rel_category:String, pub rel_type:String, pub status:String, pub description:Option<String>, pub chapter_idx:i64, pub observed_chapter:i64, pub observed_order:i64, pub temporal_scope:String, pub assertion_kind:String, pub evidence_refs:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Change { pub key:String, pub event_key:String, pub subject_key:String, pub predicate:String, pub operation:String, pub before:TypedValue, pub after:TypedValue, pub chapter_idx:i64, pub observed_chapter:i64, pub observed_order:i64, pub temporal_scope:String, pub assertion_kind:String, pub evidence_refs:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StageB { pub schema_version:String, pub chapter_idx:i64, pub facts:Vec<Fact>, pub relations:Vec<RelationObservation>, pub changes:Vec<Change>, pub preconditions:Vec<Precondition>, pub unresolved:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChapterBundle { pub stage_a:StageA, pub stage_b:StageB, pub identities:Vec<Identity>, pub spans:Vec<Span>, pub identity_links:Vec<IdentityLink> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IdentityLink { pub from_key:String, pub to_key:String, pub observed_chapter:i64, pub observed_order:i64, pub evidence_refs:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Precondition { pub key:String, pub event_key:String, pub subject_key:String, pub predicate:String, pub value:TypedValue, pub assertion_kind:String, pub evidence_refs:Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RuntimeConfig { pub provider_id:i64, pub model_extract:String, pub context_limit:usize, pub output_reserve:usize, pub prompt_version:String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphQuery { pub book_id:i64, pub scope:String, pub purpose:String, pub chapter_idx:Option<i64> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphNode { pub key:String, #[serde(rename="type")] pub entity_type:String, pub name:String, pub aliases:Vec<String>, pub summary:Option<String>, pub chapter_idx:i64, pub provenance:String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphEdge { pub key:String, pub src:String, pub dst:String, pub rel_type:String, pub description:Option<String>, pub status:String, pub since_chapter:i64, pub chapter_idx:i64, pub provenance:String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphEvent { pub key:String, pub title:String, pub chapter_idx:i64, pub order:i64, pub participants:Vec<String>, pub location:Option<String>, pub summary:String, pub outcome:String, pub provenance:String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphView { pub nodes:Vec<GraphNode>, pub edges:Vec<GraphEdge>, pub events:Vec<GraphEvent>, pub purpose:String, pub reader_progress:i64, pub chapter_idx:Option<i64>, pub fog:Option<FogCounts>, pub revealed:bool, pub ready:bool, pub unresolved:Vec<String>, pub revision:Option<i64>, pub drift_score:Option<f64>, pub reading_line_version:i64, pub divergence_idx:Option<i64> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FogCounts { pub entities:usize, pub relations:usize, pub events:usize }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EditInput { pub book_id:i64, pub scope:String, pub chapter_idx:Option<i64>, pub kind:String, pub action:String, pub key:Option<String>, pub data:Value }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EditRecord { pub id:i64, pub target_type:String, pub target_id:String, pub action:String, pub after_json:Option<String>, pub effective_from:i64, pub active:bool }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EvidenceView { pub key:String, pub chapter_idx:i64, pub start_byte:usize, pub end_byte:usize, pub text:String, pub source:String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GenerationContext { pub book_id:i64, pub chapter_idx:i64, pub reading_line_version:i64, pub digest:String, pub actual:GraphView, pub spec:GraphView, pub previous_text:String, pub reference_text:Option<String>, pub style_guide:Option<String>, pub directives:Vec<String>, pub drift_pref:f64 }

mod validate;
mod reduce;
pub use validate::*;
pub use reduce::*;

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut encoded=String::with_capacity(64);
    for byte in Sha256::digest(bytes){write!(&mut encoded,"{byte:02x}").expect("writing digest to String");}
    encoded
}
pub fn public_key(book_id:i64, kind:&str, internal:&str) -> String {
    format!("{}-{}",kind,&digest(format!("{book_id}:{kind}:{internal}").as_bytes())[..24])
}
