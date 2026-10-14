import type { GraphJobStatus } from "./graph";

// 与 Rust 侧 serde 结构对齐的前端类型
export interface Book {
  id: number;
  title: string;
  author: string | null;
  format: string;
  source_path: string;
}

export type ReadingMode = "original" | "rewrite";
export type RewriteStatus = "ready" | "inherited" | "generating" | "unavailable";
export interface ChapterMeta {
  idx: number;
  title: string;
}

export interface ChapterContent {
  idx: number;
  title: string;
  content: string;
  source: string;
  reading_mode: ReadingMode;
  rewrite_status: RewriteStatus;
  graph_error?: string | null;
}
export interface ProviderRow {
  id: number;
  name: string;
  protocol: string;
  base_url: string;
  model_extract: string | null;
  model_write: string | null;
  model_merge: string | null;
  context_limit: number | null;
  output_reserve: number | null;
  tavily_enabled: boolean;
  has_tavily_key: boolean;
}
export type ExtractionStatus = GraphJobStatus;
