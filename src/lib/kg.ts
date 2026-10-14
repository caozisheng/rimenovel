import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ChapterContent, ReadingMode } from "./types";
import type { EditInput, EditRecord, FogCounts, GraphCoverage, GraphEdge, GraphEvent, GraphJobStatus, GraphNode, GraphPurpose, GraphQuery, GraphView } from "./graph";

export type GraphEventName = "graph:changed" | "reading_line:changed" | "job:progress" | "chapter:delta";
export interface ChapterDelta { book_id: number; chapter_idx: number; delta: string }
export interface ReadingLineChanged { book_id: number; effective_from: number }
export interface GraphChanged { book_id: number; chapter_idx: number }

function record(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error(`${label} must be an object`);
  return Object.fromEntries(Object.entries(value));
}
function stringValue(value: unknown, label: string): string { if (typeof value !== "string") throw new Error(`${label} must be a string`); return value }
function numberValue(value: unknown, label: string): number { if (typeof value !== "number" || !Number.isFinite(value)) throw new Error(`${label} must be a finite number`); return value }
function boolValue(value: unknown, label: string): boolean { if (typeof value !== "boolean") throw new Error(`${label} must be boolean`); return value }
function nullableNumber(value: unknown, label: string): number | null { return value === null || value === undefined ? null : numberValue(value, label) }
function nullableString(value: unknown, label: string): string | null { return value === null || value === undefined ? null : stringValue(value, label) }
function stringArray(value: unknown, label: string): string[] { if (!Array.isArray(value) || !value.every((item) => typeof item === "string")) throw new Error(`${label} must be a string array`); return [...value] }
function purpose(value: unknown): GraphPurpose { const result = stringValue(value, "purpose"); if (result !== "actual" && result !== "reference" && result !== "spec") throw new Error(`invalid graph purpose: ${result}`); return result }
function graphType(value: unknown): GraphNode["type"] {
  const result = stringValue(value, "node.type");
  if (result !== "character" && result !== "location" && result !== "item" && result !== "faction" && result !== "concept") throw new Error(`invalid node.type: ${result}`);
  return result;
}
function provenance(value: unknown): GraphNode["provenance"] {
  const result = stringValue(value, "provenance");
  if (result !== "ai_original" && result !== "ai_adapted" && result !== "ai_generated" && result !== "user" && result !== "legacy") throw new Error(`invalid provenance: ${result}`);
  return result;
}
function lifeStatus(value: unknown): GraphNode["life_status"] { const result = nullableString(value, "node.life_status"); if (result !== null && result !== "alive" && result !== "dead") throw new Error(`invalid node.life_status: ${result}`); return result; }
function graphNode(value: unknown): GraphNode {
  const v = record(value, "node");
  return { key: stringValue(v.key, "node.key"), type: graphType(v.type), name: stringValue(v.name, "node.name"), aliases: stringArray(v.aliases, "node.aliases"), summary: nullableString(v.summary, "node.summary"), life_status: lifeStatus(v.life_status), location: nullableString(v.location, "node.location"), owner: nullableString(v.owner, "node.owner"), chapter_idx: numberValue(v.chapter_idx, "node.chapter_idx"), provenance: provenance(v.provenance) };
}
function graphEdge(value: unknown): GraphEdge {
  const v = record(value, "edge");
  return { key: stringValue(v.key, "edge.key"), src: stringValue(v.src, "edge.src"), dst: stringValue(v.dst, "edge.dst"), rel_type: stringValue(v.rel_type, "edge.rel_type"), description: nullableString(v.description, "edge.description"), chapter_idx: numberValue(v.chapter_idx, "edge.chapter_idx"), provenance: provenance(v.provenance) };
}
function graphEvent(value: unknown): GraphEvent {
  const v = record(value, "event");
  return { key: stringValue(v.key, "event.key"), title: stringValue(v.title, "event.title"), chapter_idx: numberValue(v.chapter_idx, "event.chapter_idx"), order: numberValue(v.order, "event.order"), participants: stringArray(v.participants, "event.participants"), location: nullableString(v.location, "event.location"), time: nullableString(v.time, "event.time"), summary: stringValue(v.summary, "event.summary"), provenance: provenance(v.provenance) };
}
function fog(value: unknown): FogCounts | null {
  if (value === null || value === undefined) return null;
  const v = record(value, "fog");
  return { entities: numberValue(v.entities, "fog.entities"), relations: numberValue(v.relations, "fog.relations"), events: numberValue(v.events, "fog.events") };
}
function coverage(value: unknown): GraphCoverage {
  const v = record(value, "coverage");
  const mode = stringValue(v.mode, "coverage.mode");
  if (mode !== "full" && mode !== "sampled" && mode !== "partial") throw new Error(`invalid coverage.mode: ${mode}`);
  const chapterArray = (input: unknown, label: string): number[] => { if (!Array.isArray(input)) throw new Error(`${label} must be a number array`); return input.map(item => numberValue(item, label)); };
  return { mode, total_chapters: numberValue(v.total_chapters, "coverage.total_chapters"), selected_chapters: chapterArray(v.selected_chapters, "coverage.selected_chapters"), completed_chapters: chapterArray(v.completed_chapters, "coverage.completed_chapters"), missing_chapters: chapterArray(v.missing_chapters, "coverage.missing_chapters"), legacy_chapters: chapterArray(v.legacy_chapters, "coverage.legacy_chapters"), calls_used: numberValue(v.calls_used, "coverage.calls_used"), elapsed_ms: numberValue(v.elapsed_ms, "coverage.elapsed_ms"), target_complete: boolValue(v.target_complete, "coverage.target_complete") };
}
export function parseGraphView(value: unknown): GraphView {
  const v = record(value, "graph view");
  if (!Array.isArray(v.nodes) || !Array.isArray(v.edges) || !Array.isArray(v.events)) throw new Error("graph view arrays are required");
  return { nodes: v.nodes.map(graphNode), edges: v.edges.map(graphEdge), events: v.events.map(graphEvent), purpose: purpose(v.purpose), reader_progress: numberValue(v.reader_progress, "reader_progress"), chapter_idx: nullableNumber(v.chapter_idx, "chapter_idx"), fog: fog(v.fog), revealed: boolValue(v.revealed, "revealed"), ready: boolValue(v.ready, "ready"), summary: stringValue(v.summary, "summary"), open_threads: stringArray(v.open_threads, "open_threads"), coverage: coverage(v.coverage), revision: nullableNumber(v.revision, "revision"), drift_score: nullableNumber(v.drift_score, "drift_score"), reading_line_version: numberValue(v.reading_line_version, "reading_line_version"), divergence_idx: nullableNumber(v.divergence_idx, "divergence_idx") };
}
function parseNumber(value: unknown, label: string): number { return numberValue(value, label) }
export function parseChapterContent(value: unknown): ChapterContent {
  const v = record(value, "chapter content");
  const source = stringValue(v.source, "chapter.source");
  const mode = v.reading_mode === "rewrite" ? "rewrite" : "original";
  const status = v.rewrite_status === "inherited" || v.rewrite_status === "generating" || v.rewrite_status === "unavailable" ? v.rewrite_status : "ready";
  return { idx: numberValue(v.idx, "chapter.idx"), title: stringValue(v.title, "chapter.title"), content: stringValue(v.content, "chapter.content"), source, reading_mode: mode, rewrite_status: status, graph_error: nullableString(v.graph_error, "chapter.graph_error") };
}
export function parseEditRecord(value: unknown): EditRecord {
  const v = record(value, "edit record");
  return { id: parseNumber(v.id, "edit.id"), target_type: stringValue(v.target_type, "edit.target_type"), target_id: stringValue(v.target_id, "edit.target_id"), action: stringValue(v.action, "edit.action"), after_json: nullableString(v.after_json, "edit.after_json"), effective_from: parseNumber(v.effective_from, "edit.effective_from"), active: boolValue(v.active, "edit.active") };
}
export function parseJobStatus(value: unknown): GraphJobStatus {
  const v = record(value, "job status");
  const quick_read_source = v.quick_read_source == null ? null : stringValue(v.quick_read_source, "job.quick_read_source");
  if (quick_read_source !== null && quick_read_source !== "local_full_text" && quick_read_source !== "web_outline") throw new Error("invalid job.quick_read_source");
  const reference_sources = v.reference_sources == null ? [] : (() => {
    if (!Array.isArray(v.reference_sources)) throw new Error("invalid job.reference_sources");
    return v.reference_sources.map(source => { const item = record(source, "reference source"); const url = stringValue(item.url, "reference.url"); const parsed = new URL(url); if (parsed.protocol !== "https:" && parsed.protocol !== "http:") throw new Error("invalid reference.url"); return { url, title: stringValue(item.title, "reference.title") }; });
  })();
  const web_rejection=nullableString(v.web_rejection,"job.web_rejection");
  const local_failure_kind=nullableString(v.local_failure_kind,"job.local_failure_kind");
  const rejectionReasons=["invalid_lookup","search_unavailable","rate_limited","edition_text_unavailable","edition_unverified","evidence_insufficient","ambiguous_sources","context_insufficient","compaction_unavailable","invalid_outline"];
  const legacyRejectionCodes=["invalid_lookup","search_unavailable","edition_text_unavailable","edition_unverified","evidence_insufficient","context_insufficient","compaction_unavailable","invalid_outline"];
  const prefixed=/^(ddg|tavily)_(.+)$/.exec(web_rejection ?? "");
  if (web_rejection && !legacyRejectionCodes.includes(web_rejection) && !(prefixed && rejectionReasons.includes(prefixed[2]))) throw new Error("invalid job.web_rejection");
  if (local_failure_kind && !["authentication","rate_limited","upstream_unavailable","provider_configuration","provider_timeout","network_failure","output_truncated","stream_interrupted","invalid_output","source_evidence_incomplete","context_insufficient"].includes(local_failure_kind)) throw new Error("invalid job.local_failure_kind");
  return { quick_read_source, reference_sources, web_rejection, local_failure_kind, book_id: parseNumber(v.book_id, "job.book_id"), job_id: nullableNumber(v.job_id, "job.job_id"), kind: nullableString(v.kind, "job.kind"), chapter_idx: nullableNumber(v.chapter_idx, "job.chapter_idx"), progress: parseNumber(v.progress, "job.progress"), state: stringValue(v.state, "job.state"), error: nullableString(v.error, "job.error"), phase: nullableString(v.phase, "job.phase"), stage: nullableString(v.stage, "job.stage"), completed_slices: nullableNumber(v.completed_slices, "job.completed_slices"), total_slices: nullableNumber(v.total_slices, "job.total_slices"), completed_chapters: nullableNumber(v.completed_chapters, "job.completed_chapters"), total_chapters: nullableNumber(v.total_chapters, "job.total_chapters"), quick_read_ready: v.quick_read_ready == null ? null : boolValue(v.quick_read_ready, "job.quick_read_ready"), bootstrap_ready: v.bootstrap_ready == null ? null : boolValue(v.bootstrap_ready, "job.bootstrap_ready"), attempt: nullableNumber(v.attempt, "job.attempt"), retry_after_ms: nullableNumber(v.retry_after_ms, "job.retry_after_ms") };
}
async function call<T>(command: string, args: Record<string, unknown> | undefined, parser: (value: unknown) => T): Promise<T> { return parser(await invoke<unknown>(command, args)); }
export function getGraph(query: GraphQuery): Promise<GraphView> { return call("get_graph", { query }, parseGraphView); }
export function revealGraph(bookId: number, chapterIdx: number): Promise<void> { return call("reveal_graph", { bookId, chapterIdx }, () => undefined); }
export async function applyGraphEdit(input: EditInput): Promise<number> { return call("apply_graph_edit", { input }, (value) => parseNumber(value, "edit id")); }
export function revertGraphEdit(bookId: number, editId: number): Promise<void> { return call("revert_graph_edit", { bookId, editId }, () => undefined); }
export async function listGraphEdits(bookId: number): Promise<EditRecord[]> { const value = await call("list_graph_edits", { bookId }, (raw) => raw); if (!Array.isArray(value)) throw new Error("graph edits must be an array"); return value.map(parseEditRecord); }
export async function startBookExtraction(bookId: number, providerId?: number): Promise<number> { return call("start_book_extraction", { bookId, ...(providerId === undefined ? {} : { providerId }) }, (value) => parseNumber(value, "job id")); }
export async function startQuickRead(bookId: number, providerId?: number): Promise<number> { return call("start_quick_read", { bookId, ...(providerId === undefined ? {} : { providerId }) }, (value) => parseNumber(value, "job id")); }
export function cancelBookJobs(bookId: number): Promise<void> { return call("cancel_book_jobs", { bookId }, () => undefined); }
export function extractChapterGraph(bookId: number, chapterIdx: number, providerId?: number): Promise<void> { return call("extract_chapter_graph", { bookId, chapterIdx, ...(providerId === undefined ? {} : { providerId }) }, () => undefined); }
export async function getChapter(bookId: number, idx: number, mode: ReadingMode = "original"): Promise<ChapterContent> { return parseChapterContent(await invoke<unknown>("get_chapter_cmd", { bookId, idx, mode })); }
export async function getExtractionStatuses(): Promise<GraphJobStatus[]> { const value = await call("get_extraction_statuses", undefined, (raw) => raw); if (!Array.isArray(value)) throw new Error("extraction statuses must be an array"); return value.map(parseJobStatus); }
export function onKgEvent<T>(name: GraphEventName, parser: (value: unknown) => T, callback: (value: T) => void): Promise<UnlistenFn> { return listen<unknown>(name, (event) => callback(parser(event.payload))); }
export function parseChapterDelta(value: unknown): ChapterDelta { const v = record(value, "chapter delta"); return { book_id: numberValue(v.book_id, "delta.book_id"), chapter_idx: numberValue(v.chapter_idx, "delta.chapter_idx"), delta: stringValue(v.delta, "delta.delta") }; }
export function parseGraphChanged(value: unknown): GraphChanged { const v = record(value, "graph changed"); return { book_id: numberValue(v.book_id, "changed.book_id"), chapter_idx: numberValue(v.chapter_idx, "changed.chapter_idx") }; }
export function parseReadingLineChanged(value: unknown): ReadingLineChanged { const v = record(value, "reading line changed"); return { book_id: numberValue(v.book_id, "line.book_id"), effective_from: numberValue(v.effective_from, "line.effective_from") }; }
