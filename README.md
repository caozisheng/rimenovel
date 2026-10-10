# RimeNovel

<img width="128" height="128" alt="rimenovel-icon" src="https://github.com/user-attachments/assets/f1bedd95-909e-454e-926f-9b339ab64654" />

A novel reader where readers can influence the story direction: import a novel → extract a dual-layer knowledge graph (global/chapter) → visualize and edit → AI generates subsequent chapters based on the edited graph.

## Development

```bash
pnpm install
pnpm tauri dev # Desktop development
pnpm test # Frontend vitest
cargo test --manifest-path src-tauri/Cargo.toml # Rust tests
```

Tech stack: Tauri 2 · Rust (tokio/reqwest/rusqlite) · Vue 3 · TS · Naive UI · Cytoscape.js

## UI foundation

- The bookshelf and Provider settings use Naive UI with Chinese locale. Light, dark, and automatic application themes share CSS design tokens; automatic mode responds to system theme changes. The application theme is saved locally.
- Each bookshelf card has a trash icon that opens a confirmation dialog. Confirming deletes the book and its chapters, graph/reading data, jobs, and usage records; the original TXT/EPUB file is retained. Cancelling leaves the book unchanged.
- The reader has independent paper, sepia, and night themes, a serif font stack, 1.9 line height, paragraph indentation, and live 16–22 px font sizing. Its controls and Provider form adapt to narrow screens.
- `pnpm dev` previews the frontend only. Import, chapters, and Provider operations require the Tauri backend; use `pnpm tauri dev` for desktop use.
