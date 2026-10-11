import { mount, flushPromises, type VueWrapper } from "@vue/test-utils";
import { createPinia } from "pinia";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import GraphWorkbench from "./GraphWorkbench.vue";

const ipc = vi.hoisted(() => ({
  statuses: [] as Record<string, unknown>[],
  jobCallback: null as (() => void) | null,
}));
vi.mock("../lib/kg", () => ({
  getGraph: async () => ({ nodes: [], edges: [], events: [], purpose: "actual", ready: false, reader_progress: 1, chapter_idx: 1, fog: null, revealed: false, summary: "", open_threads: [], coverage: { mode: "partial", total_chapters: 3, selected_chapters: [1], completed_chapters: [], missing_chapters: [1, 2, 3], legacy_chapters: [], calls_used: 0, elapsed_ms: 0, target_complete: false }, revision: null, drift_score: null, reading_line_version: 1, divergence_idx: null }),
  listGraphEdits: async () => [],
  getExtractionStatuses: async () => ipc.statuses,
  startBookExtraction: async () => { ipc.statuses = [{ book_id: 1, job_id: 12, kind: "global_extract", state: "queued", progress: 0, error: null }]; return 12; },
  extractChapterGraph: vi.fn(), cancelBookJobs: vi.fn(),
  onKgEvent: async (name: string, _parser: unknown, callback: (event: unknown) => void) => { if (name === "job:progress") ipc.jobCallback = () => callback({ book_id: 1 }); return () => {}; },
  parseJobStatus: (value: unknown) => value, parseGraphChanged: (value: unknown) => value,
  applyGraphEdit: vi.fn(), revealGraph: vi.fn(), revertGraphEdit: vi.fn(),
}));
const mounted: VueWrapper[] = [];
function render() {
  const wrapper = mount(GraphWorkbench, { props: { bookId: 1, currentIdx: 1, chapterCount: 3 }, global: { plugins: [createPinia()], stubs: { GraphVisualization: true, ElementForm: true, Director: true, Fog: true } } });
  mounted.push(wrapper);
  return wrapper;
}
beforeEach(() => { ipc.statuses = []; ipc.jobCallback = null; });
afterEach(() => { for (const wrapper of mounted.splice(0)) wrapper.unmount(); });

describe("workbench background extraction feedback", () => {
  it("shows a persisted chapter failure and allows recovery without reimporting", async () => {
    ipc.statuses = [{ book_id: 1, job_id: 11, kind: "chapter_extract", chapter_idx: 1, state: "failed", progress: 0, error: "HTTP 504: gateway timeout" }];
    const wrapper = render();
    await flushPromises();
    expect(wrapper.text()).toContain("HTTP 504: gateway timeout");
    expect(wrapper.text()).toContain("第 1 章");
    const retry = wrapper.findAll("button").find(button => button.text() === "重试样本抽取");
    expect(retry).toBeDefined();
    await retry!.trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("等待执行");
    expect(wrapper.text()).not.toContain("HTTP 504");
  });
  it("updates chapter-commit progress without closing the workbench", async () => {
    ipc.statuses = [{ book_id: 1, job_id: 8, kind: "global_extract", state: "running", progress: 0.25 }];
    const wrapper = render();
    await flushPromises();
    expect(wrapper.find('progress').attributes('value')).toBe("25");
    ipc.statuses = [{ book_id: 1, job_id: 8, kind: "global_extract", state: "running", progress: 0.5 }];
    ipc.jobCallback?.();
    await flushPromises();
    expect(wrapper.find('progress').attributes('value')).toBe("50");
  });
});
