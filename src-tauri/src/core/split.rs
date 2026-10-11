// txt 分章器（dev-plan Task 2.1，TDD 核心）
// 切分完全遵循原书目录结构；序号按出现顺序重编，不信任标题内数字。
// 参考: docs/rimenovel-overall-design.md §4.1

#[derive(Debug, Clone)]
pub struct RawChapter {
    pub idx: i64, // 从 1 起（含前言块）
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct SplitOptions {
    /// 章节标题行最大长度（防对话误切）
    pub max_title_len: usize,
    /// 采纳某正则的最小匹配数（防个别行误命中）
    pub min_matches: usize,
}

impl Default for SplitOptions {
    fn default() -> Self {
        Self {
            max_title_len: 40,
            min_matches: 2,
        }
    }
}

/// 章节标题正则（按优先级顺序尝试，取首个匹配数达标的规则）
/// 1. 第X章/节/卷/回/集/部（中文数字或阿拉伯数字，可含空格/标点尾巴）
/// 2. 第X话（需空格或标点分隔标题，避免“第一话题”误切）
/// 3. Chapter N / CHAPTER N
/// 4. 数字章（"1. 标题" / "一、标题" / "一　标题"）
const PATTERNS: &[&str] = &[
    r"^\s*第\s*[0-9〇零一二两三四五六七八九十百千万亿]+\s*[章节卷回集部]\s*[^\n]{0,30}$",
    r"^\s*第\s*[0-9〇零一二两三四五六七八九十百千万亿]+\s*话(?:[\s：:、.．]+[^\n]{1,30})?\s*$",
    r"^\s*(?:Chapter|CHAPTER|chapter)\s+\d+.*$",
    r"^\s*[0-9〇零一二两三四五六七八九十]+\s*[、.．]\s*[^\n]{1,30}$",
    r"^\s*[0-9〇零一二两三四五六七八九十]+[\s　]+[^\n]{1,30}$",
];

/// 对全文按章节标题行切分。无任何规则达标 → 整书单章。
pub fn split_txt(text: &str, opts: &SplitOptions) -> Vec<RawChapter> {
    let lines: Vec<&str> = text.lines().collect();

    // 选出首个匹配数达标的模式
    let mut chosen: Option<(usize, Vec<bool>)> = None;
    for (pi, pat) in PATTERNS.iter().enumerate() {
        let re: regex::Regex = match regex::Regex::new(pat) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let hits: Vec<bool> = lines
            .iter()
            .map(|l| {
                let t = l.trim();
                !t.is_empty()
                    && t.chars().count() <= opts.max_title_len
                    && re.is_match(t)
                    // 纯数字行（页码等）不当标题
                    && !t.chars().all(|c| c.is_ascii_digit())
            })
            .collect();
        let count = hits.iter().filter(|&&h| h).count();
        if count >= opts.min_matches {
            chosen = Some((pi, hits));
            break;
        }
    }

    let (pattern_idx, hits) = match chosen {
        Some(c) => c,
        // 无规则达标：整书一章
        None => {
            return vec![RawChapter {
                idx: 1,
                title: "正文".into(),
                text: text.to_string(),
            }];
        }
    };
    let _ = pattern_idx;

    // 首个标题行之前的内容 → 前言章
    let mut chapters: Vec<RawChapter> = Vec::new();
    let first_hit = hits.iter().position(|&h| h).unwrap();
    if first_hit > 0 {
        let preface: Vec<&str> = lines[..first_hit].to_vec();
        let preface_text = preface.join("\n").trim().to_string();
        if !preface_text.is_empty() {
            chapters.push(RawChapter {
                idx: 1,
                title: "前言".into(),
                text: preface_text,
            });
        }
    }

    // 逐标题切片
    let mut current: Option<(String, Vec<&str>)> = None;
    for (i, line) in lines.iter().enumerate() {
        if hits.get(i).copied().unwrap_or(false) {
            if let Some((title, body)) = current.take() {
                chapters.push(RawChapter {
                    idx: 0,
                    title,
                    text: body.join("\n").trim().to_string(),
                });
            }
            current = Some((line.trim().to_string(), Vec::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some((title, body)) = current.take() {
        chapters.push(RawChapter {
            idx: 0,
            title,
            text: body.join("\n").trim().to_string(),
        });
    }
    chapters.retain(|chapter| !chapter.text.trim().is_empty());
    // 序号按出现顺序重编
    for (i, ch) in chapters.iter_mut().enumerate() {
        ch.idx = (i + 1) as i64;
    }
    chapters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_standard_chinese_chapters() {
        let txt = "前言内容。\n第一章 起点\n正文A\n第二章 风波\n正文B\n";
        let cs = split_txt(txt, &SplitOptions::default());
        assert_eq!(cs.len(), 3, "前言+2章: {cs:?}");
        assert_eq!(cs[0].title, "前言");
        assert_eq!(cs[1].title, "第一章 起点");
        assert!(cs[1].text.contains("正文A"));
        assert_eq!(cs[2].title, "第二章 风波");
    }

    #[test]
    fn handles_chinese_numerals_and_variants() {
        let txt = "第一百二十三章 破境\nA\n第一百二十四章 追猎\nB\n";
        let cs = split_txt(txt, &SplitOptions::default());
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].title, "第一百二十三章 破境");

        let txt2 = "第12卷 天骄\nA\n第13卷 峥嵘\nB\n";
        let cs2 = split_txt(
            txt2,
            &SplitOptions {
                min_matches: 2,
                ..Default::default()
            },
        );
        assert_eq!(cs2.len(), 2);
    }

    #[test]
    fn splits_episode_headings_but_not_numbered_discussion_topics() {
        let text = "引子\n背景。\n第一话 古瓶\n阿豪与藤明月出发。\n第二话 离奇的死亡事件\n老马讲述经过。\n";
        let chapters = split_txt(text, &SplitOptions::default());
        assert_eq!(chapters.iter().map(|chapter| chapter.title.as_str()).collect::<Vec<_>>(),
            ["前言", "第一话 古瓶", "第二话 离奇的死亡事件"]);
        assert_eq!(chapters[1].text, "阿豪与藤明月出发。");
        assert_eq!(chapters[2].text, "老马讲述经过。");
        let topics = "第一话题是时间。\n讨论正文。\n第二话题是关系。\n继续讨论正文。";
        assert_eq!(split_txt(topics, &SplitOptions::default())[0].text, topics);
    }

    #[test]
    fn splits_chinese_numeral_headings_with_ideographic_space() {
        let txt = "一　灭门\n正文A\n二　聆秘\n正文B\n三　救难\n正文C\n";
        let cs = split_txt(txt, &SplitOptions::default());
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].title, "一　灭门");
        assert_eq!(cs[2].title, "三　救难");
    }

    #[test]
    fn handles_english_chapters() {
        let txt = "Intro\nChapter 1 The Beginning\nAAA\nChapter 2 The Storm\nBBB\n";
        let cs = split_txt(txt, &SplitOptions::default());
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[1].title, "Chapter 1 The Beginning");
    }

