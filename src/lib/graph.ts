// Public presentation records, independent of extraction envelopes and IPC commands.
// Keys are opaque server-projected view IDs, never internal identity slugs.
// Names, aliases and references must already obey the server's reveal permissions.
export type GraphPurpose = "actual" | "reference" | "spec";
export const purposeLabels: Record<GraphPurpose, string> = { actual: "当前事实", reference: "原作参考", spec: "未来规格" };
export type Provenance = "ai_original" | "ai_adapted" | "ai_generated" | "user" | "legacy";
export type EntityType = "character" | "location" | "item" | "faction" | "concept";
export type ElementKind = "node" | "edge" | "event";
export interface GraphNode { key: string; type: EntityType; name: string; aliases: string[]; summary: string | null; life_status: "alive" | "dead" | null; location: string | null; owner: string | null; chapter_idx: number; provenance: Provenance }
export interface GraphEdge { key: string; src: string; dst: string; rel_type: string; description: string | null; chapter_idx: number; provenance: Provenance }
export interface GraphEvent { key: string; title: string; chapter_idx: number; order: number; participants: string[]; location: string | null; time: string | null; summary: string; provenance: Provenance }
export interface GraphData { nodes: GraphNode[]; edges: GraphEdge[]; events: GraphEvent[] }
export type GraphElement = GraphNode | GraphEdge | GraphEvent;
export const entityTypes: { value: EntityType; label: string }[] = [
  { value: "character", label: "人物" }, { value: "location", label: "地点" },
  { value: "item", label: "物品" }, { value: "faction", label: "势力" }, { value: "concept", label: "概念" },
];
export const provenanceLabels: Record<Provenance, string> = { ai_original: "原文提取", ai_adapted: "AI 调和规格", ai_generated: "生成正文回抽", user: "用户意图", legacy: "旧版迁移" };
export const provenanceTokens: Record<Provenance, string> = { ai_original: "--color-text-secondary", ai_adapted: "--color-success", ai_generated: "--color-accent", user: "--color-warning", legacy: "--color-text-muted" };
export function sortEvents(events: GraphEvent[]): GraphEvent[] {
  return [...events].sort((a, b) => a.chapter_idx - b.chapter_idx || a.order - b.order || a.key.localeCompare(b.key));
}
export function eventNodeKeys(event: GraphEvent): string[] {
  return [...new Set([...event.participants, ...(event.location ? [event.location] : [])])];
}
export function filterGraph(graph: Pick<GraphData, "nodes" | "edges">, types: EntityType[]): { nodes: GraphNode[]; edges: GraphEdge[] } {
  const nodes = graph.nodes.filter((node) => types.includes(node.type));
  const keys = new Set(nodes.map((node) => node.key));
  return { nodes, edges: graph.edges.filter((edge) => keys.has(edge.src) && keys.has(edge.dst)) };
}
export interface FogCounts { entities: number; relations: number; events: number }
export interface GraphCoverage { mode: "full" | "sampled" | "partial"; total_chapters: number; selected_chapters: number[]; completed_chapters: number[]; missing_chapters: number[]; legacy_chapters: number[]; calls_used: number; elapsed_ms: number; target_complete: boolean }
export interface GraphView extends GraphData {
  purpose: GraphPurpose;
  reader_progress: number;
  chapter_idx: number | null;
  fog: FogCounts | null;
  revealed: boolean;
  ready: boolean;
  summary: string;
  open_threads: string[];
  coverage: GraphCoverage;
  revision: number | null;
  drift_score: number | null;
  reading_line_version: number;
  divergence_idx: number | null;
}
export interface GraphQuery { book_id: number; scope: "global" | "chapter"; purpose: GraphPurpose; chapter_idx?: number | null }
export interface EditInput { book_id: number; scope: "global" | "chapter"; chapter_idx?: number | null; kind: "node" | "edge" | "event" | "directive" | "drift_pref"; action: "upsert" | "delete"; key?: string | null; data: Record<string, unknown> }
export interface EditRecord { id: number; target_type: string; target_id: string; action: string; after_json: string | null; effective_from: number; active: boolean }
export interface GraphJobStatus { book_id: number; job_id?: number | null; kind?: string | null; chapter_idx?: number | null; progress: number; state: string; error?: string | null; phase?: string | null; stage?: string | null; completed_slices?: number | null; total_slices?: number | null; completed_chapters?: number | null; total_chapters?: number | null; quick_read_ready?: boolean | null; bootstrap_ready?: boolean | null; quick_read_source?: "local_full_text" | "web_outline" | null; reference_sources?: { url: string; title: string }[]; web_rejection?: string | null; local_failure_kind?: string | null; attempt?: number | null; retry_after_ms?: number | null }

