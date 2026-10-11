import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ReaderView from "./ReaderView.vue";

const backend = vi.hoisted(() => ({ invoke: vi.fn(), history: [] as Record<string, unknown>[], version: 7, noBatch: false, batchError: false, overridden: [] as string[], listeners: new Map<string, (event: { payload: unknown }) => void>() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: backend.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async (name: string, callback: (event: { payload: unknown }) => void) => { backend.listeners.set(name, callback); return () => backend.listeners.delete(name); } }));
const mounted: VueWrapper[] = [];
const questions = [1, 2, 3].map(index => ({ id: `q${index}`, prompt: `雾中问题 ${index}`, options: [{ id: `q${index}-a`, label: "靠近一些" }, { id: `q${index}-b`, label: "保持距离" }] }));
const wiki = { entries: [{ id: "hero", kind: "character", name: "甲", aliases: [], summary: "已公开的经历", state_items: [], related_entries: [], open_threads: [], source_chapters: [1] }], chapter_cards: [{ chapter_idx: 1, title: "第一章", headline: "已公开的故事", summary: null, entry_ids: ["hero"], ready: true }], coverage: { visited: [1], ready: [1], missing: [2] }, wiki_revision: 1, reading_line_version: 7, ready: true };
function render() { const wrapper = mount(ReaderView, { props: { bookId: 1 } }); mounted.push(wrapper); return wrapper; }
async function openWiki(wrapper: VueWrapper) { await flushPromises(); await wrapper.get('[data-action="open-wiki"]').trigger("click"); await flushPromises(); }
beforeEach(() => {
  localStorage.clear();
  backend.history = []; backend.version = 7; backend.noBatch = false; backend.batchError = false; backend.overridden = []; backend.listeners.clear(); backend.invoke.mockReset();
  vi.spyOn(window, "scrollTo").mockImplementation(() => {});
  backend.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    if (command === "get_chapter_titles") return [{ idx: 1, title: "第一章", char_count: 20 }, { idx: 2, title: "第二章", char_count: 20 }];
    if (command === "get_chapter_cmd") return { idx: args?.idx, title: args?.idx === 1 ? "第一章" : "第二章", content: `第 ${args?.idx} 章正文`, source: "original", graph_error: null };
    if (command === "get_story_map") return { ...wiki, reading_line_version: backend.version };
    if (command === "get_active_interventions") return backend.history;
    if (command === "get_question_batch") {
      if (backend.batchError) throw new Error("问题暂时不可用");
      return backend.noBatch ? null : { batch_index: args?.batchIndex, question_batch_id: `batch-${args?.batchIndex}`, questions, answered_count: 0, effective_answer_count: 0, has_more: true, can_end: true };
    }
    if (command === "save_intervention" || command === "end_questionnaire") {
      backend.history.push({ ...(args?.input as Record<string, unknown>), overridden_option_ids: backend.overridden }); backend.version++;
      return { id: backend.history.length, overridden_option_ids: backend.overridden };
    }
    throw new Error(`Unexpected command: ${command}`);
  });
});
afterEach(() => { for (const wrapper of mounted.splice(0)) wrapper.unmount(); vi.restoreAllMocks(); });

