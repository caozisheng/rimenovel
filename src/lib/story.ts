import { invoke } from "@tauri-apps/api/core";
import { parseQuestionBatch, parseQuestionnaireRecord, parseWikiView, type QuestionBatch, type QuestionnaireRecord, type QuestionnaireSubmission, type WikiView } from "./wiki";

export interface InterventionResult {
  readonly id: number;
  readonly overridden_option_ids: readonly string[];
}
export interface ActiveIntervention {
  readonly book_id: number;
  readonly chapter_idx: number;
  readonly batch_index: number | null;
  readonly question_batch_id: string | null;
  readonly selected_option_ids: readonly string[];
  readonly free_text: string | null;
  readonly overridden_option_ids: readonly string[];
  readonly reading_line_version: number;
}

function objectValue(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error(`${label} must be an object`);
  return value as Record<string, unknown>;
}
function integerValue(value: unknown, label: string, minimum = 0): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < minimum) throw new Error(`${label} must be an integer >= ${minimum}`);
  return value;
}
function nullableText(value: unknown, label: string): string | null {
  if (value !== null && typeof value !== "string") throw new Error(`${label} must be a string or null`);
  return value;
}
function stringArray(value: unknown, label: string): string[] {
  if (!Array.isArray(value) || !value.every((item): item is string => typeof item === "string")) throw new Error(`${label} must be a string array`);
  return value;
}
function parseInterventionResult(value: unknown): InterventionResult {
  const result = objectValue(value, "intervention result");
  return { id: integerValue(result.id, "intervention result.id", 1), overridden_option_ids: stringArray(result.overridden_option_ids, "intervention result.overridden_option_ids") };
}

export function getActiveInterventions(bookId: number, chapterIdx: number): Promise<ActiveIntervention[]> {
  return invoke<unknown>("get_active_interventions", { bookId, chapterIdx }).then(value => {
    if (!Array.isArray(value)) throw new Error("active interventions must be an array");
    return value.map(item => {
      const record = objectValue(item, "active intervention");
      return {
        book_id: integerValue(record.book_id, "intervention.book_id", 1),
        chapter_idx: integerValue(record.chapter_idx, "intervention.chapter_idx", 1),
        batch_index: record.batch_index === null ? null : integerValue(record.batch_index, "intervention.batch_index"),
        question_batch_id: nullableText(record.question_batch_id, "intervention.question_batch_id"),
        selected_option_ids: stringArray(record.selected_option_ids, "intervention.selected_option_ids"),
        free_text: nullableText(record.free_text, "intervention.free_text"),
        overridden_option_ids: stringArray(record.overridden_option_ids, "intervention.overridden_option_ids"),
        reading_line_version: integerValue(record.reading_line_version, "intervention.reading_line_version"),
      };
    });
  });
}

export function getWikiView(bookId: number): Promise<WikiView> {
  return invoke<unknown>("get_story_map", { bookId }).then(parseWikiView);
}

export function getQuestionBatch(bookId: number, chapterIdx: number, batchIndex: number): Promise<QuestionBatch | null> {
  return invoke<unknown>("get_question_batch", { bookId, chapterIdx, batchIndex }).then(value => value === null ? null : parseQuestionBatch(value));
}

export function getNextWiki(bookId: number, chapterIdx: number): Promise<unknown | null> {
  return invoke<unknown>("get_next_wiki", { bookId, chapterIdx });
}

export function saveIntervention(submission: QuestionnaireSubmission, bookId: number, chapterIdx: number, readingLineVersion: number): Promise<InterventionResult> {
  return invoke<unknown>("save_intervention", {
    input: {
      book_id: bookId,
      chapter_idx: chapterIdx,
      batch_index: submission.batch_index,
      question_batch_id: submission.question_batch_id,
      selected_option_ids: submission.selected_options.map(option => `${option.question_id}:${option.option_id}`),
      free_text: submission.free_text,
      overridden_option_ids: [],
      reading_line_version: readingLineVersion,
    },
  }).then(parseInterventionResult);
}

export function endQuestionnaire(submission: QuestionnaireSubmission, bookId: number, chapterIdx: number, readingLineVersion: number): Promise<InterventionResult> {
  return invoke<unknown>("end_questionnaire", {
    input: {
      book_id: bookId,
      chapter_idx: chapterIdx,
      batch_index: submission.batch_index,
      question_batch_id: submission.question_batch_id,
      selected_option_ids: submission.selected_options.map(option => `${option.question_id}:${option.option_id}`),
      free_text: submission.free_text,
      overridden_option_ids: [],
      reading_line_version: readingLineVersion,
    },
  }).then(parseInterventionResult);
}

export function parseQuestionnaireHistory(value: unknown): QuestionnaireRecord[] {
  if (!Array.isArray(value)) throw new Error("questionnaire history must be an array");
  return value.map(parseQuestionnaireRecord);
}
