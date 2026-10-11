export type WikiEntryKind = "character" | "faction" | "location" | "item" | "setting" | "plot";

export interface WikiEntryView {
  readonly id: string;
  readonly kind: WikiEntryKind;
  readonly name: string;
  readonly aliases: readonly string[];
  readonly summary: string;
  readonly state_items: readonly string[];
  readonly related_entries: readonly string[];
  readonly open_threads: readonly string[];
  readonly source_chapters: readonly number[];
}
export interface ChapterCardView {
  readonly chapter_idx: number;
  readonly title: string;
  readonly headline: string | null;
  readonly summary: string | null;
  readonly entry_ids: readonly string[];
  readonly ready: boolean;
}
export interface WikiCoverage {
  readonly visited: readonly number[];
  readonly ready: readonly number[];
  readonly missing: readonly number[];
}
export interface WikiView {
  readonly entries: readonly WikiEntryView[];
  readonly chapter_cards: readonly ChapterCardView[];
  readonly coverage: WikiCoverage;
  readonly wiki_revision: number;
  readonly reading_line_version: number;
  readonly ready: boolean;
}
/** StoryMap is the read-only chapter-card projection of the public Wiki. */
export type StoryMapView = Pick<WikiView, "chapter_cards" | "coverage" | "wiki_revision" | "reading_line_version" | "ready">;

export interface QuestionOption {
  readonly id: string;
  readonly label: string;
}
export interface Question {
  readonly id: string;
  /** Already fogged by the producer; the UI never derives text from next-Wiki. */
  readonly prompt: string;
  readonly options: readonly QuestionOption[];
}
export interface QuestionBatch {
  readonly batch_index: number;
  readonly question_batch_id: string;
  readonly questions: readonly [Question, Question, Question];
  readonly answered_count: number;
  readonly effective_answer_count: number;
  readonly has_more: boolean;
  readonly can_end: boolean;
  readonly invalid_option_ids?: readonly string[];
  /** Server-authored public reasons, keyed by globally unique option ID. */
  readonly invalid_reasons?: Readonly<Record<string, string>>;
}
export interface SelectedOption {
  readonly question_id: string;
  readonly option_id: string;
}
export interface QuestionnaireSubmission {
  readonly batch_index: number | null;
  readonly question_batch_id: string | null;
  readonly selected_options: readonly SelectedOption[];
  readonly free_text: string | null;
}
export type AnswerStatus = "effective" | "overridden" | "invalid";
export interface QuestionnaireAnswer extends SelectedOption {
  readonly prompt: string;
  readonly label: string;
  readonly status: AnswerStatus;
  readonly reason: string | null;
}
/** Retain submitted answers so text-overridden options remain visible. */
export interface QuestionnaireRecord {
  readonly batch_index: number;
  readonly question_batch_id: string;
  readonly answers: readonly QuestionnaireAnswer[];
  readonly free_text: string | null;
}
export interface AnswerResolution {
  readonly effective: readonly SelectedOption[];
  readonly selected: readonly (SelectedOption & { readonly valid: boolean; readonly invalid_reason: string | null })[];
  readonly overridden_option_ids: readonly string[];
}

