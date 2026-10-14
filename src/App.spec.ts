import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App.vue";

const backend = vi.hoisted(() => ({ invoke: vi.fn(), pick: vi.fn(), books: [] as Record<string, unknown>[], statuses: [] as Record<string, unknown>[] }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: backend.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: backend.pick }));
vi.mock("./lib/theme", () => ({ applyTheme: vi.fn(), currentTheme: () => "light" }));
const mounted: VueWrapper[] = [];
const book = { id: 1, title: "测试小说", author: null, format: "txt", source_path: "book.txt" };
function render() {
  const wrapper = mount(App, { global: { stubs: { ProviderPanel: true, ReaderView: { props: ["bookId"], template: '<section aria-label="原文阅读器">原文可读</section>' } } } });
  mounted.push(wrapper); return wrapper;
}
beforeEach(() => {
  backend.books = []; backend.statuses = []; backend.invoke.mockReset(); backend.pick.mockResolvedValue("book.txt");
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }));
  backend.invoke.mockImplementation(async (command: string) => {
    if (command === "list_books_cmd") return backend.books;
    if (command === "get_extraction_statuses") return backend.statuses;
    if (command === "import_book") {
      backend.books = [book];
      backend.statuses = [{ book_id: 1, kind: "quick_read", state: "queued", stage: "queued", progress: 0, quick_read_ready: false, bootstrap_ready: false }];
      return 1;
    }
    if (command === "start_quick_read") return 7;
    throw new Error(`Unexpected command: ${command}`);
  });
});
afterEach(() => { for (const wrapper of mounted.splice(0)) wrapper.unmount(); vi.unstubAllGlobals(); });

describe("bookshelf Quick-read preparation", () => {
  it("finishes local import while the full-book job remains queued and lets the book open", async () => {
    const wrapper = render(); await flushPromises();
    await wrapper.findAll("button").find(button => button.text() === "导入小说")!.trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("全书速读：等待执行");
    expect(wrapper.text()).toContain("原文前两章 Wiki：等待初始化");
    expect(wrapper.text()).not.toContain("导入中");
    await wrapper.get(".book-title").trigger("click");
    expect(wrapper.get('[aria-label="原文阅读器"]').text()).toContain("原文可读");
  });
  it("retries Quick-read without calling the historical sample extractor", async () => {
    backend.books = [book];
    backend.statuses = [{ book_id: 1, kind: "quick_read", state: "failed", stage: "quick_read", progress: 0.5, completed_slices: 2, total_slices: 4, quick_read_ready: false, bootstrap_ready: false, error: "HTTP 504" }];
    const wrapper = render(); await flushPromises();
    expect(wrapper.text()).toContain("分片 2 / 4");
    await wrapper.get('[data-action="retry-quick-read"]').trigger("click"); await flushPromises();
    expect(backend.invoke).toHaveBeenCalledWith("start_quick_read", { bookId: 1 });
    expect(backend.invoke.mock.calls.some(([command]) => command === "start_book_extraction")).toBe(false);
  });
  it("shows the web rejection separately from a failed full-read without private details", async () => {
    backend.books=[book];
    backend.statuses=[{book_id:1,kind:"quick_read",state:"failed",stage:"quick_read",progress:0,quick_read_ready:false,bootstrap_ready:false,web_rejection:"evidence_insufficient",local_failure_kind:"rate_limited",error:"Provider 限流",hidden_source:"未读人物秘密"}];
    const wrapper=render();await flushPromises();
    expect(wrapper.text()).toContain("联网参考：章节证据不足");
    expect(wrapper.text()).toContain("全文速读：Provider 限流");
    expect(wrapper.text()).not.toContain("未读人物秘密");
  });
  it("does not present completed legacy sampling as full-book readiness", async () => {
    backend.books = [book]; backend.statuses = [{ book_id: 1, kind: "global_extract", state: "done", progress: 1 }];
    const wrapper = render(); await flushPromises();
    expect(wrapper.text()).toContain("历史图谱抽取任务完成（不代表全书速读）");
    expect(wrapper.text()).not.toContain("整书图谱就绪");
    expect(wrapper.text()).not.toContain("第二章改写：前置资料就绪");
    expect(wrapper.text()).toContain("开始全书速读");
  });
});
