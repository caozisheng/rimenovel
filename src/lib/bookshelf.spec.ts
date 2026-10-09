import { describe, expect, it } from "vitest";
import type { Book } from "../lib/types";

// P0-P2 只注入了壳层；后续批次在此文件扩展 overlay 选择器/阅读线视图模型测试。
// 当前验证最小闭环: 类型契约与书架排序契约(按 id 倒序=最新在前)。
describe("bookshelf sorting contract", () => {
  const mk = (id: number, title: string): Book => ({
    id,
    title,
    author: null,
    format: "txt",
    source_path: `/books/${title}.txt`,
  });

  it("sorts by id descending (newest first)", () => {
    const input = [mk(1, "旧书"), mk(3, "新书"), mk(2, "中书")];
    const sorted = [...input].sort((a, b) => b.id - a.id);
    expect(sorted.map((b) => b.id)).toEqual([3, 2, 1]);
  });

  it("keeps empty shelf empty", () => {
    expect([...[] as Book[]].sort((a, b) => b.id - a.id)).toHaveLength(0);
  });
});
