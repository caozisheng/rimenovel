import { mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, describe, expect, it } from "vitest";
import WikiWorkbench from "./WikiWorkbench.vue";
import StoryMap from "./StoryMap.vue";
import { parseWikiView } from "../lib/wiki";

const view = parseWikiView({
  entries: [{ id: "traveller", kind: "character", name: "旅人", aliases: ["访客"], summary: "已经来到城中", state_items: ["在城中"], related_entries: [], open_threads: ["来意不明"], source_chapters: [1] }],
  chapter_cards: [{ chapter_idx: 1, title: "入城", headline: "访客到来", summary: "旅人进入城中", entry_ids: ["traveller"], ready: true }, { chapter_idx: 2, title: "未访问", headline: null, summary: null, entry_ids: [], ready: false }],
  coverage: { visited: [1], ready: [1], missing: [] }, wiki_revision: 1, reading_line_version: 0, ready: true,
});
const mounted: VueWrapper[] = [];
afterEach(() => { for (const wrapper of mounted.splice(0)) wrapper.unmount(); });

describe("read-only Wiki and StoryMap", () => {
  it("displays public entry fields without any field editor", async () => {
    const wrapper = mount(WikiWorkbench, { props: { view, canJump: true } });
    mounted.push(wrapper);
    expect(wrapper.text()).toContain("旅人");
    expect(wrapper.text()).toContain("在城中");
    expect(wrapper.text()).toContain("来意不明");
    expect(wrapper.findAll("input, textarea, select")).toHaveLength(0);
    const source = wrapper.findAll("button").find(button => button.text() === "第 1 章");
    expect(source).toBeDefined();
    await source!.trigger("click");
    expect(wrapper.emitted("jump")![0]).toEqual([1]);
  });

  it("offers read navigation only for visited chapter cards", async () => {
    const wrapper = mount(StoryMap, { props: { view, canJump: true } });
    mounted.push(wrapper);
    expect(wrapper.text()).toContain("访客到来");
    const jumps = wrapper.findAll("button");
    expect(jumps).toHaveLength(1);
    await jumps[0]!.trigger("click");
    expect(wrapper.emitted("jump")![0]).toEqual([1]);
    expect(wrapper.emitted("edit")).toBeUndefined();
    expect(wrapper.emitted("mutate")).toBeUndefined();
  });
});
