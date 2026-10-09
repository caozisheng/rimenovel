// 与 Rust 侧 serde 结构对齐的前端类型
export interface Book {
  id: number;
  title: string;
  author: string | null;
  format: string;
  source_path: string;
}

export interface ChapterMeta {
  idx: number;
  title: string;
}

export interface ChapterContent {
  idx: number;
  title: string;
  content: string;
  source: string;
}

export interface ProviderRow {
  id: number;
  name: string;
  protocol: string;
  base_url: string;
  model_extract: string | null;
  model_write: string | null;
  model_merge: string | null;
}

export interface ExtractionStatus {
  book_id: number;
  state: string;
  progress: number;
}