    #[test]
    fn no_headings_yields_single_chunk() {
        let txt = "没有任何标题行的纯文本。\n继续正文。\n";
        let cs = split_txt(txt, &SplitOptions::default());
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].title, "正文");
        assert!(cs[0].text.contains("继续正文"));
    }

    #[test]
    fn ignores_false_positives_in_dialogue() {
        // 对话里提到"第二章"，但全文只有一处命中 → 不达 min_matches → 单章
        let txt = "他说：「你的武功已到第二章境界。」\n林动点头。\n";
        let cs = split_txt(txt, &SplitOptions::default());
        assert_eq!(cs.len(), 1);

        // 长行（疑似正文段）即使命中正则也不当标题
        let txt2 = "第二章的事情他说了很多很多很多很多很多很多很多很多很多很长的正文段落内容超长。\n正文。\n第一章 起点\nA\n第二章 风波\nB\n";
        let cs2 = split_txt(
            txt2,
            &SplitOptions {
                max_title_len: 40,
                ..Default::default()
            },
        );
        assert_eq!(cs2.len(), 3, "长行被忽略: {cs2:?}");
    }

    #[test]
    fn indices_are_sequential_regardless_of_title_numbers() {
        // 标题数字乱序/重复 → idx 仍按出现顺序 1..n
        let txt = "第9章 倒叙\nA\n第3章 乱序\nB\n第9章 重复\nC\n";
        let cs = split_txt(txt, &SplitOptions::default());
        assert_eq!(cs.len(), 3);
        assert_eq!((cs[0].idx, cs[1].idx, cs[2].idx), (1, 2, 3));
    }
    #[test]
    fn drops_empty_chapters_created_by_adjacent_headings() {
        let text = "第一章\n\n第二章\n正文\n第三章\n";
        let chapters = split_txt(text, &SplitOptions::default());
        assert_eq!(chapters.iter().map(|chapter| chapter.title.as_str()).collect::<Vec<_>>(), ["第二章"]);
        assert_eq!(chapters[0].text, "正文");
    }

    #[test]
    fn empty_heading_only_input_has_no_chapters() {
        let chapters = split_txt("第一章\n第二章\n", &SplitOptions::default());
        assert!(chapters.is_empty());
    }
}
