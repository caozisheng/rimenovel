// 窗口合并纯函数（dev-plan Task 4.2，TDD 核心）
// merge_window(acc, window) → (acc', report)：无 LLM、全 HashMap 索引、幂等。
// 不变式矩阵见 tests（8 项，对应设计 §4.2.1）。

use crate::core::graph::{
    ChapterGraph, EntityType, GlobalEntity, GlobalEvent, GlobalGraph, GlobalRelation, Importance,
    RelationStatus,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default, Clone)]
pub struct MergeReport {
    /// 成功合并的实体/关系/事件数
    pub entities_merged: usize,
    pub relations_merged: usize,
    pub events_merged: usize,
    /// 引用缺失被丢弃的元素（key 及原因）
    pub dropped: Vec<String>,
    /// 消歧 pass 候选簇（同 key 不同 name / 同 name 不同 key / 别名交叉）
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub kind: ConflictKind,
    pub keys: Vec<String>,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// 同 name 不同 key
    SameNameDiffKey,
    /// 别名指向多个 key
    AliasCross,
}

/// 窗口章节范围（事件/实体出现章节推导用）
fn window_range(w: &ChapterGraph) -> (i64, i64) {
    let mut idxs: Vec<i64> = Vec::new();
    if let Some(css) = &w.chapter_summaries {
        idxs.extend(css.iter().map(|c| c.chapter_idx));
    }
    for ev in &w.events {
        if let Some(ci) = ev.chapter_idx {
            idxs.push(ci);
        }
    }
    if idxs.is_empty() {
        (0, 0)
    } else {
        (*idxs.iter().min().unwrap(), *idxs.iter().max().unwrap())
    }
}

