# RimeNovel

A novel reader where readers can influence the story direction: import a novel → extract a dual-layer knowledge graph (global/chapter) → visualize and edit → AI generates subsequent chapters based on the edited graph.

## Development

```bash
pnpm install
pnpm tauri dev # Desktop development
pnpm test # Frontend vitest
cargo test --manifest-path src-tauri/Cargo.toml # Rust tests
```

Tech stack: Tauri 2 · Rust (tokio/reqwest/rusqlite) · Vue 3 · TS · Naive UI · Cytoscape.js