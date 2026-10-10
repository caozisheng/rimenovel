import { describe, expect, it } from "vitest";
import { parseChapterContent, parseGraphView } from "./kg";

const validView = { nodes: [{ key: "n-1", type: "character", name: "甲", aliases: [], summary: null, chapter_idx: 1, provenance: "ai_original" }], edges: [], events: [], purpose: "actual", reader_progress: 1, chapter_idx: 1, fog: null, revealed: true, ready: true, unresolved: [], revision: 1, drift_score: null, reading_line_version: 1, divergence_idx: null };

describe("knowledge graph IPC boundary", () => {
  it("parses a complete purpose-qualified view", () => {
    expect(parseGraphView(validView)).toMatchObject({ purpose: "actual", reader_progress: 1, nodes: [{ key: "n-1" }] });
  });
  it("rejects unknown purpose and invalid enum values", () => {
    expect(() => parseGraphView({ ...validView, purpose: "future" })).toThrow(/purpose/);
    expect(() => parseGraphView({ ...validView, nodes: [{ ...validView.nodes[0], provenance: "hidden" }] })).toThrow(/provenance/);
  });
  it("keeps readable generated prose when graph warning is present", () => {
    expect(parseChapterContent({ idx: 2, title: "第二章", content: "正文", source: "generated", graph_error: "back-extraction pending" })).toEqual({ idx: 2, title: "第二章", content: "正文", source: "generated", graph_error: "back-extraction pending" });
  });
  it("rejects malformed chapter content instead of accepting a cast", () => {
    expect(() => parseChapterContent({ idx: 2, title: "第二章", content: 4, source: "original" })).toThrow(/content/);
  });
});