/// 合并一个窗口到全局累积图（就地 mutate acc，返回报告）。
pub fn merge_window(acc: &mut GlobalGraph, w: &ChapterGraph) -> MergeReport {
    let mut report = MergeReport::default();
    let (lo, hi) = window_range(w);

    // name/alias → key 索引（既有 + 本窗口），用于引用解析与冲突检测
    let mut alias_index: HashMap<String, String> = HashMap::new();
    for e in &acc.entities {
        alias_index.insert(e.name.clone(), e.key.clone());
        for a in &e.aliases {
            alias_index.insert(a.clone(), e.key.clone());
        }
    }

    // ── entities ─────────────────────────────────────────────
    let mut window_keys: HashSet<String> = HashSet::new();
    for e in &w.entities {
        // 引用解析：name/alias 命中既有 key 且窗口 key 与之一致 → 归并；
        // 命中但 key 不同 → 冲突(保留窗口 key, 交消歧裁决, 不静默归并)
        let known = alias_index.get(&e.name).cloned();
        let resolved_key = match &known {
            Some(k) if *k == e.key => k.clone(),
            Some(_) => e.key.clone(),
            None => e.key.clone(),
        };
        if let Some(k) = &known {
            if *k != resolved_key {
                report.conflicts.push(Conflict {
                    kind: ConflictKind::SameNameDiffKey,
                    keys: vec![k.clone(), resolved_key.clone()],
                    names: vec![e.name.clone()],
                });
            }
        }
        window_keys.insert(resolved_key.clone());

        match acc.entities.iter_mut().find(|x| x.key == resolved_key) {
            Some(existing) => {
                // 不变式: alias 并集; summary 取最新非空; 出现章节扩张
                for a in &e.aliases {
                    if !existing.aliases.contains(a) {
                        existing.aliases.push(a.clone());
                    }
                }
                if existing.summary.is_empty() {
                    if let Some(s) = &e.summary {
                        existing.summary = s.clone();
                    }
                }
                existing.first_seen_chapter = existing.first_seen_chapter.min(lo);
                existing.last_seen_chapter = existing.last_seen_chapter.max(hi);
            }
            None => {
                acc.entities.push(GlobalEntity {
                    key: resolved_key.clone(),
                    entity_type: e.entity_type,
                    name: e.name.clone(),
                    aliases: e.aliases.clone(),
                    summary: e.summary.clone().unwrap_or_default(),
                    traits: e.traits.clone(),
                    first_seen_chapter: lo,
                    last_seen_chapter: hi,
                    importance: Importance::Minor,
                });
            }
        }
        report.entities_merged += 1;
        // 索引更新（新实体或新别名）
        alias_index.insert(e.name.clone(), resolved_key.clone());
        for a in &e.aliases {
            alias_index.insert(a.clone(), resolved_key.clone());
        }
    }

    // 冲突候选: 同 name 不同 key（按 name 分组计数 key）
    let mut by_name: HashMap<&str, HashSet<&str>> = HashMap::new();
    for e in &acc.entities {
        by_name
            .entry(e.name.as_str())
            .or_default()
            .insert(e.key.as_str());
        for a in &e.aliases {
            by_name
                .entry(a.as_str())
                .or_default()
                .insert(e.key.as_str());
        }
    }
    for (name, keys) in by_name {
        if keys.len() > 1 {
            let mut kv: Vec<String> = keys.into_iter().map(str::to_string).collect();
            kv.sort();
            report.conflicts.push(Conflict {
                kind: ConflictKind::SameNameDiffKey,
                keys: kv,
                names: vec![name.to_string()],
            });
        }
    }

    // ── relations ────────────────────────────────────────────
    // 幂等键: (src, dst, rel_category)——同对同类不重复; 状态取最新窗口; since 取首现
    let resolve = |k: &str| -> Option<String> {
        if window_keys.contains(k) || acc.entities.iter().any(|e| &e.key == k) {
            Some(k.to_string())
        } else {
            None
        }
    };
    for r in &w.relations {
        let (Some(src), Some(dst)) = (resolve(&r.src), resolve(&r.dst)) else {
            report
                .dropped
                .push(format!("relation 引用缺失实体: {} -> {}", r.src, r.dst));
            continue;
        };
        if let Some(existing) = acc
            .relations
            .iter_mut()
            .find(|x| x.src == src && x.dst == dst && x.rel_category == r.rel_category)
        {
            // 状态演进取最新（本窗口覆盖）
            existing.status = r.status;
            existing.rel_type = r.rel_type.clone();
            if let Some(d) = &r.description {
                existing.description = Some(d.clone());
            }
            report.relations_merged += 1;
        } else {
            acc.relations.push(GlobalRelation {
                src,
                dst,
                rel_category: r.rel_category,
                rel_type: r.rel_type.clone(),
                description: r.description.clone(),
                status: r.status,
                since_chapter: lo,
                until_chapter: if r.status == RelationStatus::Active {
                    None
                } else {
                    Some(hi)
                },
            });
            report.relations_merged += 1;
        }
    }

    // ── events ───────────────────────────────────────────────
    for ev in &w.events {
        // 不变式: 事件 key 幂等去重
        if acc.events.iter().any(|x| x.key == ev.key) {
            continue;
        }
        // 不变式: 参与者引用必须可解析, 否则丢弃并记录
        let mut participants = Vec::new();
        let mut ok = true;
        for p in &ev.participants {
            match resolve(p) {
                Some(k) => participants.push(k),
                None => {
                    report
                        .dropped
                        .push(format!("event {} 引用缺失参与者: {}", ev.key, p));
                    ok = false;
                }
            }
        }
        if !ok {
            continue;
        }
        acc.events.push(GlobalEvent {
            key: ev.key.clone(),
            title: ev.title.clone(),
            category: ev.category,
            chapter_idx: ev.chapter_idx.unwrap_or(lo),
            order: ev.order,
            participants,
            location: ev.location.clone().filter(|l| resolve(l).is_some()),
            summary: ev.summary.clone(),
            outcome: ev.outcome.clone(),
        });
        report.events_merged += 1;
    }

    // ── importance 启发式（出现章节数/度数/事件参与度）────────
    recompute_importance(acc);
    report
}

