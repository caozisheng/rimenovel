// 抽取 prompt 构造（dev-plan Task 4.3 前半，纯函数）
// user 消息四段（设计 §4.2.1）: 锚定实体表 / 上窗口末尾接缝 / 窗口原文 / 章节索引。
// system 说明 schema 语义与 key 复用规则。

use crate::core::graph::GlobalGraph;

#[derive(Debug, Clone)]
pub struct WindowSpec {
    /// 窗口包含的章 (idx, title, text)
    pub chapters: Vec<(i64, String, String)>,
    /// 上一窗口末尾接缝文本（~500 token）
    pub seam: String,
}

/// 锚定实体表：全局已确认 key → name/aliases/type（防实体漂移）
fn anchor_table(global: &GlobalGraph, limit: usize) -> String {
    if global.entities.is_empty() {
        return "（暂无，本窗口是首个窗口，为每个实体生成新的拼音 slug key）".into();
    }
    let mut lines: Vec<String> = global
        .entities
        .iter()
        .map(|e| {
            let aliases = if e.aliases.is_empty() {
                String::new()
            } else {
                format!("（别名: {}）", e.aliases.join("、"))
            };
            format!(
                "- {} | {} | {}{}",
                e.key,
                entity_type_cn(e.entity_type),
                e.name,
                aliases
            )
        })
        .collect();
    lines.truncate(limit);
    lines.join("\n")
}

fn entity_type_cn(t: crate::core::graph::EntityType) -> &'static str {
    use crate::core::graph::EntityType::*;
    match t {
        Character => "人物",
        Location => "地点",
        Item => "物品",
        Faction => "势力",
        Concept => "概念",
    }
}

pub fn system_prompt() -> String {
    "你是小说知识图谱抽取器。从给定章节文本中抽取实体、关系、事件，严格输出符合要求 JSON Schema 的单个 JSON 对象。\n\
     规则：\n\
     1. 实体 key 是小写拼音 slug（如 lin-dong）；已在「已知实体表」中的实体必须复用原 key，不得新造；\n\
     2. 新实体生成新 slug，name 用最常用称呼，别名收进 aliases；\n\
     3. 关系标注本章结束时的状态（active/broken/former/unknown）；\n\
     4. 事件按叙事顺序编号 order（章内从 1 递增），key 格式 ev-<章号>-<序号>；\n\
     5. 所有 relations 的 src/dst 与 events 的 participants/location 必须引用本输出或已知实体表中的 key；\n\
     6. 只依据文本内容，不臆测未发生的事。"
        .into()
}

/// 窗口抽取的 user 消息（四段式）
pub fn window_user_prompt(spec: &WindowSpec, global: &GlobalGraph) -> String {
    let idx_list = spec
        .chapters
        .iter()
        .map(|(i, t, _)| format!("第{i}章 {t}"))
        .collect::<Vec<_>>()
        .join("；");

    let body = spec
        .chapters
        .iter()
        .map(|(i, t, txt)| format!("【第{i}章 {t}】\n{txt}"))
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "## 已知实体表（必须复用这些 key）\n{}\n\n\
         ## 上一窗口结尾（接缝，仅供上下文衔接，不抽取）\n{}\n\n\
         ## 本窗口正文（待抽取）\n{}\n\n\
         ## 窗口章节索引\n{}",
        anchor_table(global, 200),
        if spec.seam.is_empty() {
            "（无）"
        } else {
            &spec.seam
        },
        body,
        idx_list,
    )
}

/// 全书窗口切分（按章列表，默认 8-12 章/窗口）
pub fn plan_windows(chapters: &[(i64, String, String)], window_size: usize) -> Vec<WindowSpec> {
    let mut specs = Vec::new();
    for chunk in chapters.chunks(window_size) {
        let first_idx = chunk.first().map(|c| c.0).unwrap_or(0);
        let seam = chapters
            .iter()
            .take_while(|c| c.0 < first_idx)
            .last()
            .map(|(_, _, t)| t.chars().rev().take(700).collect::<String>())
            .map(|s| s.chars().rev().collect())
            .unwrap_or_default();
        specs.push(WindowSpec {
            chapters: chunk.to_vec(),
            seam,
        });
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapters(n: usize) -> Vec<(i64, String, String)> {
        (1..=n as i64)
            .map(|i| {
                (
                    i,
                    format!("第{i}章"),
                    format!("第{i}章正文。{}内容。", "很长的".repeat(50)),
                )
            })
            .collect()
    }

    #[test]
    fn plan_windows_chunks_and_seam() {
        let cs = chapters(20);
        let specs = plan_windows(&cs, 10);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].chapters.len(), 10);
        assert_eq!(specs[0].chapters[0].0, 1);
        // 第二窗口接缝 = 第一窗口末章文本尾部
        assert!(!specs[1].seam.is_empty());
        assert!(specs[1].seam.chars().count() <= 700);
        // 首窗口无接缝
        assert!(specs[0].seam.is_empty());
    }

    #[test]
    fn user_prompt_contains_all_four_sections() {
        let spec = plan_windows(&chapters(3), 10).remove(0);
        let g = GlobalGraph::default();
        let p = window_user_prompt(&spec, &g);
        assert!(p.contains("## 已知实体表"));
        assert!(p.contains("## 本窗口正文（待抽取）"));
        assert!(p.contains("【第1章"));
        assert!(p.contains("## 窗口章节索引"));
        assert!(p.contains("首个窗口"));
    }

    #[test]
    fn anchor_table_lists_entities_with_keys() {
        use crate::core::graph::*;
        let mut g = GlobalGraph::default();
        g.entities.push(GlobalEntity {
            key: "lin-dong".into(),
            entity_type: EntityType::Character,
            name: "林动".into(),
            aliases: vec!["林家小子".into()],
            summary: String::new(),
            traits: None,
            first_seen_chapter: 1,
            last_seen_chapter: 5,
            importance: Importance::Major,
        });
        let spec = plan_windows(&chapters(2), 10).remove(0);
        let p = window_user_prompt(&spec, &g);
        assert!(p.contains("lin-dong | 人物 | 林动（别名: 林家小子）"));
    }
}
