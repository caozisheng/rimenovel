// 与 Rust 侧 serde 结构对齐的前端类型
export interface Book {
  id: number;
  title: string;
  author: string | null;
  format: string;
  source_path: string;
}
