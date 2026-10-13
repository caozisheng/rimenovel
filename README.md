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
- 改写前需完成合格的全书参考与前两章原文 Wiki 初始化；参考可以是完整全文 Quick-read，或严格匹配的联网大纲。失败时已完成本地片段可恢复；局部网页/采样结果不能解锁改写，原文始终可读。
- 每章改写将 Quick-read 人物/主线参考、选中阅读线实际历史、上一章选中全文、目标原文章与来源 Wiki、有效用户干预和 NextChapterWiki 一并用于生成；原作未来只作隐藏参考，不进入已发生事实。
- 改写以完整 `{ prose, actual_chapter_wiki }` 配对提交；两者绑定同一生成文本与依赖版本，任一缺失、过期或取消均不发布半成品。下一章使用前章选中的正文与实际 Wiki。

## 导入与运行

- 导入后原文与章节目录立即可读；后台准备全书参考并初始化前两章原文 Wiki。本地全文路径保留完整 Unicode 覆盖校验；摘要上下文不足时显式失败、保留分片，不静默抽样或删减证据。
- `pnpm dev` 仅预览前端；导入、章节、Provider、Wiki 规划和正文生成需要 Tauri 后端，使用 `pnpm tauri dev`。
- Provider 分别配置 extract、merge、write 模型。真实模型质量、延迟和整书成本必须单独验收，不由离线测试推断。
- Database and the current credential vault live in Tauri's application-data directory. A first launch migrates a legacy working-directory database without overwriting existing application data. `RIMENOVEL_DATA_DIR` selects an isolated data directory for smoke runs. Credentials still use the existing local vault; this is not an OS-keyring security claim.

### 联网大纲检索

- 在 Provider 中打开“联网检索”总开关后，默认使用 DuckDuckGo（DDG），免费、无需配置 Key，检索尽力而为，不保证命中或可用性。Tavily API Key 是可选增强，可提供更好的正文质量与稳定性；未保存 Key 时仅尝试 DDG 联网层。编辑时留空保留已有密钥，关闭联网不删除已保存的 Tavily 或 LLM 凭据。密钥留在现有本机 vault，不返回 Provider 列表；当前 vault 为本地明文文件，并非系统密钥链。
- 检索按 DDG → Tavily（有 Key 时）→ 本地全文速读分层回退；DDG 限流、服务错误或没有可用完整候选时自动进入下一层，无 Tavily Key 则跳过该层。各联网层先检索起点、番茄、晋江、17K、纵横、小说阅读网（`qidian.com`、`fanqienovel.com`、`jjwxc.net`、`17k.com`、`zongheng.com`、`readnovel.com`），无可用候选再全网检索。搜索引擎只接收书名、已知作者和章节标题；本地首个实质正文与公开候选对应章节的首段仅交给已配置的 LLM Provider 判断版别，不向 DDG 或 Tavily 发送本机正文。
- 只有 AI 明确判断同版、返回准确的公开来源 URL，且书籍身份、全部章节映射及逐章证据通过校验，才采用 `web_outline`。定向检索出现多个完整候选时不通过全网重搜将歧义缩成单一来源；不同版/不确定、无可比对正文或不完整大纲不会发布联网参考，分层检索均不可用时回退全文速读，始终使用用户选定的 Provider。新修订记录 `ai_paragraph` 证明而不伪造匹配分数；历史 `text_similarity` 分数保留但不冒充新证明。
- 联网命中显示“联网大纲参考（非全文读取）”，不伪造本地全文覆盖；来源链接默认折叠，需主动展开。只保存结构化参考与来源证据，不保存抓取的整页/整章文本；它不会成为改写线已经发生的 Wiki。
- 本地 checkpoint 保留，恢复只处理缺失片段。联网来源绑定原文、模型与实际命中的搜索引擎依赖；DDG 结果不依赖 Tavily Key，Tavily 结果绑定其密钥依赖，更改相关依赖后需重新准备。真实检索命中率、模型质量、token 节省和费用尚未测量。

## LLM adapters and bounded execution

- OpenAI-compatible Chat Completions use pinned `async-openai` 0.42.2; Anthropic retains its native Messages/forced-tool adapter. Keys, provider configuration and SDK objects remain in Rust. `LlmClient` separates provider code from the KG pipeline; no general agent harness or state-graph dependency is added.
- Every structured call sends its output budget and exact schema, then validates the whole result locally. OpenAI structured output is collected privately over SDK SSE and published only after normal completion and JSON/schema validation; partial JSON is discarded on interruption. Truncation, refusal, malformed JSON and incomplete streams fail rather than publishing partial graphs/prose. A byte-prefix safeguard prevents the pinned SSE dependency's leading-BOM panic without reimplementing event parsing.
- Chapter extraction derives a typed reference schema from the supplied registry: new references use `c_` (character), `l_` (location), `i_` (item), or `f_` (faction). Entity/event locations accept only registered location references or declared new `l_` references; item owners accept character/faction references. SDK and pipeline validation reject illegal references without guessing places, clearing states, or publishing an invalid revision. The request schema participates in the LLM response cache key; existing committed revisions remain reusable.
- One shared policy permits at most three attempts for transient network failures, timeouts and HTTP 408/429/500/502/503/504. It honors Retry-After seconds/dates up to 30 seconds; otherwise waits 1/2 seconds plus bounded jitter. Connect timeout is 15 seconds, read inactivity timeout is 60 seconds, each attempt is bounded to 180 seconds and a complete call to 600 seconds. SDK/transport implicit retries are disabled. Privately buffered structured responses may retry after transport interruption; delivered prose is never automatically replayed.
- Cancellation interrupts requests, queued resource waits and retry waits, and closes the operation-owned OpenAI response body even if the SDK reader is idle. Durable writes and cancellation share a commit gate; cancelled jobs cannot be overwritten by late success, failure or checkpoints. Completed revisions remain reusable; a new explicit reading/start action may create new work, but restart recovery does not revive cancelled rows.
- Failed extraction/generation stops automatic prefetch for that book, including repeated chapter openings and jobs queued before the failure. On restart, blocked queued work becomes a visible failure with the predecessor error, not a permanently queued task. Explicit retries remain available; a successful explicit task reopens automatic prefetch. Failure history and committed checkpoints are retained, and unrelated books are not blocked.
- HTTP 503/504 still require a usable upstream provider. Bounded retries improve handling, not model/service availability; the application does not promise that switching SDKs cures gateway timeouts.

- 百万字 synthetic worker 测试验证覆盖、checkpoint、并发与一次汇总；未调用真实 Provider，不能据此声称 100 万字 60 秒或实际费用/质量达标。
Offline tests and loopback compatible-provider smoke runs verify lifecycle, evidence and projection behavior. They do not measure real-model extraction accuracy or guarantee adherence to all writing instructions; real-provider quality evaluation remains separate.
