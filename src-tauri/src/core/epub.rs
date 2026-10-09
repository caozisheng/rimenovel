// epub 分章器（dev-plan Task 2.2）
// 解析 container.xml → OPF → spine+TOC(ncx/nav) → 章列表；
// TOC 缺失 → 拼全文走 txt 策略（设计 §4.1）。

use crate::core::split::{split_txt, RawChapter, SplitOptions};
use std::io::Read;

/// 从 zip 字节中取出指定扩展名的文件 (name, bytes)。
fn read_zip_files(data: &[u8], want_ext: &[&str]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    if let Ok(mut reader) = zip::ZipArchive::new(std::io::Cursor::new(data)) {
        for i in 0..reader.len() {
            if let Ok(mut f) = reader.by_index(i) {
                let name = f.name().unwrap_or_default().to_string();
                let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
                if want_ext.contains(&ext.as_str()) {
                    let mut buf = Vec::new();
                    if f.read_to_end(&mut buf).is_ok() {
                        out.push((name, buf));
                    }
                }
            }
        }
    }
    out
}

fn strip_html(html: &str) -> String {
    // 块级标签 → 换行，其余剔除；解码基础实体
    let mut s = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag_buf = String::new();
    const BLOCK: &[&str] = &[
        "p",
        "div",
        "h1",
        "h2",
        "h3",
        "h4",
        "br",
        "li",
        "tr",
        "blockquote",
    ];
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag_buf.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let name = tag_buf
                    .trim()
                    .trim_matches('/')
                    .split(|ch: char| ch.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                if BLOCK.contains(&name.as_str()) {
                    s.push('\n');
                }
            }
            _ if in_tag => tag_buf.push(c),
            _ => s.push(c),
        }
    }
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .trim()
        .to_string()
}

/// epub → 章节。TOC/spine 可解析时按 spine 顺序产出；否则全文走 txt 切分。
pub fn split_epub(data: &[u8], opts: &SplitOptions) -> Result<Vec<RawChapter>, String> {
    let mut opf: Option<String> = None;
    let mut docs: Vec<(String, String)> = Vec::new(); // (path, 纯文本)

    for (name, bytes) in read_zip_files(data, &["opf", "ncx", "html", "xhtml", "htm"]) {
        let text = String::from_utf8_lossy(&bytes).to_string();
        let lower = name.to_lowercase();
        if lower.ends_with(".opf") {
            opf = Some(text);
        } else {
            docs.push((name.clone(), strip_html(&text)));
        }
    }
    let opf = opf.ok_or("epub 缺少 OPF")?;

    // spine 顺序：解析 <itemref idref="..."/> + <item id="..." href="..."/>
    let id_to_href: Vec<(String, String)> = {
        let re = regex::Regex::new(r#"<item\s[^>]*id="([^"]+)"[^>]*href="([^"]+)"#).unwrap();
        let re2 = regex::Regex::new(r#"<item\s[^>]*href="([^"]+)"[^>]*id="([^"]+)"#).unwrap();
        let mut v: Vec<(String, String)> = re
            .captures_iter(&opf)
            .map(|c| (c[1].to_string(), c[2].to_string()))
            .collect();
        v.extend(
            re2.captures_iter(&opf)
                .map(|c| (c[2].to_string(), c[1].to_string())),
        );
        v
    };
    let spine: Vec<String> = {
        let re = regex::Regex::new(r#"<itemref\s[^>]*idref="([^"]+)""#).unwrap();
        re.captures_iter(&opf)
            .filter_map(|c| {
                let id = c[1].to_string();
                id_to_href
                    .iter()
                    .find(|(i, _)| i == &id)
                    .map(|(_, h)| h.to_string())
            })
            .collect()
    };

    if spine.is_empty() {
        // 无 spine 信息：拼全文走 txt 策略
        let full = docs
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(split_txt(&full, opts));
    }

    // 按 spine 顺序组章；标题取文档首个非空行（常见 epub 惯例）
    let mut chapters = Vec::new();
    for href in &spine {
        let norm = href.trim_start_matches("./").to_lowercase();
        let doc = docs
            .iter()
            .find(|(n, _)| n.to_lowercase().ends_with(&norm))
            .map(|(_, t)| t.clone());
        if let Some(text) = doc {
            if text.trim().is_empty() {
                continue;
            }
            let title = text
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("无题")
                .trim()
                .to_string();
            chapters.push(RawChapter {
                idx: 0,
                title,
                text,
            });
        }
    }
    for (i, ch) in chapters.iter_mut().enumerate() {
        ch.idx = (i + 1) as i64;
    }
    Ok(chapters)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 构造最小 3 章 epub（zip 字节）
    fn mini_epub() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            w.start_file(
                "META-INF/container.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            w.write_all(br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#).unwrap();
            w.start_file(
                "OEBPS/content.opf",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            w.write_all(br#"<?xml version="1.0"?><package xmlns:opf="http://www.idpf.org/2007/opf"><manifest><item id="c1" href="c1.xhtml"/><item id="c2" href="c2.xhtml"/><item id="c3" href="c3.xhtml"/></manifest><spine><itemref idref="c1"/><itemref idref="c2"/><itemref idref="c3"/></spine></package>"#).unwrap();
            for i in 1..=3 {
                w.start_file(
                    format!("OEBPS/c{i}.xhtml"),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
                let html = format!(
                    "<html><body><h1>第{i}章 试炼</h1><p>第{i}章正文内容。</p></body></html>"
                );
                w.write_all(html.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn parses_spine_into_three_chapters() {
        let data = mini_epub();
        let cs = split_epub(&data, &SplitOptions::default()).unwrap();
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].idx, 1);
        assert_eq!(cs[0].title, "第1章 试炼");
        assert!(cs[1].text.contains("第2章正文内容"));
    }
}