const jobStateLabels: Record<string, string> = { queued: "等待执行", running: "处理中", paused: "已暂停", failed: "失败，可重试", cancelled: "已取消，可恢复", done: "已完成" };
const preparationStageLabels: Record<string, string> = { queued: "等待执行", quick_read: "全文分片速读", summary: "全书汇总", bootstrap_wiki: "原文 Wiki 初始化", not_configured: "未配置 Provider", web_search: "联网检索大纲", web_search_broad: "全网检索大纲", web_match: "首段 AI 版本比对", web_outline: "整理联网大纲", local_fallback: "回退全文速读", ddg_search: "DuckDuckGo 检索大纲", ddg_search_broad: "DuckDuckGo 全网检索", ddg_match: "DuckDuckGo 首段 AI 版本比对", ddg_outline: "整理 DuckDuckGo 大纲" };
const webRejectionLabels: Record<string,string> = { invalid_lookup:"本地章节信息不适合联网比对",search_unavailable:"搜索服务不可用",edition_text_unavailable:"未取得可比对的公开正文",edition_unverified:"无法核实同版正文",evidence_insufficient:"章节证据不足",ambiguous_sources:"多个候选来源无法裁决",context_insufficient:"大纲超出模型预算",compaction_unavailable:"大纲整理服务不可用",invalid_outline:"大纲结果未通过校验",ddg_rate_limited:"DuckDuckGo 限流",ddg_search_unavailable:"DuckDuckGo 搜索不可用",ddg_edition_text_unavailable:"DuckDuckGo 未取得可比对的公开正文",ddg_edition_unverified:"DuckDuckGo 无法核实同版正文",ddg_evidence_insufficient:"DuckDuckGo 章节证据不足",ddg_ambiguous_sources:"DuckDuckGo 多个候选来源无法裁决",ddg_context_insufficient:"DuckDuckGo 大纲超出模型预算",ddg_compaction_unavailable:"DuckDuckGo 大纲整理服务不可用",ddg_invalid_outline:"DuckDuckGo 大纲结果未通过校验",tavily_rate_limited:"Tavily 限流",tavily_search_unavailable:"Tavily 搜索不可用",tavily_edition_text_unavailable:"Tavily 未取得可比对的公开正文",tavily_edition_unverified:"Tavily 无法核实同版正文",tavily_evidence_insufficient:"Tavily 章节证据不足",tavily_ambiguous_sources:"Tavily 多个候选来源无法裁决",tavily_context_insufficient:"Tavily 大纲超出模型预算",tavily_compaction_unavailable:"Tavily 大纲整理服务不可用",tavily_invalid_outline:"Tavily 大纲结果未通过校验" };
const localFailureLabels: Record<string,string> = { authentication:"Provider 认证失败",rate_limited:"Provider 限流",upstream_unavailable:"Provider 服务不可用",provider_configuration:"Provider 配置错误",provider_timeout:"Provider 请求超时",network_failure:"网络连接失败",output_truncated:"输出截断",stream_interrupted:"响应流中断",invalid_output:"模型输出无效",source_evidence_incomplete:"原文证据不完整",context_insufficient:"上下文预算不足" };
export function safePreparationDiagnostics(status: GraphJobStatus | null | undefined): string[] {
  if (!status || status.kind !== "quick_read") return [];
  const descriptions:string[]=[];
  if (status.web_rejection && webRejectionLabels[status.web_rejection]) descriptions.push(`联网参考：${webRejectionLabels[status.web_rejection]}`);
  if (status.local_failure_kind && localFailureLabels[status.local_failure_kind]) descriptions.push(`全文速读：${localFailureLabels[status.local_failure_kind]}`);
  return descriptions;
}
export function rewritePreparationReady(status: GraphJobStatus | null | undefined): boolean {
  return status?.quick_read_ready === true && status.bootstrap_ready === true;
}
export function preparationPhases(status: GraphJobStatus | null | undefined, chapterCount?: number): string[] {
  const quickRead = status?.quick_read_ready === true ? "已完成" : status?.kind === "quick_read" ? status.state === "done" ? "需确认 / 重新准备" : jobStateLabels[status.state] ?? "准备中" : "尚未完成";
  const bootstrap = status?.bootstrap_ready === true ? "已完成" : status?.kind === "bootstrap_wiki" ? status.state === "done" ? "需确认 / 重新准备" : jobStateLabels[status.state] ?? "准备中" : "等待初始化";
  return [
    status?.quick_read_source === "web_outline" && status.quick_read_ready === true ? "联网大纲参考：已就绪（非全文读取）" : `全书速读：${quickRead}`,
    `${chapterCount === 1 ? "原文第一章 Wiki" : "原文前两章 Wiki"}：${bootstrap}`,
    chapterCount === 1 ? "本书仅一章，无第二章改写" : `第二章改写：${rewritePreparationReady(status) ? "前置资料就绪" : "等待前置资料"}`,
  ];
}
export function preparationProgress(status: GraphJobStatus | null | undefined): string {
  if (!status || (status.kind !== "quick_read" && status.kind !== "bootstrap_wiki")) return "";
  const parts: string[] = [];
  const stage = (status.phase ? preparationStageLabels[status.phase] : undefined) ?? (status.stage ? preparationStageLabels[status.stage] : undefined);
  if (stage) parts.push(stage);
  if (status.completed_slices != null && status.total_slices != null) parts.push(`分片 ${status.completed_slices} / ${status.total_slices}`);
  if (status.completed_chapters != null && status.total_chapters != null) parts.push(`${status.kind === "bootstrap_wiki" ? "Wiki" : "章节"} ${status.completed_chapters} / ${status.total_chapters}`);
  parts.push(`${Math.round(status.progress * 100)}%`);
  return parts.join(" · ");
}
export function canStartQuickRead(status: GraphJobStatus | null | undefined): boolean {
  return !rewritePreparationReady(status) && (!status || (status.kind !== "quick_read" && status.kind !== "bootstrap_wiki") || !["queued", "running"].includes(status.state));
}
export function legacyJobLabel(status: GraphJobStatus): string {
  if (status.kind === "quick_read" || status.kind === "bootstrap_wiki") return "";
  if (status.state === "done") return status.kind === "global_extract" ? "历史图谱抽取任务完成（不代表全书速读）" : "后台任务完成";
  return `${jobStateLabels[status.state] ?? "后台处理中"} ${Math.round(status.progress * 100)}%`;
}
