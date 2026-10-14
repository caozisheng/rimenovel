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
  it("preserves full-read checkpoints and bootstrap progress without reference payloads", () => {
    expect(parseJobStatus({ book_id: 1, kind: "quick_read", state: "running", progress: 0.5, stage: "slices", completed_slices: 3, total_slices: 6, completed_chapters: 7, total_chapters: 14, reference: { name: "未读人物秘密" } })).toMatchObject({ stage: "slices", completed_slices: 3, total_slices: 6, completed_chapters: 7, total_chapters: 14 });
    expect(parseJobStatus({ book_id: 1, state: "queued", progress: 0 })).toMatchObject({ stage: null, completed_slices: null, total_slices: null });
    expect(parseJobStatus({ book_id: 1, state: "running", progress: 0, reference: { name: "未读人物秘密" } })).not.toHaveProperty("reference");
  });
  it("rejects malformed progress counts", () => {
    expect(() => parseJobStatus({ book_id: 1, state: "running", progress: 0.5, completed_slices: "3" })).toThrow(/completed_slices/);
    expect(() => parseJobStatus({ book_id: 1, state: "done", progress: 1, quick_read_ready: "true" })).toThrow(/quick_read_ready/);
    expect(() => parseJobStatus({ book_id: 1, state: "done", progress: 1, bootstrap_ready: 1 })).toThrow(/bootstrap_ready/);
  });
  it("retains authoritative readiness separately from job completion", () => {
    expect(parseJobStatus({ book_id: 1, kind: "generate", state: "done", progress: 1, quick_read_ready: true, bootstrap_ready: true })).toMatchObject({ quick_read_ready: true, bootstrap_ready: true });
    expect(parseJobStatus({ book_id: 1, kind: "bootstrap_wiki", state: "done", progress: 1 })).toMatchObject({ quick_read_ready: null, bootstrap_ready: null });
  });
  it("accepts source provenance but rejects script URLs and unknown source types", () => {
    const status = { book_id: 1, state: "done", progress: 1, quick_read_source: "web_outline", reference_sources: [{ url: "https://www.qidian.com/book/1/", title: "候选来源", content: "不得公开的网页正文" }] };
    expect(parseJobStatus(status)).toMatchObject({ quick_read_source: "web_outline", reference_sources: [{ url: "https://www.qidian.com/book/1/", title: "候选来源" }] });
    expect(parseJobStatus(status).reference_sources?.[0]).not.toHaveProperty("content");
    expect(() => parseJobStatus({ ...status, quick_read_source: "sampled_web" })).toThrow(/quick_read_source/);
    expect(() => parseJobStatus({ ...status, reference_sources: [{ url: "javascript:alert(1)", title: "危险" }] })).toThrow(/reference.url/);
  });
  it("only parses known diagnostic codes, never external free text", () => {
    const status={book_id:1,kind:"quick_read",state:"failed",progress:0,web_rejection:"evidence_insufficient",local_failure_kind:"rate_limited",private_candidate:"未读人物秘密"};
    expect(parseJobStatus(status)).toMatchObject({web_rejection:"evidence_insufficient",local_failure_kind:"rate_limited"});
    expect(parseJobStatus(status)).not.toHaveProperty("private_candidate");
    expect(() => parseJobStatus({...status,web_rejection:"未读人物秘密"})).toThrow(/web_rejection/);
    expect(() => parseJobStatus({...status,local_failure_kind:"未读人物秘密"})).toThrow(/local_failure_kind/);
  });
  it("accepts engine-prefixed web rejection codes, still rejecting free text", () => {
    const status={book_id:1,kind:"quick_read",state:"failed",progress:0};
    for (const code of ["ddg_rate_limited","ddg_search_unavailable","ddg_edition_text_unavailable","ddg_evidence_insufficient","ddg_edition_unverified","ddg_ambiguous_sources","ddg_invalid_outline","tavily_search_unavailable","tavily_rate_limited","tavily_edition_unverified"]) {
      expect(parseJobStatus({...status,web_rejection:code})).toMatchObject({web_rejection:code});
    }
    expect(() => parseJobStatus({...status,web_rejection:"ddg_未读人物秘密"})).toThrow(/web_rejection/);
    expect(() => parseJobStatus({...status,web_rejection:"bing_search_unavailable"})).toThrow(/web_rejection/);
  });
  it("rejects invalid lightweight metadata at the IPC boundary", () => {
    expect(() => parseGraphView({ ...validView, coverage: { ...validView.coverage, mode: "all" } })).toThrow(/coverage.mode/);
    expect(() => parseGraphView({ ...validView, nodes: [{ ...validView.nodes[0], life_status: "missing" }] })).toThrow(/life_status/);
  });
});
