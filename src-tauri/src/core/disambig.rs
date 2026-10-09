// synopsis/style_guide reduce + 消歧 pass（dev-plan Task 4.4）
// reduce: 窗口 chapter_summaries → 一次 extract 调用产出 {synopsis, style_guide, themes}
// disambig: MergeReport.conflicts → 每簇一次裁决调用(merge/keep/merge-into) → key 归并映射

use crate::core::graph::merge::{Conflict, ConflictKind};
use crate::core::graph::GlobalGraph;
use crate::llm::{ChatMessage, StructuredRequest, TaskKind};
use serde_json::{json, Value};

/// reduce 调用的输出 schema（内嵌小 schema）
pub fn reduce_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["synopsis", "narrative_person", "tone", "pacing", "signature", "themes"],
        "properties": {
            "synopsis": {"type": "string"},
            "narrative_person": {"type": "string", "enum": ["first", "third_limited", "third_omniscient", "mixed"]},
            "tone": {"type": "string"},
            "pacing": {"type": "string"},
            "signature": {"type": "array", "items": {"type": "string"}},
            "themes": {"type": "array", "items": {"type": "string"}}
        }
    })
}

/// reduce 的 user prompt：窗口摘要列表 → 全书梗概 + 文风
pub fn reduce_prompt(summaries: &[(i64, String)]) -> String {
    let body = summaries
        .iter()
        .map(|(i, s)| format!("第{i}章: {s}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "以下是全书逐章摘要。请综合产出:\n\
         1. synopsis: 一段式全书梗概(200字内);\n\
         2. narrative_person/tone/pacing/signature: 文风指南(人称/基调/节奏/标志性写法);\n\
         3. themes: 主题标签。\n\n## 逐章摘要\n{body}"
    )
}

/// 消歧裁决 schema
fn disambig_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "canonical_key"],
        "properties": {
            "verdict": {"type": "string", "enum": ["merge", "keep"]},
            "canonical_key": {"type": "string"}
        }
    })
}

/// 消歧 user prompt：同 name 的两个 key，提供各自证据（出现章节/关系/事件），裁决是否同一实体
pub fn disambig_prompt(global: &GlobalGraph, conflict: &Conflict) -> String {
    let mut cards = Vec::new();
    for key in &conflict.keys {
        let entity = global.entities.iter().find(|e| &e.key == key);
        let rels = global
            .relations
            .iter()
            .filter(|r| &r.src == key || &r.dst == key)
            .map(|r| format!("{}-{}({:?})", r.src, r.dst, r.rel_category))
            .collect::<Vec<_>>();
        let evs = global
            .events
            .iter()
            .filter(|e| e.participants.iter().any(|p| p == key))
            .map(|e| format!("第{}章 {}", e.chapter_idx, e.title))
            .collect::<Vec<_>>();
        cards.push(format!(
            "### key={}\nname={:?}\n关系: {}\n事件: {}",
            key,
            entity.map(|e| e.name.clone()).unwrap_or_default(),
            if rels.is_empty() {
                "无".into()
            } else {
                rels.join("; ")
            },
            if evs.is_empty() {
                "无".into()
            } else {
                evs.join("; ")
            },
        ));
    }
    format!(
        "小说知识图谱中, 以下 {} 个实体 key 共享同一个称呼 {:?}。\n\
         判断它们是否为同一实体: 同一 → verdict=merge 且 canonical_key 取更常被引用的 key; 不同 → verdict=keep。\n\n{}",
        conflict.keys.len(),
        conflict.names.first(),
        cards.join("\n\n")
    )
}

/// 应用消歧结果: key 归并映射 (from_key → canonical_key), 全图重写
pub fn apply_merge_map(
    global: &mut GlobalGraph,
    merge_map: &std::collections::HashMap<String, String>,
) {
    // 实体: 归并到 canonical (别名并集), 删除被归并行
    let mut removed_keys = std::collections::HashSet::new();
    let mut to_merge: Vec<(String, String)> = Vec::new(); // (from, canonical)
    for (from, to) in merge_map {
        if from != to {
            to_merge.push((from.clone(), to.clone()));
            removed_keys.insert(from.clone());
        }
    }
    for (from, to) in &to_merge {
        if let Some(src) = global.entities.iter().find(|e| &e.key == from).cloned() {
            if let Some(dst) = global.entities.iter_mut().find(|e| &e.key == to) {
                for a in &src.aliases {
                    if !dst.aliases.contains(a) && *a != dst.name {
                        dst.aliases.push(a.clone());
                    }
                }
                if !dst.aliases.contains(&src.name) && src.name != dst.name {
                    dst.aliases.push(src.name.clone());
                }
                dst.first_seen_chapter = dst.first_seen_chapter.min(src.first_seen_chapter);
                dst.last_seen_chapter = dst.last_seen_chapter.max(src.last_seen_chapter);
            }
        }
    }
    global.entities.retain(|e| !removed_keys.contains(&e.key));

    let remap = |k: &str| -> String { merge_map.get(k).cloned().unwrap_or_else(|| k.to_string()) };
    for r in &mut global.relations {
        r.src = remap(&r.src);
        r.dst = remap(&r.dst);
    }
    for ev in &mut global.events {
        for p in &mut ev.participants {
            *p = remap(p);
        }
        if let Some(loc) = &mut ev.location {
            *loc = remap(loc);
        }
    }
}

