import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { createPinia } from "pinia";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import GraphWorkbench from "./GraphWorkbench.vue";

const backend = vi.hoisted(() => ({
  statuses: [] as Record<string, unknown>[],
  statusError: null as Error | null,
  cancel: vi.fn<(bookId: number) => Promise<void>>(),
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: backend.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, callback: (event: { payload: unknown }) => void) => {
    backend.listeners.set(name, callback);
    return () => backend.listeners.delete(name);
  },
}));
const view = { nodes: [], edges: [], events: [], purpose: "actual", reader_progress: 1, chapter_idx: 1, fog: null, revealed: false, ready: false, summary: "", open_threads: [], coverage: { mode: "partial", total_chapters: 2, selected_chapters: [1], completed_chapters: [], missing_chapters: [1, 2], legacy_chapters: [], calls_used: 0, elapsed_ms: 0, target_complete: false }, revision: null, drift_score: null, reading_line_version: 1, divergence_idx: null };
const running = { book_id: 1, job_id: 12, kind: "global_extract", chapter_idx: 1, state: "running", progress: 0.25, phase: "request", attempt: 1, retry_after_ms: null, error: null };
const mounted: VueWrapper[] = [];
function render() {
  const wrapper = mount(GraphWorkbench, { props: { bookId: 1, currentIdx: 1, chapterCount: 4 }, global: { plugins: [createPinia()], stubs: { GraphVisualization: true, ElementForm: true, Director: true, Fog: true } } });
  mounted.push(wrapper);
  return wrapper;
}
function cancelButton(wrapper: VueWrapper) {
  return wrapper.findAll("button").find(button => button.attributes("aria-label") === "取消当前书籍后台任务");
}
function progressEvent(payload: unknown = running) {
  backend.listeners.get("job:progress")?.({ payload });
}
beforeEach(() => {
  backend.statuses = [{ ...running }];
  backend.statusError = null;
  backend.listeners.clear();
  backend.cancel.mockReset();
  backend.invoke.mockReset();
  backend.cancel.mockImplementation(async bookId => {
    backend.statuses = backend.statuses.map(status => status.book_id === bookId ? { ...status, state: "cancelled", phase: "cancelled", retry_after_ms: null } : status);
  });
  backend.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "get_graph") return view;
    if (command === "list_graph_edits") return [];
    if (command === "get_extraction_statuses") {
      if (backend.statusError) throw backend.statusError;
      return backend.statuses;
    }
    if (command === "cancel_book_jobs") return backend.cancel(args?.bookId as number);
    throw new Error(`Unexpected command: ${command}`);
  });
});
afterEach(() => { for (const wrapper of mounted.splice(0)) wrapper.unmount(); });

describe("durable workbench runtime controls", () => {
  it("uses persisted progress and terminal state rather than a stale event payload", async () => {
    const wrapper = render();
    await flushPromises();
    const panel = wrapper.get('section[aria-label="后台图谱任务"]');
    expect(cancelButton(wrapper)).toBeDefined();
    backend.statuses = [{ ...running, progress: 0.5 }];
    progressEvent();
    await flushPromises();
    expect(panel.get("progress").attributes("value")).toBe("50");
    backend.statuses = [{ ...running, state: "failed", phase: "failed", error: "HTTP 503: upstream saturated" }];
    progressEvent();
    await flushPromises();
    expect(panel.findAll('[role="alert"]').some(alert => alert.text().includes("HTTP 503: upstream saturated"))).toBe(true);
    expect(cancelButton(wrapper)).toBeUndefined();
    backend.statuses = [{ ...running, state: "done", phase: "done", progress: 1 }];
    progressEvent();
    await flushPromises();
    expect(panel.get("progress").attributes("value")).toBe("100");
    expect(cancelButton(wrapper)).toBeUndefined();
  });
  it.each(["queued", "running", "paused"])("prevents duplicate cancellation of an active %s job until its terminal state is observed", async state => {
    backend.statuses = [{ ...running, state, phase: state === "paused" ? "paused" : running.phase }];
    let finishCancel!: () => void;
    backend.cancel.mockImplementation(bookId => new Promise<void>(resolve => {
      finishCancel = () => {
        backend.statuses = [{ ...running, book_id: bookId, state: "cancelled", phase: "cancelled" }];
        resolve();
      };
    }));
    const wrapper = render();
    await flushPromises();
    await cancelButton(wrapper)!.trigger("click");
    expect(cancelButton(wrapper)!.attributes("disabled")).toBeDefined();
    await cancelButton(wrapper)!.trigger("click");
    expect(backend.cancel).toHaveBeenCalledTimes(1);
    finishCancel();
    await flushPromises();
    expect(cancelButton(wrapper)).toBeUndefined();
  });
  it("keeps cancellation failures visible after successful background refreshes", async () => {
    backend.cancel.mockRejectedValue(new Error("cancel denied"));
    const wrapper = render();
    await flushPromises();
    await cancelButton(wrapper)!.trigger("click");
    await flushPromises();
    expect(wrapper.findAll('[role="alert"]').some(alert => alert.text().includes("取消失败：Error: cancel denied"))).toBe(true);
    progressEvent();
    await flushPromises();
    expect(wrapper.text()).toContain("取消失败：Error: cancel denied");
    expect(cancelButton(wrapper)!.attributes("disabled")).toBeUndefined();
  });
  it("reports a failed post-cancel refresh without claiming cancellation was observed", async () => {
    backend.cancel.mockImplementation(async () => {
      backend.statuses = [{ ...running, state: "cancelled", phase: "cancelled" }];
      backend.statusError = new Error("status unavailable");
    });
    const wrapper = render();
    await flushPromises();
    await cancelButton(wrapper)!.trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("后台状态获取失败：Error: status unavailable");
    expect(wrapper.text()).not.toContain("已取消");
    expect(cancelButton(wrapper)!.attributes("disabled")).toBeUndefined();
  });
  it("does not cancel a job on close or unmount", async () => {
    const wrapper = render();
    await flushPromises();
    await wrapper.findAll("button").find(button => button.text() === "关闭")!.trigger("click");
    expect(wrapper.emitted("close")).toHaveLength(1);
    wrapper.unmount();
    expect(backend.cancel).not.toHaveBeenCalled();
    expect(backend.listeners.size).toBe(0);
  });
  it("counts completed plan targets separately from extra reading coverage", async () => {
    const supplemented = { ...view, ready: true, coverage: { ...view.coverage, mode: "sampled", total_chapters: 5, selected_chapters: [1, 3], completed_chapters: [1, 2, 3], missing_chapters: [4, 5], calls_used: 2, target_complete: true } };
    backend.invoke.mockImplementation(async command => {
      if (command === "get_graph") return supplemented;
      if (command === "list_graph_edits") return [];
      if (command === "get_extraction_statuses") return [];
      throw new Error(`Unexpected command: ${command}`);
    });
    const wrapper = render();
    await flushPromises();
    expect(wrapper.get(".target-completed").text()).toBe("2");
  });
});
