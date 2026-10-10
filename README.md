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

## Knowledge-graph visualization

`src/components/GraphVisualization.vue` is a read-only, props-driven Cytoscape/fcose view with entity-type filters, source colors, element properties, and a linked event timeline. Desktop uses three panes; narrow screens use a full canvas and a bottom property sheet. Pinia and the shared CSS tokens must be installed by the host.

Pass `nodes`, `edges`, `events`, and an explicit `purpose` (`actual`, `reference`, or `spec`). These are public presentation records from `src/lib/graph.ts`, not LLM extraction envelopes. The backend adapter must provide already-authorized names, aliases, references, and opaque public IDs; the component does not receive a complete future graph and redact it locally. Provenance colors indicate origin, not fact eligibility; user records are labeled as intent.

Chapter navigation is opt-in: pass `canJump` with a real `jump(chapterIdx)` handler. The reading workbench loads this view lazily; extraction and mutation stay in Rust, not in the rendering component.

## Evidence-backed knowledge graph

- Import queues book extraction. Stage A extracts evidence-backed event/mention candidates; identity decisions align them; Stage B extracts facts, independently keyed relationships, changes, and event prerequisites. Long chapters are internally budgeted by UTF-8-safe spans without changing the original chapter list.
- Immutable text versions and whole extraction revisions own every evidence range and fact. Global and chapter graphs are projections of the same records. Current facts, original reference, and future specifications are queried separately; future AI details and identity names are hidden server-side until authorized.
- Reading opens original text before the earliest active edit, and generated text afterward. Future graph edits, directives and drift preferences leave already-observed facts unchanged. Generation streams prose, back-extracts that exact text version, then reconciles the next specification. Saved prose remains readable with a warning if a later graph stage fails; dependent generation waits for actual facts.
- Revoking edits invalidates dependent selections and returns to the deterministic reading line. Cached prose/facts/specifications are reused only when immutable content, prior facts, identity dependencies and extraction/refinement versions match. Old model graph rows without evidence are archived for re-extraction, not treated as new facts.
- Configure extract, merge and write models in Provider settings. Context and output budgets default to 32,768 and 4,096 tokens and can be adjusted. In-flight work is serialized per book and globally limited; a foreground request waits for the current chapter transaction boundary, then precedes the next queued background chapter.
- The workbench exposes read-only current facts/original reference and editable next-chapter/global future intent. Blind user-created intent stays visible; AI details require the spoiler-confirmation reveal gate. Selected elements support evidence lookup, future changes/deletion, and edit-history reversal. The reader shows target-chapter stream deltas while loading, then replaces them with the saved authoritative text.
- Database and the current credential vault live in Tauri's application-data directory. A first launch migrates a legacy working-directory database without overwriting existing application data. `RIMENOVEL_DATA_DIR` selects an isolated data directory for smoke runs. Credentials still use the existing local vault; this is not an OS-keyring security claim.

Offline tests and loopback compatible-provider smoke runs verify lifecycle, evidence and projection behavior. They do not measure real-model extraction accuracy or guarantee adherence to all writing instructions; real-provider quality evaluation remains separate.
