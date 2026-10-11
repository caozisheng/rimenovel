import { describe, expect, it } from "vitest";
import { parseChapterContent, parseGraphView, parseJobStatus } from "./kg";

const validView = { nodes: [{ key: "n-1", type: "character", name: "甲", aliases: [], summary: null, life_status: "alive", location: null, owner: null, chapter_idx: 1, provenance: "ai_original" }], edges: [], events: [], purpose: "actual", reader_progress: 1, chapter_idx: 1, fog: null, revealed: true, ready: true, summary: "本章摘要", open_threads: [], coverage: { mode: "sampled", total_chapters: 3, selected_chapters: [1, 3], completed_chapters: [1], missing_chapters: [2, 3], legacy_chapters: [], calls_used: 1, elapsed_ms: 900, target_complete: false }, revision: 1, drift_score: null, reading_line_version: 1, divergence_idx: null };

describe("knowledge graph IPC boundary", () => {
  it("parses a complete purpose-qualified view", () => {
    expect(parseGraphView(validView)).toMatchObject({ purpose: "actual", reader_progress: 1, nodes: [{ key: "n-1" }] });
  });
  it("preserves legacy provenance across public reference records", () => {
    const parsed = parseGraphView({
      ...validView,
      purpose: "reference",
      nodes: [{ ...validView.nodes[0], provenance: "legacy" }],
      edges: [{ key: "r-1", src: "n-1", dst: "n-1", rel_type: "关联", description: null, chapter_idx: 1, provenance: "legacy" }],
      events: [{ key: "e-1", title: "旧事", chapter_idx: 1, order: 0, participants: ["n-1"], location: null, time: null, summary: "内容", provenance: "legacy" }],
    });
    expect(parsed.purpose).toBe("reference");
    expect(parsed.nodes[0].provenance).toBe("legacy");
    expect(parsed.edges[0].provenance).toBe("legacy");
    expect(parsed.events[0].provenance).toBe("legacy");
  });
  it("rejects unknown purpose and invalid enum values", () => {
    expect(() => parseGraphView({ ...validView, purpose: "future" })).toThrow(/purpose/);
    expect(() => parseGraphView({ ...validView, nodes: [{ ...validView.nodes[0], provenance: "hidden" }] })).toThrow(/provenance/);
  });
  it("keeps readable generated prose when graph warning is present", () => {
    expect(parseChapterContent({ idx: 2, title: "第二章", content: "正文", source: "generated", graph_error: "back-extraction pending" })).toEqual({ idx: 2, title: "第二章", content: "正文", source: "generated", reading_mode: "original", rewrite_status: "ready", graph_error: "back-extraction pending" });
  });
  it("rejects malformed chapter content instead of accepting a cast", () => {
    expect(() => parseChapterContent({ idx: 2, title: "第二章", content: 4, source: "original" })).toThrow(/content/);
  });
  it("accepts absent optional job observations", () => {
    const status = { book_id: 1, state: "paused", progress: 0 };
    expect(parseJobStatus(status)).toMatchObject({ phase: null, attempt: null, retry_after_ms: null });
  });
  it("rejects invalid lightweight metadata at the IPC boundary", () => {
    expect(() => parseGraphView({ ...validView, coverage: { ...validView.coverage, mode: "all" } })).toThrow(/coverage.mode/);
    expect(() => parseGraphView({ ...validView, nodes: [{ ...validView.nodes[0], life_status: "missing" }] })).toThrow(/life_status/);
  });
});