function record(value: unknown, label: string, keys?: readonly string[]): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error(`${label} must be an object`);
  const result = Object.fromEntries(Object.entries(value));
  if (keys) for (const key of Object.keys(result)) if (!keys.includes(key)) throw new Error(`${label} has unknown field: ${key}`);
  return result;
}
function stringValue(value: unknown, label: string): string {
  if (typeof value !== "string") throw new Error(`${label} must be a string`);
  return value;
}
function nonEmptyString(value: unknown, label: string): string {
  const result = stringValue(value, label);
  if (!result.trim()) throw new Error(`${label} must not be empty`);
  return result;
}
function integer(value: unknown, label: string, minimum = 0): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < minimum) throw new Error(`${label} must be an integer >= ${minimum}`);
  return value;
}
function boolValue(value: unknown, label: string): boolean {
  if (typeof value !== "boolean") throw new Error(`${label} must be boolean`);
  return value;
}
function nullableString(value: unknown, label: string): string | null {
  return value === null ? null : stringValue(value, label);
}
function array<T>(value: unknown, label: string, parser: (item: unknown, label: string) => T): T[] {
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  return value.map((item, index) => parser(item, `${label}[${index}]`));
}
function strings(value: unknown, label: string): string[] { return array(value, label, stringValue); }
function chapters(value: unknown, label: string): number[] { return array(value, label, (item, itemLabel) => integer(item, itemLabel, 1)); }
function unique(values: readonly (string | number)[], label: string): void {
  if (new Set(values).size !== values.length) throw new Error(`${label} must be unique`);
}
function entryKind(value: unknown): WikiEntryKind {
  const result = stringValue(value, "entry.kind");
  if (result !== "character" && result !== "faction" && result !== "location" && result !== "item" && result !== "setting" && result !== "plot") throw new Error(`invalid entry.kind: ${result}`);
  return result;
}
function parseEntry(value: unknown): WikiEntryView {
  const v = record(value, "entry", ["id", "kind", "name", "aliases", "summary", "state_items", "related_entries", "open_threads", "source_chapters"]);
  return { id: nonEmptyString(v.id, "entry.id"), kind: entryKind(v.kind), name: nonEmptyString(v.name, "entry.name"), aliases: strings(v.aliases, "entry.aliases"), summary: stringValue(v.summary, "entry.summary"), state_items: strings(v.state_items, "entry.state_items"), related_entries: strings(v.related_entries, "entry.related_entries"), open_threads: strings(v.open_threads, "entry.open_threads"), source_chapters: chapters(v.source_chapters, "entry.source_chapters") };
}
function parseCard(value: unknown): ChapterCardView {
  const v = record(value, "chapter card", ["chapter_idx", "title", "headline", "summary", "entry_ids", "ready"]);
  return { chapter_idx: integer(v.chapter_idx, "card.chapter_idx", 1), title: stringValue(v.title, "card.title"), headline: nullableString(v.headline, "card.headline"), summary: nullableString(v.summary, "card.summary"), entry_ids: strings(v.entry_ids, "card.entry_ids"), ready: boolValue(v.ready, "card.ready") };
}
function parseCoverage(value: unknown): WikiCoverage {
  const v = record(value, "coverage", ["visited", "ready", "missing"]);
  const result = { visited: chapters(v.visited, "coverage.visited"), ready: chapters(v.ready, "coverage.ready"), missing: chapters(v.missing, "coverage.missing") };
  unique(result.visited, "coverage.visited");
  unique(result.ready, "coverage.ready");
  unique(result.missing, "coverage.missing");
  return result;
}
export function parseWikiView(value: unknown): WikiView {
  const v = record(value, "Wiki view", ["entries", "chapter_cards", "coverage", "wiki_revision", "reading_line_version", "ready"]);
  const entries = array(v.entries, "entries", parseEntry);
  const chapter_cards = array(v.chapter_cards, "chapter_cards", parseCard);
  unique(entries.map(entry => entry.id), "entry ids");
  unique(chapter_cards.map(card => card.chapter_idx), "chapter indices");
  return { entries, chapter_cards, coverage: parseCoverage(v.coverage), wiki_revision: integer(v.wiki_revision, "wiki_revision"), reading_line_version: integer(v.reading_line_version, "reading_line_version"), ready: boolValue(v.ready, "ready") };
}
export function parseStoryMapView(value: unknown): StoryMapView {
  const v = record(value, "StoryMap view", ["chapter_cards", "coverage", "wiki_revision", "reading_line_version", "ready"]);
  const chapter_cards = array(v.chapter_cards, "chapter_cards", parseCard);
  unique(chapter_cards.map(card => card.chapter_idx), "chapter indices");
  return { chapter_cards, coverage: parseCoverage(v.coverage), wiki_revision: integer(v.wiki_revision, "wiki_revision"), reading_line_version: integer(v.reading_line_version, "reading_line_version"), ready: boolValue(v.ready, "ready") };
}
function parseQuestion(value: unknown): Question {
  const v = record(value, "question");
  const options = array(v.options, "question.options", item => {
    const option = record(item, "option");
    return { id: nonEmptyString(option.id, "option.id"), label: nonEmptyString(option.label, "option.label") };
  });
  if (options.length < 2) throw new Error("question must contain at least two options");
  unique(options.map(option => option.id), "option ids");
  return { id: nonEmptyString(v.id, "question.id"), prompt: nonEmptyString(v.prompt, "question.prompt"), options };
}
export function parseQuestionBatch(value: unknown): QuestionBatch {
  const v = record(value, "question batch");
  const questions = array(v.questions, "questions", parseQuestion);
  if (questions.length !== 3) throw new Error("question batch must contain exactly three questions");
  unique(questions.map(question => question.id), "question ids");
  unique(questions.flatMap(question => question.options.map(option => option.id)), "batch option ids");
  const answered_count = integer(v.answered_count, "answered_count");
  const effective_answer_count = integer(v.effective_answer_count, "effective_answer_count");
  if (effective_answer_count > answered_count) throw new Error("effective_answer_count cannot exceed answered_count");
  const [first, second, third] = questions;
  const result: QuestionBatch = { batch_index: integer(v.batch_index, "batch_index"), question_batch_id: nonEmptyString(v.question_batch_id, "question_batch_id"), questions: [first!, second!, third!], answered_count, effective_answer_count, has_more: boolValue(v.has_more, "has_more"), can_end: boolValue(v.can_end, "can_end") };
  const invalid_option_ids = v.invalid_option_ids === undefined ? undefined : strings(v.invalid_option_ids, "invalid_option_ids");
  const invalid_reasons = v.invalid_reasons === undefined ? undefined : Object.fromEntries(Object.entries(record(v.invalid_reasons, "invalid_reasons")).map(([key, reason]) => [key, nonEmptyString(reason, `invalid_reasons.${key}`)]));
  return { ...result, ...(invalid_option_ids === undefined ? {} : { invalid_option_ids }), ...(invalid_reasons === undefined ? {} : { invalid_reasons }) };
}
export function parseQuestionnaireSubmission(value: unknown, batch: QuestionBatch | null): QuestionnaireSubmission {
  const v = record(value, "submission", ["batch_index", "question_batch_id", "selected_options", "free_text"]);
  const selected_options = array(v.selected_options, "selected_options", item => {
    const answer = record(item, "selected option", ["question_id", "option_id"]);
    return { question_id: nonEmptyString(answer.question_id, "question_id"), option_id: nonEmptyString(answer.option_id, "option_id") };
  });
  if (selected_options.length > 3) throw new Error("a batch allows zero to three answers");
  if (new Set(selected_options.map(answer => answer.question_id)).size !== selected_options.length) throw new Error("select at most one option per question");
  const batch_index = v.batch_index === null ? null : integer(v.batch_index, "batch_index");
  const question_batch_id = v.question_batch_id === null ? null : nonEmptyString(v.question_batch_id, "question_batch_id");
  if (batch === null) {
    if (batch_index !== null || question_batch_id !== null || selected_options.length) throw new Error("answers require a current batch");
  } else {
    if (batch_index !== batch.batch_index || question_batch_id !== batch.question_batch_id) throw new Error("submission does not match current batch");
    for (const answer of selected_options) {
      const question = batch.questions.find(question => question.id === answer.question_id);
      if (!question || !question.options.some(option => option.id === answer.option_id)) throw new Error("unknown option in current batch");
    }
  }
  const text = v.free_text === null ? null : stringValue(v.free_text, "free_text").trim();
  return { batch_index, question_batch_id, selected_options, free_text: text || null };
}
export function parseQuestionnaireRecord(value: unknown): QuestionnaireRecord {
  const v = record(value, "questionnaire record", ["batch_index", "question_batch_id", "answers", "free_text"]);
  const answers = array(v.answers, "answers", item => {
    const answer = record(item, "answer", ["question_id", "option_id", "prompt", "label", "status", "reason"]);
    const status = stringValue(answer.status, "answer.status");
    if (status !== "effective" && status !== "overridden" && status !== "invalid") throw new Error(`invalid answer.status: ${status}`);
    const reason = nullableString(answer.reason, "answer.reason");
    if (status !== "effective" && !reason?.trim()) throw new Error("invalid or overridden answers require a public reason");
    return { question_id: nonEmptyString(answer.question_id, "answer.question_id"), option_id: nonEmptyString(answer.option_id, "answer.option_id"), prompt: nonEmptyString(answer.prompt, "answer.prompt"), label: nonEmptyString(answer.label, "answer.label"), status: status as AnswerStatus, reason };
  });
  if (answers.length > 3) throw new Error("record allows zero to three answers");
  unique(answers.map(answer => answer.question_id), "record question ids");
  return { batch_index: integer(v.batch_index, "record.batch_index"), question_batch_id: nonEmptyString(v.question_batch_id, "record.question_batch_id"), answers, free_text: nullableString(v.free_text, "record.free_text") };
}

