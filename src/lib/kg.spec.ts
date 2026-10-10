import { describe, expect, it } from "vitest";
import { parseChapterContent, parseGraphView, parseJobStatus } from "./kg";

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
  it("preserves optional runtime observations without exposing internal job payload", () => {
    expect(parseJobStatus({ book_id: 1, job_id: 9, kind: "chapter_extract", chapter_idx: 2, state: "running", progress: 0.25, phase: "retry_wait", attempt: 2, retry_after_ms: 1500, payload_json: { provider_id: 8, checkpoint: "private" } })).toEqual({ book_id: 1, job_id: 9, kind: "chapter_extract", chapter_idx: 2, state: "running", progress: 0.25, error: null, phase: "retry_wait", attempt: 2, retry_after_ms: 1500 });
  });
  it("accepts older statuses and null runtime observations", () => {
    const status = { book_id: 1, state: "paused", progress: 0 };
    expect(parseJobStatus(status)).toMatchObject({ phase: null, attempt: null, retry_after_ms: null });
    expect(parseJobStatus({ ...status, phase: null, attempt: null, retry_after_ms: null })).toMatchObject({ phase: null, attempt: null, retry_after_ms: null });
  });
  it("rejects malformed runtime observations at the IPC boundary", () => {
    const status = { book_id: 1, state: "running", progress: 0 };
    expect(() => parseJobStatus({ ...status, phase: 3 })).toThrow(/phase/);
    expect(() => parseJobStatus({ ...status, attempt: "2" })).toThrow(/attempt/);
    expect(() => parseJobStatus({ ...status, retry_after_ms: Infinity })).toThrow(/retry_after_ms/);
  });
});
