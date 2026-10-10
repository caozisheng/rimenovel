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
const view = { nodes: [], edges: [], events: [], purpose: "actual", reader_progress: 1, chapter_idx: 1, fog: null, revealed: false, ready: false, unresolved: [], revision: null, drift_score: null, reading_line_version: 1, divergence_idx: null };
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
  it("shows request, retry wait, failure and done observations from persisted status queries", async () => {
    const wrapper = render();
    await flushPromises();
    const panel = wrapper.get('section[aria-label="后台图谱任务"]');
    expect(panel.text()).toContain("请求模型");
    expect(panel.text()).toContain("第 1 次尝试");
    expect(cancelButton(wrapper)).toBeDefined();
    backend.statuses = [{ ...running, phase: "retry_wait", attempt: 2, retry_after_ms: 1500 }];
    // Event payload is deliberately stale: the persisted query, not the callback, is authoritative.
    progressEvent();
    await flushPromises();
    expect(panel.text()).toContain("等待重试");
    expect(panel.text()).toContain("第 2 次尝试");
    expect(panel.text()).toContain("重试等待：1.5 秒");
    expect(panel.get("progress").attributes("value")).toBe("25");
    backend.statuses = [{ ...running, state: "failed", phase: "failed", attempt: 3, error: "HTTP 503: upstream saturated" }];
    progressEvent();
    await flushPromises();
    expect(panel.text()).toContain("任务失败");
    expect(panel.text()).toContain("HTTP 503: upstream saturated");
    expect(cancelButton(wrapper)).toBeUndefined();
    backend.statuses = [{ ...running, state: "done", phase: "done", progress: 1 }];
    progressEvent();
    await flushPromises();
    expect(panel.text()).toContain("任务完成");
    expect(panel.get("progress").attributes("value")).toBe("100");
    expect(cancelButton(wrapper)).toBeUndefined();
  });
  it.each(["queued", "running", "paused"])("cancels an active %s job through the exact IPC and refreshes its durable transition", async state => {
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
    if (state === "paused") expect(wrapper.get('section[aria-label="后台图谱任务"]').text()).toContain("阶段：已暂停");
    await cancelButton(wrapper)!.trigger("click");
    expect(backend.invoke).toHaveBeenCalledWith("cancel_book_jobs", { bookId: 1 });
    expect(cancelButton(wrapper)!.attributes("disabled")).toBeDefined();
    expect(wrapper.text()).toContain("取消中");
    expect(wrapper.text()).not.toContain("已取消");
    await cancelButton(wrapper)!.trigger("click");
    expect(backend.cancel).toHaveBeenCalledTimes(1);
    finishCancel();
    await flushPromises();
    expect(wrapper.text()).toContain("已取消");
    expect(cancelButton(wrapper)).toBeUndefined();
    expect(wrapper.text()).toContain("开始抽取");
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
});
