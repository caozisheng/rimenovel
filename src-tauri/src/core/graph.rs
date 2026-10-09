// 图谱领域类型（dev-plan Task 4.1）
// serde 直接映射 docs/schemas/{chapter,global}-graph.schema.json；
// schema 原文编译期内嵌供校验复用。LLM 输出 → ChapterGraph/GlobalGraph 强类型。

use serde::{Deserialize, Serialize};

pub const CHAPTER_GRAPH_SCHEMA: &str = include_str!("../../schemas/chapter-graph.schema.json");
pub const GLOBAL_GRAPH_SCHEMA: &str = include_str!("../../schemas/global-graph.schema.json");

pub fn chapter_schema_json() -> serde_json::Value {
    serde_json::from_str(CHAPTER_GRAPH_SCHEMA).expect("chapter schema 内嵌合法")
}

pub fn global_schema_json() -> serde_json::Value {
    serde_json::from_str(GLOBAL_GRAPH_SCHEMA).expect("global schema 内嵌合法")
}

// ── 共用受控词表（两 schema 一致）──────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Character,
    Location,
    Item,
    Faction,
    Concept,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelCategory {
    Family,
    Romantic,
    Friendship,
    Enmity,
    Alliance,
    MasterStudent,
    Subordination,
    Rivalry,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    Conflict,
    Battle,
    Meeting,
    Parting,
    Journey,
    Discovery,
    Betrayal,
    Deal,
    Loss,
    Death,
    Romance,
    Mystery,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationStatus {
    Active,
    Broken,
    Former,
    Unknown,
}

// ── 章节图谱（chapter/window 双模式信封）──────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterGraph {
    pub schema_version: String,
    pub mode: ChapterMode,
    pub chapter_idx: Option<i64>,
    pub chapter_summary: Option<String>,
    pub chapter_summaries: Option<Vec<ChapterSummary>>,
    pub open_threads: Option<Vec<String>>,
    pub entities: Vec<Entity>,
    pub relations: Vec<Relation>,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChapterMode {
    Chapter,
    Window,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterSummary {
    pub chapter_idx: i64,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub key: String,
    #[serde(rename = "type")]
    pub entity_type: EntityType,
    pub name: String,
    pub aliases: Vec<String>,
    pub summary: Option<String>,
    pub traits: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub src: String,
    pub dst: String,
    pub rel_category: RelCategory,
    pub rel_type: String,
    pub description: Option<String>,
    pub status: RelationStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub key: String,
    pub title: String,
    pub category: EventCategory,
    pub chapter_idx: Option<i64>,
    pub order: i64,
    pub participants: Vec<String>,
    pub location: Option<String>,
    pub summary: String,
    pub outcome: String,
    pub cause: Option<String>,
}

// ── 全局图谱（聚合形态）───────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalGraph {
    pub schema_version: String,
    pub synopsis: String,
    #[serde(default)]
    pub style_guide: Option<StyleGuide>,
    pub themes: Option<Vec<String>>,
    #[serde(default)]
    pub entities: Vec<GlobalEntity>,
    #[serde(default)]
    pub relations: Vec<GlobalRelation>,
    #[serde(default)]
    pub events: Vec<GlobalEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StyleGuide {
    pub narrative_person: String,
    pub tone: String,
    pub pacing: String,
    pub signature: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalEntity {
    pub key: String,
    #[serde(rename = "type")]
    pub entity_type: EntityType,
    pub name: String,
    pub aliases: Vec<String>,
    pub summary: String,
    pub traits: Option<Vec<String>>,
    pub first_seen_chapter: i64,
    pub last_seen_chapter: i64,
    pub importance: Importance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Importance {
    Major,
    Supporting,
    Minor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalRelation {
    pub src: String,
    pub dst: String,
    pub rel_category: RelCategory,
    pub rel_type: String,
    pub description: Option<String>,
    pub status: RelationStatus,
    pub since_chapter: i64,
    pub until_chapter: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalEvent {
    pub key: String,
    pub title: String,
    pub category: EventCategory,
    pub chapter_idx: i64,
    pub order: i64,
    pub participants: Vec<String>,
    pub location: Option<String>,
    pub summary: String,
    pub outcome: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapter_schema_roundtrip() {
        let doc = serde_json::json!({
            "schema_version": "1",
            "mode": "chapter",
            "chapter_idx": 3,
            "chapter_summary": "林动在矿洞与堂兄冲突并结怨。",
            "chapter_summaries": null,
            "open_threads": ["矿洞背后的势力未明"],
            "entities": [
                {"key": "lin-dong", "type": "character", "name": "林动", "aliases": ["林家小子"], "summary": null, "traits": ["坚韧"]},
                {"key": "ironwood-vein", "type": "location", "name": "铁木矿脉", "aliases": [], "summary": "林家矿脉", "traits": null}
            ],
            "relations": [
                {"src": "lin-dong", "dst": "lin-xiao", "rel_category": "enmity", "rel_type": "堂兄弟反目", "description": "因矿洞分配结怨", "status": "active"}
            ],
            "events": [
                {"key": "ev-3-1", "title": "矿洞冲突", "category": "conflict", "chapter_idx": null, "order": 1,
                 "participants": ["lin-dong", "lin-xiao"], "location": "ironwood-vein",
                 "summary": "争执升级为动手。", "outcome": "结怨", "cause": null}
            ]
        });
        let g: ChapterGraph = serde_json::from_value(doc.clone()).unwrap();
        assert_eq!(g.mode, ChapterMode::Chapter);
        assert_eq!(g.entities.len(), 2);
        assert_eq!(g.entities[0].entity_type, EntityType::Character);
        assert_eq!(g.relations[0].rel_category, RelCategory::Enmity);
        assert_eq!(g.events[0].category, EventCategory::Conflict);
        let back = serde_json::to_value(&g).unwrap();
        assert_eq!(doc, back, "序列化往返无损");
    }

    #[test]
    fn window_mode_parses() {
        let doc = serde_json::json!({
            "schema_version": "1",
            "mode": "window",
            "chapter_idx": null,
            "chapter_summary": null,
            "chapter_summaries": [
                {"chapter_idx": 1, "summary": "a"},
                {"chapter_idx": 2, "summary": "b"}
            ],
            "open_threads": null,
            "entities": [],
            "relations": [],
            "events": []
        });
        let g: ChapterGraph = serde_json::from_value(doc).unwrap();
        assert_eq!(g.mode, ChapterMode::Window);
        assert_eq!(g.chapter_summaries.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn global_graph_defaults_empty() {
        let g: GlobalGraph = serde_json::from_value(serde_json::json!({
            "schema_version": "1",
            "synopsis": "梗概",
            "style_guide": {"narrative_person": "third_limited", "tone": "热血", "pacing": "快", "signature": []},
            "themes": null,
            "entities": [], "relations": [], "events": []
        }))
        .unwrap();
        assert!(g.entities.is_empty());
        assert!(g.style_guide.is_some());
    }

    #[test]
    fn embedded_schemas_are_valid_json() {
        let cs: serde_json::Value = chapter_schema_json();
        assert_eq!(
            cs["title"],
            "RimeNovel 章节知识图谱（抽取信封，chapter/window 双模式）"
        );
        let gs: serde_json::Value = global_schema_json();
        assert!(gs["properties"]["style_guide"].is_object());
    }
}