describe("reader Wiki integration", () => {
  it("opens the read-only Wiki and jumps back to a chapter", async () => {
    const wrapper = render(); await flushPromises();
    expect(backend.invoke).toHaveBeenCalledWith("get_chapter_cmd", { bookId: 1, idx: 1, mode: "original" });
    await openWiki(wrapper);
    expect(wrapper.get('section[aria-label="只读 Wiki 工作台"]').text()).toContain("已公开的经历");
    expect(wrapper.text()).not.toContain("图谱");
    expect(wrapper.findAll("fieldset")).toHaveLength(3);
    expect(backend.invoke).toHaveBeenCalledWith("get_story_map", { bookId: 1 });
    await wrapper.findAll("button").find(button => button.text() === "第 1 章")!.trigger("click"); await flushPromises();
    expect(wrapper.find('section[aria-label="只读 Wiki 工作台"]').exists()).toBe(false);
    expect(wrapper.get(".chapter-text").text()).toContain("第 1 章正文");
  });
  it.each([0, 1, 3])("submits %i choices through real IPC wrappers and refreshes the next batch", async count => {
    const wrapper = render(); await openWiki(wrapper);
    for (let index = 1; index <= count; index++) await wrapper.get(`input[value="q${index}-a"]`).setValue(true);
    await wrapper.get("textarea").setValue("继续同行");
    await wrapper.get('[data-action="submit"]').trigger("click"); await flushPromises();
    expect(backend.invoke).toHaveBeenCalledWith("save_intervention", { input: { book_id: 1, chapter_idx: 2, batch_index: 0, question_batch_id: "batch-0", selected_option_ids: Array.from({ length: count }, (_, index) => `q${index + 1}:q${index + 1}-a`), free_text: "继续同行", overridden_option_ids: [], reading_line_version: 7 } });
    expect(backend.invoke).toHaveBeenCalledWith("get_question_batch", { bookId: 1, chapterIdx: 2, batchIndex: 1 });
    expect(wrapper.get('section[aria-label="已提交答案"]').text()).toContain("继续同行");
    await wrapper.get('[data-action="end"]').trigger("click"); await flushPromises();
    expect(backend.invoke).toHaveBeenCalledWith("end_questionnaire", { input: expect.objectContaining({ book_id: 1, chapter_idx: 2, reading_line_version: 8 }) });
  });
  it.each([false, true])("keeps free-text end available without a batch (load failure: %s)", async failure => {
    backend.noBatch = true; backend.batchError = failure;
    const wrapper = render(); await openWiki(wrapper);
    expect(wrapper.get('[data-action="end"]').attributes("disabled")).toBeUndefined();
    await wrapper.get("textarea").setValue("不要分开"); await wrapper.get('[data-action="end"]').trigger("click"); await flushPromises();
    expect(backend.invoke).toHaveBeenCalledWith("end_questionnaire", { input: { book_id: 1, chapter_idx: 2, batch_index: null, question_batch_id: null, selected_option_ids: [], free_text: "不要分开", overridden_option_ids: [], reading_line_version: 7 } });
  });
  it("retains server-reported overridden choices and stops fetching after end", async () => {
    backend.overridden = ["q1-a"];
    const wrapper = render(); await openWiki(wrapper);
    await wrapper.get('input[value="q1-a"]').setValue(true); await wrapper.get("textarea").setValue("保持距离");
    await wrapper.get('[data-action="submit"]').trigger("click"); await flushPromises();
    expect(wrapper.get('section[aria-label="已提交答案"]').text()).toContain("被文字覆盖");
    await wrapper.get('[data-action="end"]').trigger("click"); await flushPromises();
    const batches = backend.invoke.mock.calls.filter(([command]) => command === "get_question_batch").length;
    await openWiki(wrapper);
    expect(backend.invoke.mock.calls.filter(([command]) => command === "get_question_batch")).toHaveLength(batches);
    expect(wrapper.get('section[aria-label="已结束的下一章选择"]').text()).toContain("已被用户文字覆盖");
  });
  it("resumes at the next persisted batch without inventing historical labels", async () => {
    backend.history = [{ book_id: 1, chapter_idx: 2, batch_index: 2, question_batch_id: "old-batch", selected_option_ids: ["old-q:old-option"], free_text: "已保存补充", overridden_option_ids: [], reading_line_version: 6 }];
    const wrapper = render(); await openWiki(wrapper);
    expect(backend.invoke).toHaveBeenCalledWith("get_question_batch", { bookId: 1, chapterIdx: 2, batchIndex: 3 });
    expect(wrapper.text()).not.toContain("old-option");
  });
  it("preserves streamed chapter deltas and ordinary next-chapter navigation", async () => {
    const wrapper = render(); await flushPromises();
    const invoke = backend.invoke.getMockImplementation()!;
    let finishChapter!: (value: unknown) => void;
    backend.invoke.mockImplementation((command: string, args?: Record<string, unknown>) => command === "get_chapter_cmd" && args?.idx === 2 ? new Promise(resolve => { finishChapter = resolve; }) : invoke(command, args));
    await wrapper.findAll("button").find(button => button.text() === "下一章")!.trigger("click");
    backend.listeners.get("chapter:delta")?.({ payload: { book_id: 1, chapter_idx: 2, delta: "正在生成的下一章" } });
    await flushPromises();
    expect(wrapper.get(".chapter-text").text()).toContain("正在生成的下一章");
    finishChapter({ idx: 2, title: "第二章", content: "保存的下一章正文", source: "generated", graph_error: null });
    await flushPromises();
    expect(wrapper.get(".chapter-text").text()).toContain("保存的下一章正文");
    expect(wrapper.get(".pager").text()).toContain("2 / 2");
  });
});
