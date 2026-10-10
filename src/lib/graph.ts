// Public presentation records, independent of extraction envelopes and IPC commands.
// Keys are opaque server-projected view IDs, never internal identity slugs.
// Names, aliases and references must already obey the server's reveal permissions.
export type GraphPurpose = "actual" | "reference" | "spec";
export const purposeLabels: Record<GraphPurpose, string> = { actual: "当前事实", reference: "原作参考", spec: "未来规格" };
export type Provenance = "ai_original" | "ai_adapted" | "ai_generated" | "user";
export type EntityType = "character" | "location" | "item" | "faction" | "concept";
export type ElementKind = "node" | "edge" | "event";
export interface GraphNode { key: string; type: EntityType; name: string; aliases: string[]; summary: string | null; chapter_idx: number; provenance: Provenance }
export interface GraphEdge { key: string; src: string; dst: string; rel_type: string; description: string | null; status: "active" | "broken" | "former" | "unknown"; since_chapter: number; chapter_idx: number; provenance: Provenance }
export interface GraphEvent { key: string; title: string; chapter_idx: number; order: number; participants: string[]; location: string | null; summary: string; outcome: string; provenance: Provenance }
export interface GraphData { nodes: GraphNode[]; edges: GraphEdge[]; events: GraphEvent[] }
export type GraphElement = GraphNode | GraphEdge | GraphEvent;
export const entityTypes: { value: EntityType; label: string }[] = [
  { value: "character", label: "人物" }, { value: "location", label: "地点" },
  { value: "item", label: "物品" }, { value: "faction", label: "势力" }, { value: "concept", label: "概念" },
];
export const provenanceLabels: Record<Provenance, string> = { ai_original: "原文提取", ai_adapted: "AI 调和规格", ai_generated: "生成正文回抽", user: "用户意图" };
export const provenanceTokens: Record<Provenance, string> = { ai_original: "--color-text-secondary", ai_adapted: "--color-success", ai_generated: "--color-accent", user: "--color-warning" };
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
export interface GraphView extends GraphData {
  purpose: GraphPurpose;
  reader_progress: number;
  chapter_idx: number | null;
  fog: FogCounts | null;
  revealed: boolean;
  ready: boolean;
  unresolved: string[];
  revision: number | null;
  drift_score: number | null;
  reading_line_version: number;
  divergence_idx: number | null;
}
export interface GraphQuery { book_id: number; scope: "global" | "chapter"; purpose: GraphPurpose; chapter_idx?: number | null }
export interface EditInput { book_id: number; scope: "global" | "chapter"; chapter_idx?: number | null; kind: "node" | "edge" | "event" | "directive" | "drift_pref"; action: "upsert" | "delete"; key?: string | null; data: Record<string, unknown> }
export interface EditRecord { id: number; target_type: string; target_id: string; action: string; after_json: string | null; effective_from: number; active: boolean }
export interface EvidenceView { key: string; chapter_idx: number; start_byte: number; end_byte: number; text: string; source: string }
export interface GraphJobStatus { book_id: number; job_id?: number | null; kind?: string | null; chapter_idx?: number | null; progress: number; state: string; error?: string | null; phase?: string | null; attempt?: number | null; retry_after_ms?: number | null }