export function parseAnswerResolution(value: unknown): AnswerResolution {
  const v = record(value, "answer resolution", ["effective", "selected", "overridden_option_ids"]);
  const effective = array(v.effective, "effective", item => {
    const answer = record(item, "effective answer", ["question_id", "option_id"]);
    return { question_id: nonEmptyString(answer.question_id, "question_id"), option_id: nonEmptyString(answer.option_id, "option_id") };
  });
  const selected = array(v.selected, "selected", item => {
    const answer = record(item, "resolved answer", ["question_id", "option_id", "valid", "invalid_reason"]);
    const valid = boolValue(answer.valid, "answer.valid");
    const invalid_reason = nullableString(answer.invalid_reason, "answer.invalid_reason");
    if (!valid && !invalid_reason?.trim()) throw new Error("invalid answers require a public reason");
    return { question_id: nonEmptyString(answer.question_id, "question_id"), option_id: nonEmptyString(answer.option_id, "option_id"), valid, invalid_reason };
  });
  const overridden_option_ids = strings(v.overridden_option_ids, "overridden_option_ids");
  if (selected.length > 3) throw new Error("resolution allows zero to three answers");
  unique(selected.map(answer => answer.question_id), "resolution question ids");
  unique(selected.map(answer => answer.option_id), "resolution option ids");
  unique(effective.map(answer => answer.option_id), "effective option ids");
  unique(overridden_option_ids, "overridden_option_ids");
  for (const answer of effective) {
    if (!selected.some(item => item.valid && item.question_id === answer.question_id && item.option_id === answer.option_id)) throw new Error("effective answer must be a valid selected answer");
  }
  for (const answer of selected) {
    if (answer.valid && !effective.some(item => item.question_id === answer.question_id && item.option_id === answer.option_id)) throw new Error("valid answer must be effective");
  }
  for (const id of overridden_option_ids) {
    if (!selected.some(answer => answer.option_id === id && !answer.valid)) throw new Error("overridden option must be an invalid selected answer");
  }
  return { effective, selected, overridden_option_ids };
}

/** Attach only already-public fogged labels to server-resolved selections. */
export function createQuestionnaireRecord(batch: QuestionBatch, submission: QuestionnaireSubmission, value: unknown): QuestionnaireRecord {
  const input = parseQuestionnaireSubmission(submission, batch);
  const resolution = parseAnswerResolution(value);
  if (resolution.selected.length !== input.selected_options.length || resolution.selected.some(answer => !input.selected_options.some(item => item.question_id === answer.question_id && item.option_id === answer.option_id))) throw new Error("resolution must match submitted selections");
  if (!input.free_text && resolution.overridden_option_ids.length) throw new Error("text overrides require user text");
  const answers: QuestionnaireAnswer[] = resolution.selected.map(answer => {
    const question = batch.questions.find(question => question.id === answer.question_id)!;
    const option = question.options.find(option => option.id === answer.option_id)!;
    return { question_id: answer.question_id, option_id: answer.option_id, prompt: question.prompt, label: option.label, status: answer.valid ? "effective" : resolution.overridden_option_ids.includes(answer.option_id) ? "overridden" : "invalid", reason: answer.invalid_reason };
  });
  return { batch_index: batch.batch_index, question_batch_id: batch.question_batch_id, answers, free_text: input.free_text };
}
