import { beforeEach, describe, expect, it, vi } from "vitest";
import { endQuestionnaire, getActiveInterventions, saveIntervention } from "./story";
const backend = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: backend.invoke }));
const submission = { batch_index: null, question_batch_id: null, selected_options: [], free_text: "继续同行" };
const intervention = { book_id: 1, chapter_idx: 2, batch_index: null, question_batch_id: null, selected_option_ids: [], free_text: "继续同行", overridden_option_ids: [], reading_line_version: 7 };
beforeEach(() => backend.invoke.mockReset());
describe("story intervention IPC contracts", () => {
  it.each([["save_intervention", saveIntervention], ["end_questionnaire", endQuestionnaire]] as const)("parses %s result as the real Rust object", async (command, save) => {
    backend.invoke.mockResolvedValue({ id: 12, overridden_option_ids: ["option-a"] });
    expect(await save(submission, 1, 2, 7)).toEqual({ id: 12, overridden_option_ids: ["option-a"] });
    expect(backend.invoke).toHaveBeenCalledWith(command, { input: intervention });
  });
  it("rejects the obsolete numeric save result", async () => {
    backend.invoke.mockResolvedValue(12);
    await expect(saveIntervention(submission, 1, 2, 7)).rejects.toThrow(/intervention result/);
  });
  it("loads raw active history without manufacturing question labels", async () => {
    backend.invoke.mockResolvedValue([intervention]);
    expect(await getActiveInterventions(1, 2)).toEqual([intervention]);
    expect(backend.invoke).toHaveBeenCalledWith("get_active_interventions", { bookId: 1, chapterIdx: 2 });
  });
  it("rejects malformed raw active history", async () => {
    backend.invoke.mockResolvedValue([{ ...intervention, selected_option_ids: [4] }]);
    await expect(getActiveInterventions(1, 2)).rejects.toThrow(/selected_option_ids/);
  });
});