/// 冲突簇 → LLM 裁决（每簇一次调用）；返回归并映射。LLM 失败的簇保守 keep。
pub async fn disambiguate(
    client: &dyn crate::llm::LlmClient,
    global: &GlobalGraph,
    conflicts: &[Conflict],
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let schema = disambig_schema();
    for c in conflicts {
        if c.kind != ConflictKind::SameNameDiffKey {
            continue;
        }
        let req = StructuredRequest {
            task: TaskKind::Merge,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: disambig_prompt(global, c),
            }],
            schema: &schema,
            schema_name: "disambig_verdict",
            temperature: 0.0,
        };
        let Ok(v) = client.structured(req).await else {
            continue; // 失败保守 keep
        };
        let verdict = v["verdict"].as_str().unwrap_or("keep");
        let canonical = v["canonical_key"].as_str().unwrap_or_default();
        if verdict == "merge" && c.keys.iter().any(|k| k == canonical) {
            for k in &c.keys {
                if k != canonical {
                    map.insert(k.clone(), canonical.to_string());
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::*;

    fn entity(key: &str, name: &str) -> GlobalEntity {
        GlobalEntity {
            key: key.into(),
            entity_type: EntityType::Character,
            name: name.into(),
            aliases: vec![],
            summary: String::new(),
            traits: None,
            first_seen_chapter: 1,
            last_seen_chapter: 2,
            importance: Importance::Minor,
        }
    }

    #[test]
    fn reduce_prompt_lists_all_chapters() {
        let p = reduce_prompt(&[(1, "a".into()), (2, "b".into())]);
        assert!(p.contains("第1章: a") && p.contains("第2章: b"));
        assert!(p.contains("逐章摘要"));
    }

    #[test]
    fn disambig_prompt_contains_both_cards() {
        let mut g = GlobalGraph::default();
        g.entities.push(entity("lin-dong", "林动"));
        g.entities.push(entity("lin-dong-2", "林动"));
        g.events.push(GlobalEvent {
            key: "ev-1-1".into(),
            title: "冲突".into(),
            category: EventCategory::Conflict,
            chapter_idx: 1,
            order: 1,
            participants: vec!["lin-dong".into()],
            location: None,
            summary: "s".into(),
            outcome: "o".into(),
        });
        let c = Conflict {
            kind: ConflictKind::SameNameDiffKey,
            keys: vec!["lin-dong".into(), "lin-dong-2".into()],
            names: vec!["林动".into()],
        };
        let p = disambig_prompt(&g, &c);
        assert!(p.contains("key=lin-dong"));
        assert!(p.contains("key=lin-dong-2"));
        assert!(p.contains("第1章 冲突"));
    }

    #[test]
    fn apply_merge_map_unifies_everything() {
        let mut g = GlobalGraph::default();
        g.entities.push(entity("lin-dong", "林动"));
        let mut e2 = entity("lin-dong-2", "林动");
        e2.name = "林动".into();
        g.entities.push(e2);
        g.relations.push(GlobalRelation {
            src: "lin-dong-2".into(),
            dst: "x".into(),
            rel_category: RelCategory::Enmity,
            rel_type: "敌".into(),
            description: None,
            status: RelationStatus::Active,
            since_chapter: 1,
            until_chapter: None,
        });
        g.events.push(GlobalEvent {
            key: "ev-1-1".into(),
            title: "t".into(),
            category: EventCategory::Conflict,
            chapter_idx: 1,
            order: 1,
            participants: vec!["lin-dong-2".into()],
            location: Some("loc".into()),
            summary: "s".into(),
            outcome: "o".into(),
        });
        let mut map = std::collections::HashMap::new();
        map.insert("lin-dong-2".to_string(), "lin-dong".to_string());
        apply_merge_map(&mut g, &map);
        assert_eq!(g.entities.len(), 1);
        assert_eq!(g.entities[0].key, "lin-dong");
        assert_eq!(g.relations[0].src, "lin-dong");
        assert_eq!(g.events[0].participants, vec!["lin-dong".to_string()]);
    }

    #[test]
    fn apply_merge_map_noop_when_identity() {
        let mut g = GlobalGraph::default();
        g.entities.push(entity("a", "甲"));
        let map = std::collections::HashMap::from([("a".to_string(), "a".to_string())]);
        apply_merge_map(&mut g, &map);
        assert_eq!(g.entities.len(), 1);
    }
}
