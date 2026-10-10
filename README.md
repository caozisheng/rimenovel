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
- Saved Providers can be edited from the list; saving updates the same ID even after a rename. Connection settings, models and budgets are filled in, but credentials are never returned to the form: leave API Key blank to retain it or enter a new key to replace it. Cancel discards the draft; creating a Provider requires a key and an unused name.
- The workbench exposes read-only current facts/original reference and editable next-chapter/global future intent. Blind user-created intent stays visible; AI details require the spoiler-confirmation reveal gate. Selected elements support evidence lookup, future changes/deletion, and edit-history reversal. The reader shows target-chapter stream deltas while loading, then replaces them with the saved authoritative text.
- The workbench shows durable background task kind, state, chapter, phase, attempt, retry delay, progress and failure details. Running work takes priority over queued successors; failed prefetch blockers remain visible and actionable. Start/retry explicitly without reimporting, or cancel all unfinished tasks for the current book. Closing the workbench does not cancel work. Events and three-second refreshes read SQLite status; progress advances on committed chapters, not guessed model-token percentages.
- Database and the current credential vault live in Tauri's application-data directory. A first launch migrates a legacy working-directory database without overwriting existing application data. `RIMENOVEL_DATA_DIR` selects an isolated data directory for smoke runs. Credentials still use the existing local vault; this is not an OS-keyring security claim.

## LLM adapters and bounded execution

- OpenAI-compatible Chat Completions use pinned `async-openai` 0.42.2; Anthropic retains its native Messages/forced-tool adapter. Keys, provider configuration and SDK objects remain in Rust. `LlmClient` separates provider code from the KG pipeline; no general agent harness or state-graph dependency is added.
- Every structured call sends its output budget and exact schema, then validates the whole result locally. OpenAI structured output is collected privately over SDK SSE and published only after normal completion and JSON/schema validation; partial JSON is discarded on interruption. Truncation, refusal, malformed JSON and incomplete streams fail rather than publishing partial graphs/prose. A byte-prefix safeguard prevents the pinned SSE dependency's leading-BOM panic without reimplementing event parsing.
- One shared policy permits at most three attempts for transient network failures, timeouts and HTTP 408/429/500/502/503/504. It honors Retry-After seconds/dates up to 30 seconds; otherwise waits 1/2 seconds plus bounded jitter. Connect timeout is 15 seconds, read inactivity timeout is 60 seconds, each attempt is bounded to 180 seconds and a complete call to 600 seconds. SDK/transport implicit retries are disabled. Privately buffered structured responses may retry after transport interruption; delivered prose is never automatically replayed.
- Cancellation interrupts requests, queued resource waits and retry waits, and closes the operation-owned OpenAI response body even if the SDK reader is idle. Durable writes and cancellation share a commit gate; cancelled jobs cannot be overwritten by late success, failure or checkpoints. Completed revisions remain reusable; a new explicit reading/start action may create new work, but restart recovery does not revive cancelled rows.
- Failed extraction/generation stops automatic prefetch for that book, including repeated chapter openings and jobs queued before the failure. On restart, blocked queued work becomes a visible failure with the predecessor error, not a permanently queued task. Explicit retries remain available; a successful explicit task reopens automatic prefetch. Failure history and committed checkpoints are retained, and unrelated books are not blocked.
- HTTP 503/504 still require a usable upstream provider. Bounded retries improve handling, not model/service availability; the application does not promise that switching SDKs cures gateway timeouts.

Offline tests and loopback compatible-provider smoke runs verify lifecycle, evidence and projection behavior. They do not measure real-model extraction accuracy or guarantee adherence to all writing instructions; real-provider quality evaluation remains separate.
