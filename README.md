# RimeNovel

<img width="128" height="128" alt="rimenovel-icon" src="https://github.com/user-attachments/assets/f1bedd95-909e-454e-926f-9b339ab64654" />

小说阅读器：导入小说 → 按章整理轻量 Wiki → 阅读时查看只读 StoryMap → 通过三道迷雾选择题或一句话影响下一章 → 生成并继续阅读。

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

# 轻量 Wiki 与 StoryMap

`src/components/WikiWorkbench.vue` 是阅读器内的只读故事工作台。它展示截至当前阅读线的 Wiki 条目、章节卡片、已知状态、关联条目和未解线索；用户不直接编辑节点、边或 Wiki 字段。

- 每章只产生一份轻量章节整理，保留摘要、主要人物/地点/物品、关系、已确认状态和未解线索；不再把章内时间线拆成多个事件并逐事件计算图谱。
- Wiki 是系统的速查和续写约束，不是第二份正文。原文、生成正文和章节整理按选中阅读线与版本保存，未读章节不会出现在公开历史中。
- Reader 的 `Wiki / StoryMap` 面板支持跳转章节。章节正文仍保留原文或生成阅读线来源，整理失败不阻止已保存正文阅读。
- 下一章干预只通过每批恰好三道迷雾选择题和可选自由文字提交。文字覆盖冲突选项；未回答的问题不产生约束；已提交答案保留在历史中。
- 顶栏可在“原文”和“改写”之间切换。两条阅读线独立持久化：切换不会删除用户干预、规划或已生成章节；改写尚未到达的章节不会静默冒充已改写内容。
- 改写正文按章节逐步保存，改写线不等待全书分析完成。分析/Wiki 是旁路记忆，改写生成、保存和阅读可以独立推进。
- 题批和下一章规划使用独立结构化响应。规划未完成时不能以旧图谱或隐藏原作细节替代；正文生成只接收历史 Wiki、上一章选中完整正文和有效下一章规划。

## 导入与运行

- 导入只保留原书章节划分；章节整理在后台按预算执行，缓存命中不重复付费请求。
- `pnpm dev` 仅预览前端；导入、章节、Provider、Wiki 规划和正文生成需要 Tauri 后端，使用 `pnpm tauri dev`。
- Provider 分别配置 extract、merge、write 模型。真实模型质量、延迟和整书成本必须单独验收，不由离线测试推断。
- Database and the current credential vault live in Tauri's application-data directory. A first launch migrates a legacy working-directory database without overwriting existing application data. `RIMENOVEL_DATA_DIR` selects an isolated data directory for smoke runs. Credentials still use the existing local vault; this is not an OS-keyring security claim.

## LLM adapters and bounded execution

- OpenAI-compatible Chat Completions use pinned `async-openai` 0.42.2; Anthropic retains its native Messages/forced-tool adapter. Keys, provider configuration and SDK objects remain in Rust. `LlmClient` separates provider code from the KG pipeline; no general agent harness or state-graph dependency is added.
- Every structured call sends its output budget and exact schema, then validates the whole result locally. OpenAI structured output is collected privately over SDK SSE and published only after normal completion and JSON/schema validation; partial JSON is discarded on interruption. Truncation, refusal, malformed JSON and incomplete streams fail rather than publishing partial graphs/prose. A byte-prefix safeguard prevents the pinned SSE dependency's leading-BOM panic without reimplementing event parsing.
- Chapter extraction derives a typed reference schema from the supplied registry: new references use `c_` (character), `l_` (location), `i_` (item), or `f_` (faction). Entity/event locations accept only registered location references or declared new `l_` references; item owners accept character/faction references. SDK and pipeline validation reject illegal references without guessing places, clearing states, or publishing an invalid revision. The request schema participates in the LLM response cache key; existing committed revisions remain reusable.
- One shared policy permits at most three attempts for transient network failures, timeouts and HTTP 408/429/500/502/503/504. It honors Retry-After seconds/dates up to 30 seconds; otherwise waits 1/2 seconds plus bounded jitter. Connect timeout is 15 seconds, read inactivity timeout is 60 seconds, each attempt is bounded to 180 seconds and a complete call to 600 seconds. SDK/transport implicit retries are disabled. Privately buffered structured responses may retry after transport interruption; delivered prose is never automatically replayed.
- Cancellation interrupts requests, queued resource waits and retry waits, and closes the operation-owned OpenAI response body even if the SDK reader is idle. Durable writes and cancellation share a commit gate; cancelled jobs cannot be overwritten by late success, failure or checkpoints. Completed revisions remain reusable; a new explicit reading/start action may create new work, but restart recovery does not revive cancelled rows.
- Failed extraction/generation stops automatic prefetch for that book, including repeated chapter openings and jobs queued before the failure. On restart, blocked queued work becomes a visible failure with the predecessor error, not a permanently queued task. Explicit retries remain available; a successful explicit task reopens automatic prefetch. Failure history and committed checkpoints are retained, and unrelated books are not blocked.
- HTTP 503/504 still require a usable upstream provider. Bounded retries improve handling, not model/service availability; the application does not promise that switching SDKs cures gateway timeouts.

Offline tests and loopback compatible-provider smoke runs verify lifecycle, evidence and projection behavior. They do not measure real-model extraction accuracy or guarantee adherence to all writing instructions; real-provider quality evaluation remains separate.
