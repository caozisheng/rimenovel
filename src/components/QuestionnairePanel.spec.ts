import { mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, describe, expect, it } from "vitest";
import QuestionnairePanel from "./QuestionnairePanel.vue";
import { parseQuestionBatch, type QuestionnaireSubmission } from "../lib/wiki";

const batch = parseQuestionBatch({
  batch_index: 0, question_batch_id: "batch-1", answered_count: 2, effective_answer_count: 1, has_more: true, can_end: true,
  questions: [1, 2, 3].map(index => ({ id: `q${index}`, prompt: `雾中问题 ${index}`, options: [{ id: `q${index}-a`, label: "靠近一些" }, { id: `q${index}-b`, label: "保持距离" }] })),
});
const mounted: VueWrapper[] = [];
afterEach(() => { for (const wrapper of mounted.splice(0)) wrapper.unmount(); });
function render(props: Record<string, unknown> = {}) {
  const wrapper = mount(QuestionnairePanel, { props: { batch, ...props } });
  mounted.push(wrapper);
  return wrapper;
}

describe("fogged questionnaire panel", () => {
  it("shows three questions and submits zero, one or three selected answers", async () => {
    const wrapper = render();
    expect(wrapper.findAll("fieldset")).toHaveLength(3);
    expect(wrapper.text()).toContain("已回答 2");
    expect(wrapper.text()).toContain("有效答案 1");
    await wrapper.get('[data-action="submit"]').trigger("click");
    expect((wrapper.emitted("submit")![0]![0] as QuestionnaireSubmission).selected_options).toHaveLength(0);
    await wrapper.get('input[value="q1-a"]').setValue(true);
    await wrapper.get('[data-action="submit"]').trigger("click");
    expect((wrapper.emitted("submit")![1]![0] as QuestionnaireSubmission).selected_options).toHaveLength(1);
    await wrapper.get('input[value="q2-a"]').setValue(true);
    await wrapper.get('input[value="q3-a"]').setValue(true);
    await wrapper.get('[data-action="submit"]').trigger("click");
    expect((wrapper.emitted("submit")![2]![0] as QuestionnaireSubmission).selected_options).toHaveLength(3);
  });

  it("allows clearing an answer and includes optional text in end payload", async () => {
    const wrapper = render();
    await wrapper.get('input[value="q1-a"]').setValue(true);
    await wrapper.get('[data-clear="q1"]').trigger("click");
    await wrapper.get("textarea").setValue("  不要让他们分开  ");
    await wrapper.get('[data-action="end"]').trigger("click");
    expect(wrapper.emitted("end")![0]![0]).toMatchObject({ selected_options: [], free_text: "不要让他们分开" });
  });

  it("keeps end available during loading, failures and without a batch", async () => {
    const wrapper = render({ batch: null, loading: true, error: "暂时不可用" });
    expect(wrapper.get('[data-action="end"]').attributes("disabled")).toBeUndefined();
    await wrapper.get('[data-action="end"]').trigger("click");
    expect(wrapper.emitted("end")![0]![0]).toEqual({ batch_index: null, question_batch_id: null, selected_options: [], free_text: null });
  });

  it("retains overridden answers and public reasons without exposing private metadata", () => {
    const wrapper = render({ records: [{ batch_index: 0, question_batch_id: "previous", free_text: "继续同行", answers: [{ question_id: "old-q", option_id: "old-a", prompt: "是否分别", label: "分头行动", status: "overridden", reason: "已被用户文字覆盖" }] }] });
    expect(wrapper.text()).toContain("分头行动");
    expect(wrapper.text()).toContain("已被用户文字覆盖");
    expect(wrapper.text()).toContain("无效");
    expect(wrapper.text()).not.toContain("conflicts_with");
    expect(wrapper.text()).not.toContain("next_wiki");
  });

  it("resets choices when the batch changes, retaining optional free text", async () => {
    const wrapper = render();
    await wrapper.get('input[value="q1-a"]').setValue(true);
    await wrapper.get("textarea").setValue("继续合作");
    await wrapper.setProps({ batch: { ...batch, batch_index: 1, question_batch_id: "batch-2" } });
    await wrapper.get('[data-action="end"]').trigger("click");
    expect(wrapper.emitted("end")![0]![0]).toMatchObject({ batch_index: 1, selected_options: [], free_text: "继续合作" });
  });
});
