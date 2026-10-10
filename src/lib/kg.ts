import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ChapterContent } from "./types";
import type { EditInput, EditRecord, EvidenceView, FogCounts, GraphEdge, GraphEvent, GraphJobStatus, GraphNode, GraphPurpose, GraphQuery, GraphView } from "./graph";

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
  if (result !== "ai_original" && result !== "ai_adapted" && result !== "ai_generated" && result !== "user") throw new Error(`invalid provenance: ${result}`);
  return result;
}
function edgeStatus(value: unknown): GraphEdge["status"] {
  const result = stringValue(value, "edge.status");
  if (result !== "active" && result !== "broken" && result !== "former" && result !== "unknown") throw new Error(`invalid edge.status: ${result}`);
  return result;
}
function graphNode(value: unknown): GraphNode {
  const v = record(value, "node");
  return { key: stringValue(v.key, "node.key"), type: graphType(v.type), name: stringValue(v.name, "node.name"), aliases: stringArray(v.aliases, "node.aliases"), summary: nullableString(v.summary, "node.summary"), chapter_idx: numberValue(v.chapter_idx, "node.chapter_idx"), provenance: provenance(v.provenance) };
}
function graphEdge(value: unknown): GraphEdge {
  const v = record(value, "edge");
  return { key: stringValue(v.key, "edge.key"), src: stringValue(v.src, "edge.src"), dst: stringValue(v.dst, "edge.dst"), rel_type: stringValue(v.rel_type, "edge.rel_type"), description: nullableString(v.description, "edge.description"), status: edgeStatus(v.status), since_chapter: numberValue(v.since_chapter, "edge.since_chapter"), chapter_idx: numberValue(v.chapter_idx, "edge.chapter_idx"), provenance: provenance(v.provenance) };
}
function graphEvent(value: unknown): GraphEvent {
  const v = record(value, "event");
  return { key: stringValue(v.key, "event.key"), title: stringValue(v.title, "event.title"), chapter_idx: numberValue(v.chapter_idx, "event.chapter_idx"), order: numberValue(v.order, "event.order"), participants: stringArray(v.participants, "event.participants"), location: nullableString(v.location, "event.location"), summary: stringValue(v.summary, "event.summary"), outcome: stringValue(v.outcome, "event.outcome"), provenance: provenance(v.provenance) };
}
function fog(value: unknown): FogCounts | null {
  if (value === null || value === undefined) return null;
  const v = record(value, "fog");
  return { entities: numberValue(v.entities, "fog.entities"), relations: numberValue(v.relations, "fog.relations"), events: numberValue(v.events, "fog.events") };
}
export function parseGraphView(value: unknown): GraphView {
  const v = record(value, "graph view");
  if (!Array.isArray(v.nodes) || !Array.isArray(v.edges) || !Array.isArray(v.events)) throw new Error("graph view arrays are required");
  return { nodes: v.nodes.map(graphNode), edges: v.edges.map(graphEdge), events: v.events.map(graphEvent), purpose: purpose(v.purpose), reader_progress: numberValue(v.reader_progress, "reader_progress"), chapter_idx: nullableNumber(v.chapter_idx, "chapter_idx"), fog: fog(v.fog), revealed: boolValue(v.revealed, "revealed"), ready: boolValue(v.ready, "ready"), unresolved: stringArray(v.unresolved, "unresolved"), revision: nullableNumber(v.revision, "revision"), drift_score: nullableNumber(v.drift_score, "drift_score"), reading_line_version: numberValue(v.reading_line_version, "reading_line_version"), divergence_idx: nullableNumber(v.divergence_idx, "divergence_idx") };
}
function parseNumber(value: unknown, label: string): number { return numberValue(value, label) }
export function parseChapterContent(value: unknown): ChapterContent {
  const v = record(value, "chapter content");
  return { idx: numberValue(v.idx, "chapter.idx"), title: stringValue(v.title, "chapter.title"), content: stringValue(v.content, "chapter.content"), source: stringValue(v.source, "chapter.source"), graph_error: nullableString(v.graph_error, "chapter.graph_error") };
}
export function parseEditRecord(value: unknown): EditRecord {
  const v = record(value, "edit record");
  return { id: parseNumber(v.id, "edit.id"), target_type: stringValue(v.target_type, "edit.target_type"), target_id: stringValue(v.target_id, "edit.target_id"), action: stringValue(v.action, "edit.action"), after_json: nullableString(v.after_json, "edit.after_json"), effective_from: parseNumber(v.effective_from, "edit.effective_from"), active: boolValue(v.active, "edit.active") };
}
export function parseEvidence(value: unknown): EvidenceView {
  const v = record(value, "evidence");
  return { key: stringValue(v.key, "evidence.key"), chapter_idx: parseNumber(v.chapter_idx, "evidence.chapter_idx"), start_byte: parseNumber(v.start_byte, "evidence.start_byte"), end_byte: parseNumber(v.end_byte, "evidence.end_byte"), text: stringValue(v.text, "evidence.text"), source: stringValue(v.source, "evidence.source") };
}
export function parseJobStatus(value: unknown): GraphJobStatus {
  const v = record(value, "job status");
  return { book_id: parseNumber(v.book_id, "job.book_id"), job_id: v.job_id === null || v.job_id === undefined ? null : parseNumber(v.job_id, "job.job_id"), progress: parseNumber(v.progress, "job.progress"), state: stringValue(v.state, "job.state"), error: nullableString(v.error, "job.error") };
}
async function call<T>(command: string, args: Record<string, unknown> | undefined, parser: (value: unknown) => T): Promise<T> { return parser(await invoke<unknown>(command, args)); }
export function getGraph(query: GraphQuery): Promise<GraphView> { return call("get_graph", { query }, parseGraphView); }
export function revealGraph(bookId: number, chapterIdx: number): Promise<void> { return call("reveal_graph", { bookId, chapterIdx }, () => undefined); }
export async function getGraphEvidence(query: GraphQuery, kind: string, key: string): Promise<EvidenceView[]> { const value = await call("get_graph_evidence", { query, kind, key }, (raw) => raw); if (!Array.isArray(value)) throw new Error("graph evidence must be an array"); return value.map(parseEvidence); }
export async function applyGraphEdit(input: EditInput): Promise<number> { return call("apply_graph_edit", { input }, (value) => parseNumber(value, "edit id")); }
export function revertGraphEdit(bookId: number, editId: number): Promise<void> { return call("revert_graph_edit", { bookId, editId }, () => undefined); }
export async function listGraphEdits(bookId: number): Promise<EditRecord[]> { const value = await call("list_graph_edits", { bookId }, (raw) => raw); if (!Array.isArray(value)) throw new Error("graph edits must be an array"); return value.map(parseEditRecord); }
export async function startBookExtraction(bookId: number, providerId?: number): Promise<number> { return call("start_book_extraction", { bookId, ...(providerId === undefined ? {} : { providerId }) }, (value) => parseNumber(value, "job id")); }
export function extractChapterGraph(bookId: number, chapterIdx: number, providerId?: number): Promise<void> { return call("extract_chapter_graph", { bookId, chapterIdx, ...(providerId === undefined ? {} : { providerId }) }, () => undefined); }
export async function getChapter(bookId: number, idx: number): Promise<ChapterContent> { return parseChapterContent(await invoke<unknown>("get_chapter_cmd", { bookId, idx })); }
export async function getExtractionStatuses(): Promise<GraphJobStatus[]> { const value = await call("get_extraction_statuses", undefined, (raw) => raw); if (!Array.isArray(value)) throw new Error("extraction statuses must be an array"); return value.map(parseJobStatus); }
export function onKgEvent<T>(name: GraphEventName, parser: (value: unknown) => T, callback: (value: T) => void): Promise<UnlistenFn> { return listen<unknown>(name, (event) => callback(parser(event.payload))); }
export function parseChapterDelta(value: unknown): ChapterDelta { const v = record(value, "chapter delta"); return { book_id: numberValue(v.book_id, "delta.book_id"), chapter_idx: numberValue(v.chapter_idx, "delta.chapter_idx"), delta: stringValue(v.delta, "delta.delta") }; }
export function parseGraphChanged(value: unknown): GraphChanged { const v = record(value, "graph changed"); return { book_id: numberValue(v.book_id, "changed.book_id"), chapter_idx: numberValue(v.chapter_idx, "changed.chapter_idx") }; }
export function parseReadingLineChanged(value: unknown): ReadingLineChanged { const v = record(value, "reading line changed"); return { book_id: numberValue(v.book_id, "line.book_id"), effective_from: numberValue(v.effective_from, "line.effective_from") }; }
