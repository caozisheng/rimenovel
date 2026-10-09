# RimeNovel

可被读者干预走向的小说阅读器：导入小说 → 抽取双层知识图谱（全局/章节）→ 可视化编辑 → AI 按编辑后的图谱连载生成后续章节。

## 文档

- [总体设计](docs/rimenovel-overall-design.md)（v1.0 已冻结）
- [实施计划](docs/rimenovel-dev-plan.md)
- 图谱 Schema: [chapter](docs/schemas/chapter-graph.schema.json) · [global](docs/schemas/global-graph.schema.json)

## 开发

```bash
pnpm install
pnpm tauri dev      # 桌面开发
pnpm test           # 前端 vitest
cargo test --manifest-path src-tauri/Cargo.toml   # Rust 测试
```

技术栈：Tauri 2 · Rust(tokio/reqwest/rusqlite) · Vue 3 + TS + Naive UI · Cytoscape.js