fn recompute_importance(acc: &mut GlobalGraph) {
    // seen 章节: 从实体实际参与的关系(since/until)与事件(chapter_idx)推导,
    // 不用窗口整体范围(否则同窗口龙套白得全书 span)
    let mut updates: Vec<(String, Importance)> = Vec::new();
    for e in &acc.entities {
        if e.entity_type != EntityType::Character {
            continue;
        }
        let mut seen: Vec<i64> = Vec::new();
        for r in &acc.relations {
            if r.src == e.key || r.dst == e.key {
                seen.push(r.since_chapter);
                if let Some(u) = r.until_chapter {
                    seen.push(u);
                }
            }
        }
        for ev in &acc.events {
            if ev.participants.iter().any(|p| *p == e.key) {
                seen.push(ev.chapter_idx);
            }
        }
        if seen.is_empty() {
            updates.push((e.key.clone(), Importance::Minor));
            continue;
        }
        let span = seen.iter().max().unwrap() - seen.iter().min().unwrap() + 1;
        let degree = acc
            .relations
            .iter()
            .filter(|r| r.src == e.key || r.dst == e.key)
            .count() as i64;
        let events = acc
            .events
            .iter()
            .filter(|ev| ev.participants.iter().any(|p| *p == e.key))
            .count() as i64;
        let score = span + degree * 2 + events * 3;
        let importance = if score >= 15 {
            Importance::Major
        } else if score >= 5 {
            Importance::Supporting
        } else {
            Importance::Minor
        };
        updates.push((e.key.clone(), importance));
    }
    for (key, imp) in updates {
        if let Some(e) = acc.entities.iter_mut().find(|x| x.key == key) {
            e.importance = imp;
        }
    }
    // 同步修正 seen 章节（防止窗口范围污染的旧值残留）
    for e in acc.entities.iter_mut() {
        let mut seen: Vec<i64> = Vec::new();
        for r in &acc.relations {
            if r.src == e.key || r.dst == e.key {
                seen.push(r.since_chapter);
                if let Some(u) = r.until_chapter {
                    seen.push(u);
                }
            }
        }
        for ev in &acc.events {
            if ev.participants.iter().any(|p| *p == e.key) {
                seen.push(ev.chapter_idx);
            }
        }
        if let (Some(min), Some(max)) = (seen.iter().min(), seen.iter().max()) {
            e.first_seen_chapter = *min;
            e.last_seen_chapter = *max;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph::*;

    fn win(
        chapters: &[i64],
        entities: &[(&str, &str)],
        relations: &[(&str, &str, RelCategory, RelationStatus)],
        events: &[(&str, i64, &[&str])],
    ) -> ChapterGraph {
        ChapterGraph {
            schema_version: "1".into(),
            mode: ChapterMode::Window,
            chapter_idx: None,
            chapter_summary: None,
            chapter_summaries: Some(
                chapters
                    .iter()
                    .map(|&c| ChapterSummary {
                        chapter_idx: c,
                        summary: "s".into(),
                    })
                    .collect(),
            ),
            open_threads: None,
            entities: entities
                .iter()
                .map(|(k, n)| Entity {
                    key: k.to_string(),
                    entity_type: EntityType::Character,
                    name: n.to_string(),
                    aliases: vec![],
                    summary: None,
                    traits: None,
                })
                .collect(),
            relations: relations
                .iter()
                .map(|(s, d, c, st)| Relation {
                    src: s.to_string(),
                    dst: d.to_string(),
                    rel_category: *c,
                    rel_type: "关系".into(),
                    description: None,
                    status: *st,
                })
                .collect(),
            events: events
                .iter()
                .map(|(k, ci, parts)| Event {
                    key: k.to_string(),
                    title: k.to_string(),
                    category: EventCategory::Conflict,
                    chapter_idx: Some(*ci),
                    order: 1,
                    participants: parts.iter().map(|p| p.to_string()).collect(),
                    location: None,
                    summary: "s".into(),
                    outcome: "o".into(),
                    cause: None,
                })
                .collect(),
        }
    }

    // 1. 新实体 upsert + 出现章节
    #[test]
    fn new_entity_upserted_with_seen_chapters() {
        let mut acc = GlobalGraph::default();
        let r = merge_window(&mut acc, &win(&[1, 2], &[("lin-dong", "林动")], &[], &[]));
        assert_eq!(r.entities_merged, 1);
        let e = &acc.entities[0];
        assert_eq!((e.first_seen_chapter, e.last_seen_chapter), (1, 2));
    }

    // 2. 别名并集归并进既有 key
    #[test]
    fn alias_union_merges_into_existing_key() {
        let mut acc = GlobalGraph::default();
        merge_window(&mut acc, &win(&[1], &[("lin-dong", "林动")], &[], &[]));
        // 第二窗口以 name 命中 + 新别名
        let mut w2 = win(&[2], &[("lin-dong", "林动")], &[], &[]);
        w2.entities[0].aliases.push("林家小子".into());
        let r = merge_window(&mut acc, &w2);
        assert_eq!(r.entities_merged, 1, "归并不新增");
        assert_eq!(acc.entities.len(), 1);
        assert!(acc.entities[0].aliases.contains(&"林家小子".to_string()));
        assert_eq!(acc.entities[0].last_seen_chapter, 2);
    }

    // 3. 关系按 (src,dst,category) 幂等
    #[test]
    fn relation_idempotent_by_src_dst_category() {
        let mut acc = GlobalGraph::default();
        let rels = [(
            "lin-dong",
            "lin-xiao",
            RelCategory::Enmity,
            RelationStatus::Active,
        )];
        let w = win(
            &[1],
            &[("lin-dong", "林动"), ("lin-xiao", "林啸")],
            &rels,
            &[],
        );
        merge_window(&mut acc, &w);
        merge_window(&mut acc, &w); // 重复窗口
        assert_eq!(acc.relations.len(), 1, "重复窗口不重复插入");
    }

    // 4. 关系状态取最新窗口（active → broken 演进）
    #[test]
    fn relation_status_takes_latest_window() {
        let mut acc = GlobalGraph::default();
        let e = [("lin-dong", "lin-xiao"), ("lin-dong", "lin-xiao")];
        let ents = [("lin-dong", "林动"), ("lin-xiao", "林啸")];
        merge_window(
            &mut acc,
            &win(
                &[1],
                &ents,
                &[(
                    "lin-dong",
                    "lin-xiao",
                    RelCategory::Alliance,
                    RelationStatus::Active,
                )],
                &[],
            ),
        );
        merge_window(
            &mut acc,
            &win(
                &[3],
                &ents,
                &[(
                    "lin-dong",
                    "lin-xiao",
                    RelCategory::Alliance,
                    RelationStatus::Broken,
                )],
                &[],
            ),
        );
        assert_eq!(acc.relations.len(), 1);
        assert_eq!(acc.relations[0].status, RelationStatus::Broken);
        assert_eq!(acc.relations[0].since_chapter, 1, "since 取首现");
    }

    // 5. 事件 key 去重
    #[test]
    fn event_key_dedup() {
        let mut acc = GlobalGraph::default();
        let ents = [("lin-dong", "林动")];
        let evs = [("ev-1-1", 1, &["lin-dong"][..])];
        merge_window(&mut acc, &win(&[1], &ents, &[], &evs));
        merge_window(&mut acc, &win(&[1], &ents, &[], &evs));
        assert_eq!(acc.events.len(), 1);
    }

    // 6. 引用缺失实体 → 丢弃并记录
    #[test]
    fn references_must_resolve_or_dropped() {
        let mut acc = GlobalGraph::default();
        let r = merge_window(
            &mut acc,
            &win(
                &[1],
                &[("lin-dong", "林动")],
                &[],
                &[("ev-1-1", 1, &["ghost"])],
            ),
        );
        assert!(acc.events.is_empty());
        assert!(r.dropped.iter().any(|d| d.contains("ghost")));
    }

    // 7. importance 启发式
    #[test]
    fn importance_heuristic() {
        let mut acc = GlobalGraph::default();
        // 主角: 跨 10 章 + 3 关系 + 3 事件 → score = 10+6+9 = 25 ≥ 15 → major
        let ents = [
            ("lin-dong", "林动"),
            ("a1", "甲"),
            ("a2", "乙"),
            ("a3", "丙"),
        ];
        let rels = [
            (
                "lin-dong",
                "a1",
                RelCategory::Enmity,
                RelationStatus::Active,
            ),
            (
                "lin-dong",
                "a2",
                RelCategory::Alliance,
                RelationStatus::Active,
            ),
            (
                "lin-dong",
                "a3",
                RelCategory::Family,
                RelationStatus::Active,
            ),
        ];
        let evs = [
            ("ev-1-1", 1, &["lin-dong"][..]),
            ("ev-2-1", 2, &["lin-dong"][..]),
            ("ev-3-1", 3, &["lin-dong"][..]),
        ];
        merge_window(&mut acc, &win(&[1, 10], &ents, &rels, &evs));
        let hero = acc.entities.iter().find(|e| e.key == "lin-dong").unwrap();
        assert_eq!(hero.importance, Importance::Major);
        let minor = acc.entities.iter().find(|e| e.key == "a3").unwrap();
        assert_eq!(minor.importance, Importance::Minor, "度数1无事件 → minor");
    }

    // 8. 同名异 key 冲突进报告（消歧 pass 候选）
    #[test]
    fn same_name_diff_key_reported() {
        let mut acc = GlobalGraph::default();
        merge_window(&mut acc, &win(&[1], &[("lin-dong", "林动")], &[], &[]));
        let r = merge_window(&mut acc, &win(&[5], &[("lin-dong-2", "林动")], &[], &[]));
        assert!(r
            .conflicts
            .iter()
            .any(|c| c.kind == ConflictKind::SameNameDiffKey
                && c.keys.contains(&"lin-dong".to_string())));
        assert_eq!(acc.entities.len(), 2, "不自动合并, 交消歧裁决");
    }
}
