import { beforeEach, describe, expect, it } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { filterGraph, sortEvents, eventNodeKeys, preparationPhases, preparationProgress, safePreparationDiagnostics, rewritePreparationReady, canStartQuickRead, type GraphData, type GraphEvent } from "./graph";
import { useGraphSelectionStore } from "./graphSelection";

const event = (key: string, chapter_idx: number, order: number): GraphEvent => ({ key, chapter_idx, order, title: key, participants: ["person", "person"], location: "place", time: null, summary: "", provenance: "user" });

describe("graph presentation", () => {
  it("sorts timeline by chapter then order without mutating the input", () => {
    const input = [event("late", 3, 0), event("second", 1, 2), event("first", 1, 1)];
    expect(sortEvents(input).map((item) => item.key)).toEqual(["first", "second", "late"]);
    expect(input[0].key).toBe("late");
  });
  it("filters entity types and removes relations with hidden endpoints", () => {
    const graph: GraphData = {
      nodes: [
        { key: "person", type: "character", name: "人物", aliases: [], summary: null, life_status: "alive", location: "place", owner: null, chapter_idx: 1, provenance: "user" },
        { key: "place", type: "location", name: "地点", aliases: [], summary: null, life_status: null, location: null, owner: null, chapter_idx: 1, provenance: "user" },
      ],
      edges: [{ key: "edge", src: "person", dst: "place", rel_type: "位于", description: null, chapter_idx: 1, provenance: "user" }],
      events: [],
    };
    const result = filterGraph(graph, ["character"]);
    expect(result.nodes.map((node) => node.key)).toEqual(["person"]);
    expect(result.edges).toEqual([]);
    expect(graph.edges).toHaveLength(1);
    expect(filterGraph(graph, []).nodes).toEqual([]);
    expect(filterGraph(graph, []).edges).toEqual([]);
    expect(filterGraph(graph, ["character", "location"]).edges).toEqual(graph.edges);
  });
  it("deduplicates timeline highlight participants and location", () => {
    expect(eventNodeKeys(event("one", 1, 1))).toEqual(["person", "place"]);
  });
  it("omits absent locations and empty participant lists from highlights", () => {
    expect(eventNodeKeys({ ...event("empty", 1, 0), participants: [], location: null })).toEqual([]);
  });
});

describe("shared graph selection", () => {
  beforeEach(() => setActivePinia(createPinia()));
  it("highlights an event's participants and location, and clears them on node selection", () => {
    const store = useGraphSelectionStore();
    store.selectEvent(event("one", 1, 1));
    expect(store.selection).toEqual({ kind: "event", key: "one" });
    expect(store.highlightedNodeKeys).toEqual(["person", "place"]);
    store.select("node", "person");
    expect(store.highlightedNodeKeys).toEqual([]);
    expect(store.selection).toEqual({ kind: "node", key: "person" });
    store.clear();
    expect(store.selection).toBeNull();
  });
  it("replaces prior event highlights rather than accumulating unrelated nodes", () => {
    const store = useGraphSelectionStore();
    store.selectEvent(event("one", 1, 1));
    store.selectEvent({ ...event("two", 2, 0), participants: ["other"], location: null });
    expect(store.highlightedNodeKeys).toEqual(["other"]);
    store.select("edge", "relationship");
    expect(store.selection).toEqual({ kind: "edge", key: "relationship" });
    expect(store.highlightedNodeKeys).toEqual([]);
  });
});

describe("engine-layered preparation labels", () => {
  it("labels ddg stages and engine-prefixed rejections for reader diagnostics", () => {
    for (const phase of ["ddg_search","ddg_search_broad","ddg_match","ddg_outline"]) {
      expect(preparationProgress({ kind: "quick_read", phase } as never)).toContain("DuckDuckGo");
    }
    const diagnostics=safePreparationDiagnostics({book_id:1,kind:"quick_read",state:"failed",progress:0,web_rejection:"ddg_rate_limited"});
    expect(diagnostics.some(d=>d.includes("DuckDuckGo"))).toBe(true);
    const tavilyDiagnostics=safePreparationDiagnostics({book_id:1,kind:"quick_read",state:"failed",progress:0,web_rejection:"tavily_search_unavailable"});
    expect(tavilyDiagnostics.some(d=>d.includes("Tavily"))).toBe(true);
  });
});

describe("full-book preparation presentation", () => {
  it("requires both authoritative readiness fields, never a completed sampled job", () => {
    const legacy = { book_id: 1, kind: "global_extract", state: "done", progress: 1 };
    expect(rewritePreparationReady(legacy)).toBe(false);
    expect(rewritePreparationReady({ ...legacy, quick_read_ready: true, bootstrap_ready: false })).toBe(false);
    expect(rewritePreparationReady({ ...legacy, quick_read_ready: true, bootstrap_ready: true })).toBe(true);
    expect(canStartQuickRead(legacy)).toBe(true);
    expect(canStartQuickRead({ ...legacy, kind: "quick_read", state: "running" })).toBe(false);
    expect(canStartQuickRead({ ...legacy, state: "running" })).toBe(true);
  });
  it("does not expose arbitrary reference text through stage or state labels", () => {
    const status = { book_id: 1, kind: "quick_read", state: "未读人物秘密", stage: "未读人物秘密", progress: 0 };
    expect(preparationPhases(status).join(" ") + preparationProgress(status)).not.toContain("未读人物秘密");
  });
});
