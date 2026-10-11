import { describe, expect, it } from "vitest";
import { createQuestionnaireRecord, parseQuestionBatch, parseQuestionnaireRecord, parseQuestionnaireSubmission, parseWikiView } from "./wiki";

const rawBatch = {
  batch_index: 0,
  question_batch_id: "batch-1",
  answered_count: 0,
  effective_answer_count: 0,
  has_more: true,
  can_end: true,
  questions: [1, 2, 3].map(index => ({
    id: `q${index}`, prompt: `模糊方向 ${index}`,
    options: [{ id: `q${index}-a`, label: "靠近" }, { id: `q${index}-b`, label: "远离" }],
  })),
};

describe("public questionnaire records", () => {
  it("requires exactly three questions and strips private metadata", () => {
    const batch = parseQuestionBatch({ ...rawBatch, next_wiki: { secret: "未来" }, questions: rawBatch.questions.map(question => ({ ...question, options: question.options.map(option => ({ ...option, conflicts_with: ["secret"] })) })) });
    expect(batch.questions).toHaveLength(3);
    expect(batch.questions[0].options[0]).toEqual({ id: "q1-a", label: "靠近" });
    expect(batch).not.toHaveProperty("next_wiki");
    for (const count of [0, 1, 2, 4]) {
      expect(() => parseQuestionBatch({ ...rawBatch, questions: Array.from({ length: count }, (_, index) => ({ ...rawBatch.questions[0], id: `q${index}` })) })).toThrow(/exactly three/);
    }
  });

  it("rejects duplicate questions, duplicate options, empty text and fewer than two options", () => {
    expect(() => parseQuestionBatch({ ...rawBatch, questions: [rawBatch.questions[0], rawBatch.questions[0], rawBatch.questions[2]] })).toThrow(/unique/);
    for (const question of [
      { ...rawBatch.questions[0], options: [rawBatch.questions[0].options[0], rawBatch.questions[0].options[0]] },
      { ...rawBatch.questions[0], prompt: " " },
      { ...rawBatch.questions[0], options: [rawBatch.questions[0].options[0]] },
    ]) {
      expect(() => parseQuestionBatch({ ...rawBatch, questions: [question, ...rawBatch.questions.slice(1)] })).toThrow();
    }
  });

  it("allows zero answers, partial answers and all three answers", () => {
    const batch = parseQuestionBatch(rawBatch);
    for (const count of [0, 1, 2, 3]) {
      const selected_options = batch.questions.slice(0, count).map(question => ({ question_id: question.id, option_id: question.options[0]!.id }));
      expect(parseQuestionnaireSubmission({ batch_index: 0, question_batch_id: "batch-1", selected_options, free_text: "  自由补充  " }, batch)).toEqual({ batch_index: 0, question_batch_id: "batch-1", selected_options, free_text: "自由补充" });
    }
  });

  it("rejects repeated answers, unknown options and stale batch identities", () => {
    const batch = parseQuestionBatch(rawBatch);
    const submission = { batch_index: 0, question_batch_id: "batch-1", selected_options: [], free_text: null };
    expect(() => parseQuestionnaireSubmission({ ...submission, selected_options: [{ question_id: "q1", option_id: "q1-a" }, { question_id: "q1", option_id: "q1-b" }] }, batch)).toThrow(/one option/);
    expect(() => parseQuestionnaireSubmission({ ...submission, selected_options: [{ question_id: "q1", option_id: "unknown" }] }, batch)).toThrow(/unknown option/);
    expect(() => parseQuestionnaireSubmission({ ...submission, question_batch_id: "old-batch" }, batch)).toThrow(/batch/);
  });

  it("allows ending without a generated batch, but not attaching answers to no batch", () => {
    expect(parseQuestionnaireSubmission({ batch_index: null, question_batch_id: null, selected_options: [], free_text: " " }, null)).toEqual({ batch_index: null, question_batch_id: null, selected_options: [], free_text: null });
    expect(() => parseQuestionnaireSubmission({ batch_index: null, question_batch_id: null, selected_options: [{ question_id: "q1", option_id: "q1-a" }], free_text: null }, null)).toThrow(/batch/);
  });

  it("retains text-overridden options with authoritative public reasons", () => {
    const batch = parseQuestionBatch(rawBatch);
    const submission = parseQuestionnaireSubmission({ batch_index: 0, question_batch_id: "batch-1", selected_options: [{ question_id: "q1", option_id: "q1-a" }], free_text: "保持距离" }, batch);
    const record = createQuestionnaireRecord(batch, submission, { effective: [], selected: [{ question_id: "q1", option_id: "q1-a", valid: false, invalid_reason: "已被用户文字覆盖" }], overridden_option_ids: ["q1-a"] });
    expect(record.answers[0]).toMatchObject({ label: "靠近", status: "overridden", reason: "已被用户文字覆盖" });
    expect(parseQuestionnaireRecord(record)).toEqual(record);
    expect(() => parseQuestionnaireRecord({ ...record, answers: [{ ...record.answers[0], reason: null }] })).toThrow(/reason/);
  });
});

describe("read-only Wiki records", () => {
  const view = {
    entries: [{ id: "entry-1", kind: "character", name: "旅人", aliases: [], summary: "已经阅读的内容", state_items: ["在城中"], related_entries: [], open_threads: [], source_chapters: [1] }],
    chapter_cards: [{ chapter_idx: 1, title: "第一章", headline: null, summary: "已读摘要", entry_ids: ["entry-1"], ready: true }],
    coverage: { visited: [1], ready: [1], missing: [] },
    wiki_revision: 1, reading_line_version: 0, ready: true,
  };
  it("parses read-only entries and chapter cards", () => {
    expect(parseWikiView(view)).toEqual(view);
  });
  it("rejects unknown and private future fields instead of exposing them", () => {
    expect(() => parseWikiView({ ...view, next_wiki: {} })).toThrow(/unknown/);
    expect(() => parseWikiView({ ...view, entries: [{ ...view.entries[0], goals: ["未来"] }] })).toThrow(/unknown/);
    expect(() => parseWikiView({ ...view, entries: [{ ...view.entries[0], kind: "future" }] })).toThrow(/kind/);
  });
  it("rejects duplicate ids and invalid chapter/revision numbers", () => {
    expect(() => parseWikiView({ ...view, entries: [view.entries[0], view.entries[0]] })).toThrow(/unique/);
    expect(() => parseWikiView({ ...view, wiki_revision: -1 })).toThrow(/wiki_revision/);
    expect(() => parseWikiView({ ...view, coverage: { ...view.coverage, visited: [0] } })).toThrow(/visited/);
  });
});
