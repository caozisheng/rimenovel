<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { NAlert, NButton, NConfigProvider, NEmpty, NSelect, darkTheme } from "naive-ui";
import WikiWorkbench from "./WikiWorkbench.vue";
import QuestionnairePanel from "./QuestionnairePanel.vue";
import type { ChapterMeta, ReadingMode, RewriteStatus } from "../lib/types";
import type { QuestionBatch, QuestionnaireAnswer, QuestionnaireRecord, QuestionnaireSubmission, WikiView } from "../lib/wiki";
import { getChapter, onKgEvent, parseChapterDelta } from "../lib/kg";
import { getChapterTitles } from "../lib/ipc";
import { endQuestionnaire, getActiveInterventions, getQuestionBatch, getWikiView, saveIntervention } from "../lib/story";
import { readingChapter, readingMode as savedReadingMode, saveReadingChapter, saveReadingMode } from "../lib/readingMode";
const props = defineProps<{ bookId: number }>();
const emit = defineEmits<{ back: [] }>();
const titles = ref<ChapterMeta[]>([]);
const currentIdx = ref(1);
const chapterText = ref("");
const chapterTitle = ref("");
const chapterSource = ref("");
const readingMode = ref<ReadingMode>("original");
const rewriteStatus = ref<RewriteStatus>("ready");
const wikiWarning = ref<string | null>(null);
const loading = ref(false);
const error = ref("");
const fontSize = ref(18);
const readerTheme = ref<"paper" | "sepia" | "night">("paper");
const savedScroll = ref(0);
const wikiOpen = ref(false);
const wikiView = ref<WikiView | null>(null);
const wikiLoading = ref(false);
const wikiError = ref("");
const questionBatch = ref<QuestionBatch | null>(null);
const questionLoading = ref(false);
const questionError = ref("");
const submitting = ref(false);
const records = ref<QuestionnaireRecord[]>([]);
const nextChapterIdx = computed(() => currentIdx.value + 1);
const hasNextChapter = computed(() => nextChapterIdx.value <= titles.value.length);
const chapterRecords = new Map<number, QuestionnaireRecord[]>();
const endedChapters = ref(new Set<number>());
let workbenchToken = 0;
let focusBeforeWiki: HTMLElement | null = null;
let stopDelta: (() => void) | null = null;
let pendingIdx: number | null = null;
let disposed = false;
const streamingText = ref("");
const paragraphs = computed(() => (loading.value ? streamingText.value : chapterText.value).split(/\r?\n/).map((line) => line.trim()).filter(Boolean));
const fontOptions = [16, 18, 20, 22].map((size) => ({ label: `${size} px`, value: size }));
const themeOptions = [{ label: "纸白", value: "paper" }, { label: "米黄", value: "sepia" }, { label: "夜间", value: "night" }];
const readingModeOptions = [{ label: "原文", value: "original" }, { label: "改写", value: "rewrite" }];
const readerOverrides = computed(() => ({ common: { primaryColor: readerTheme.value === "night" ? "#818cf8" : "#6366f1", primaryColorHover: readerTheme.value === "night" ? "#a5b4fc" : "#818cf8", primaryColorPressed: "#4f46e5", bodyColor: readerTheme.value === "sepia" ? "#f5efdc" : readerTheme.value === "night" ? "#18181b" : "#ffffff", cardColor: readerTheme.value === "sepia" ? "#f5efdc" : readerTheme.value === "night" ? "#18181b" : "#ffffff", textColor2: readerTheme.value === "sepia" ? "#5b4a32" : readerTheme.value === "night" ? "#d4d4d8" : "#27272a" } }));

async function loadChapter(idx: number): Promise<void> {
  if (idx < 1 || idx > titles.value.length || loading.value) return;
  loading.value = true; error.value = ""; wikiWarning.value = null; pendingIdx = idx; streamingText.value = "";
  try {
    const ch = await getChapter(props.bookId, idx, readingMode.value);
    currentIdx.value = ch.idx; chapterTitle.value = ch.title; chapterText.value = ch.content; chapterSource.value = ch.source; rewriteStatus.value = ch.rewrite_status ?? "ready"; wikiWarning.value = ch.graph_error ?? null; window.scrollTo({ top: 0 });
    saveReadingChapter(props.bookId, readingMode.value, ch.idx);
  } catch (e) { error.value = String(e); }
  finally { loading.value = false; pendingIdx = null; streamingText.value = ""; }
}
async function changeReadingMode(mode: ReadingMode): Promise<void> {
  if (mode === readingMode.value) return;
  readingMode.value = mode; saveReadingMode(props.bookId, mode);
  await loadChapter(readingChapter(props.bookId, mode));
}
async function loadBook(): Promise<void> {
  closeWiki();
  readingMode.value = savedReadingMode(props.bookId);
  wikiView.value = null; questionBatch.value = null; records.value = []; chapterRecords.clear(); endedChapters.value.clear(); submitting.value = false;
  error.value = ""; chapterText.value = ""; chapterTitle.value = ""; titles.value = []; currentIdx.value = 1;
  try { titles.value = await getChapterTitles(props.bookId); if (titles.value.length) await loadChapter(Math.min(readingChapter(props.bookId, readingMode.value), titles.value.length)); }
  catch (e) { error.value = String(e); }
}
function closeWiki(): void {
  wikiOpen.value = false;
  workbenchToken++;
  focusBeforeWiki?.focus({ preventScroll: true });
  focusBeforeWiki = null;
  window.scrollTo({ top: savedScroll.value });
}
function openWiki(): void {
  savedScroll.value = window.scrollY;
  focusBeforeWiki = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  wikiOpen.value = true;
  records.value = chapterRecords.get(nextChapterIdx.value) ?? [];
  wikiView.value = null;
  void refreshWorkbench();
}
function jumpToChapter(idx: number): void { closeWiki(); void loadChapter(idx); }

async function refreshWorkbench(): Promise<void> {
  const token = ++workbenchToken;
  const bookId = props.bookId;
  const chapterIdx = nextChapterIdx.value;
  const current = () => !disposed && token === workbenchToken && bookId === props.bookId && chapterIdx === nextChapterIdx.value;
  wikiLoading.value = true; wikiError.value = ""; wikiView.value = null;
  questionLoading.value = hasNextChapter.value; questionError.value = ""; questionBatch.value = null;
  const loadWiki = async () => {
    try { const view = await getWikiView(bookId); if (current()) wikiView.value = view; }
    catch (reason) { if (current()) wikiError.value = String(reason); }
    finally { if (current()) wikiLoading.value = false; }
  };
  const loadQuestions = async () => {
    if (!hasNextChapter.value) return;
    try {
      const history = await getActiveInterventions(bookId, chapterIdx);
      if (!current()) return;
      // Only retained public questions supply labels; raw history cannot reconstruct them.
      const activeBatchIds = new Set(history.map(item => item.question_batch_id));
      records.value = (chapterRecords.get(chapterIdx) ?? []).filter(record => activeBatchIds.has(record.question_batch_id));
      applyReturnedOverrides(history.flatMap(item => item.overridden_option_ids), chapterIdx);
      if (endedChapters.value.has(chapterIdx)) return;
      const batchIndex = history.reduce((next, item) => item.batch_index === null ? next : Math.max(next, item.batch_index + 1), 0);
      const batch = await getQuestionBatch(bookId, chapterIdx, batchIndex);
      if (current()) questionBatch.value = batch;
    } catch (reason) { if (current()) questionError.value = String(reason); }
    finally { if (current()) questionLoading.value = false; }
  };
  await Promise.all([loadWiki(), loadQuestions()]);
}

function applyReturnedOverrides(optionIds: readonly string[], chapterIdx: number): void {
  const overridden = new Set(optionIds);
  records.value = records.value.map<QuestionnaireRecord>(record => ({ ...record, answers: record.answers.map<QuestionnaireAnswer>(answer => overridden.has(answer.option_id) || overridden.has(`${answer.question_id}:${answer.option_id}`) ? { ...answer, status: "overridden", reason: "已被用户文字覆盖" } : answer) }));
  chapterRecords.set(chapterIdx, records.value);
}

async function submitQuestionnaire(submission: QuestionnaireSubmission, end: boolean): Promise<void> {
  if (submitting.value || !hasNextChapter.value) return;
  const bookId = props.bookId;
  const chapterIdx = nextChapterIdx.value;
  const batch = questionBatch.value;
  const current = () => !disposed && bookId === props.bookId && chapterIdx === nextChapterIdx.value;
  submitting.value = true; questionError.value = "";
  workbenchToken++;
  try {
    const view = wikiView.value ?? await getWikiView(bookId);
    if (!current()) return;
    wikiView.value = view;
    const result = await (end ? endQuestionnaire : saveIntervention)(submission, bookId, chapterIdx, view.reading_line_version);
    if (!current()) return;
    if (batch && submission.question_batch_id === batch.question_batch_id) {
      const overridden = new Set(result.overridden_option_ids);
      const record: QuestionnaireRecord = {
        batch_index: batch.batch_index,
        question_batch_id: batch.question_batch_id,
        free_text: submission.free_text,
        answers: submission.selected_options.map<QuestionnaireAnswer>(selected => {
          const question = batch.questions.find(item => item.id === selected.question_id)!;
          const option = question.options.find(item => item.id === selected.option_id)!;
          const isOverridden = overridden.has(option.id) || overridden.has(`${question.id}:${option.id}`);
          const invalidReason = batch.invalid_reasons?.[option.id] ?? (batch.invalid_option_ids?.includes(option.id) ? "该选项已被判定无效" : null);
          return { ...selected, prompt: question.prompt, label: option.label, status: isOverridden ? "overridden" : invalidReason ? "invalid" : "effective", reason: isOverridden ? "已被用户文字覆盖" : invalidReason };
        }),
      };
      records.value = [...records.value.filter(item => item.question_batch_id !== record.question_batch_id), record];
      chapterRecords.set(chapterIdx, records.value);
    }
    applyReturnedOverrides(result.overridden_option_ids, chapterIdx);
    if (end) endedChapters.value.add(chapterIdx);
    await refreshWorkbench();
    if (end && current()) closeWiki();
  } catch (reason) { if (current()) questionError.value = String(reason); }
  finally {
    if (!disposed && bookId === props.bookId) submitting.value = false;
    if (current()) { wikiLoading.value = false; questionLoading.value = false; }
  }
}
watch(() => props.bookId, loadBook);
onMounted(async () => {
  try {
    const stop = await onKgEvent("chapter:delta", parseChapterDelta, event => {
      if (!disposed && event.book_id === props.bookId && event.chapter_idx === pendingIdx) streamingText.value += event.delta;
    });
    if (disposed) stop(); else { stopDelta = stop; await loadBook(); }
  } catch (e) { if (!disposed) error.value = String(e); }
});
onUnmounted(() => { disposed = true; workbenchToken++; stopDelta?.(); });
</script>

<template>
  <NConfigProvider :theme="readerTheme === 'night' ? darkTheme : null" :theme-overrides="readerOverrides">
    <section v-if="wikiOpen" class="wiki-overlay" aria-label="Wiki 与下一章选择" @keydown.esc="closeWiki">
      <header class="wiki-bar"><NButton data-action="close-wiki" @click="closeWiki">返回阅读</NButton><span>第 {{ currentIdx }} 章 · Wiki / StoryMap</span></header>
      <div class="wiki-content">
        <WikiWorkbench :view="wikiView" :loading="wikiLoading" :error="wikiError" :can-jump="!loading && !submitting" @jump="jumpToChapter" />
        <QuestionnairePanel v-if="hasNextChapter && !endedChapters.has(nextChapterIdx)" :key="`${bookId}-${nextChapterIdx}`" :batch="questionBatch" :records="records" :loading="questionLoading" :submitting="submitting" :error="questionError" @submit="submitQuestionnaire($event, false)" @end="submitQuestionnaire($event, true)" />
        <section v-else-if="hasNextChapter" class="questionnaire-ended" aria-label="已结束的下一章选择">
          <p role="status">本章的选择已结束，返回阅读即可继续。</p>
          <article v-for="record in records" :key="record.question_batch_id" class="saved-record">
            <h3>第 {{ record.batch_index + 1 }} 批已提交答案</h3>
            <ul><li v-for="answer in record.answers" :key="answer.question_id"><span :class="{ 'invalid-answer': answer.status !== 'effective' }">{{ answer.prompt }}：{{ answer.label }}</span> · {{ answer.status === 'effective' ? '有效' : '无效' }}<p v-if="answer.reason">{{ answer.reason }}</p></li></ul>
            <p v-if="record.free_text" class="record-text">文字补充：{{ record.free_text }}</p>
          </article>
        </section>
      </div>
    </section>
    <main v-else class="reader" :data-reader-theme="readerTheme">
      <header class="reader-bar">
        <NButton quaternary @click="emit('back')">← 书架</NButton>
        <NSelect :value="readingMode" :options="readingModeOptions" aria-label="阅读版本" class="reading-mode-select" size="small" @update:value="changeReadingMode" />
        <span class="ch-title">{{ chapterTitle }}</span>
        <span v-if="readingMode === 'rewrite' && rewriteStatus !== 'ready'" class="source-label">{{ rewriteStatus === 'inherited' ? '改写线沿用原文' : rewriteStatus === 'generating' ? '改写生成中' : '本章尚未改写' }}</span>
        <span v-else-if="chapterSource" class="source-label">{{ chapterSource === 'generated' ? '改写' : '原文' }}</span>
        <NButton v-if="titles.length" size="small" data-action="open-wiki" :disabled="loading" @click="openWiki">Wiki / StoryMap</NButton>
        <div class="bar-ops"><NSelect v-model:value="fontSize" :options="fontOptions" aria-label="字号" class="font-select" size="small" /><NSelect v-model:value="readerTheme" :options="themeOptions" aria-label="阅读主题" class="theme-select" size="small" /></div>
      </header>
      <NAlert v-if="readingMode === 'rewrite' && rewriteStatus === 'unavailable'" type="info" class="warning">本章尚未改写；请先完成上一章的选择或文字干预。</NAlert>
      <NAlert v-if="wikiWarning" type="warning" class="warning">正文已保存；Wiki 同步稍后重试：{{ wikiWarning }}</NAlert>
      <NAlert v-if="error" type="error" class="error" role="alert">{{ error }}</NAlert>
      <p v-if="loading" class="loading" role="status">加载中…</p>
      <article v-if="titles.length && (!loading || streamingText)" class="chapter-text" :style="{ fontSize: `${fontSize}px` }"><p v-for="(paragraph, idx) in paragraphs" :key="idx">{{ paragraph }}</p></article>
      <NEmpty v-else-if="!titles.length && !loading && !error" description="本书暂无章节" class="loading" />
      <nav v-if="titles.length" class="pager" aria-label="章节导航"><NButton :disabled="loading || currentIdx <= 1" @click="loadChapter(currentIdx - 1)">上一章</NButton><span>{{ currentIdx }} / {{ titles.length }}</span><NButton :disabled="loading || currentIdx >= titles.length" @click="loadChapter(currentIdx + 1)">下一章</NButton></nav>
    </main>
  </NConfigProvider>
</template>

<style scoped>
.wiki-overlay { min-height: 100dvh; padding: max(var(--space-md), env(safe-area-inset-top)) max(var(--space-md), env(safe-area-inset-left), env(safe-area-inset-right)) max(var(--space-xl), env(safe-area-inset-bottom)); background: var(--color-surface); color: var(--color-text-primary); }
.wiki-bar { display: flex; align-items: center; flex-wrap: wrap; gap: var(--space-md); max-width: 1080px; margin: 0 auto var(--space-xl); }
.wiki-content { display: grid; gap: var(--space-xl); max-width: 1080px; margin: 0 auto; }
.questionnaire-ended { color: var(--color-text-secondary); }
.saved-record { margin-top: var(--space-md); padding: var(--space-md); border: 1px solid var(--color-border); border-radius: var(--radius-xs); overflow-wrap: anywhere; }
.saved-record h3 { margin: 0 0 var(--space-sm); font-size: 14px; }
.invalid-answer { text-decoration: line-through; }
.record-text { white-space: pre-wrap; }
@media (min-width: 900px) { .wiki-content { grid-template-columns: minmax(0, 1.2fr) minmax(0, 1fr); align-items: start; } }
.reader {
  --reader-bg: var(--reader-theme-paper-bg);
  --reader-text: var(--reader-theme-paper-text);
  background: var(--reader-bg);
  color: var(--reader-text);
  min-height: 100dvh;
  padding: max(var(--space-md), env(safe-area-inset-top)) max(var(--reader-inline-padding), env(safe-area-inset-left), env(safe-area-inset-right)) calc(96px + env(safe-area-inset-bottom));
  transition: background var(--transition-base), color var(--transition-base);
}
.reader[data-reader-theme="sepia"] {
  --reader-bg: var(--reader-theme-sepia-bg);
  --reader-text: var(--reader-theme-sepia-text);
}
.reader[data-reader-theme="night"] {
  --reader-bg: var(--reader-theme-night-bg);
  --reader-text: var(--reader-theme-night-text);
}
.reader-bar { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-sm); max-width: var(--reader-measure); margin: 0 auto; }
.source-label { font-size: 12px; color: var(--reader-text); border: 1px solid currentColor; border-radius: var(--radius-xs); padding: 2px 6px; }
.bar-ops { display: flex; gap: var(--space-xs); }
.font-select { width: 88px; }
.theme-select { width: 92px; }
.chapter-text { max-width: var(--reader-measure); margin: var(--space-xl) auto 0; font-family: var(--reader-font-serif); line-height: var(--reader-line-height); letter-spacing: var(--reader-letter-spacing); overflow-wrap: anywhere; }
.chapter-text p { margin: 0 0 0.8em; text-indent: 2em; white-space: pre-wrap; }
.loading { padding: var(--space-xl) 0; text-align: center; }
.warning { max-width: var(--reader-measure); margin: var(--space-md) auto; }
.pager { position: fixed; bottom: 0; left: 0; right: 0; display: flex; justify-content: center; align-items: center; gap: var(--space-lg); padding: 12px 12px max(12px, env(safe-area-inset-bottom)); background: var(--reader-bg); color: var(--reader-text); border-top: 1px solid currentColor; }
@media (max-width: 480px) {
  .reader { padding-inline: max(var(--space-md), env(safe-area-inset-left), env(safe-area-inset-right)); }
  .bar-ops { width: 100%; justify-content: flex-end; }
  .pager { gap: var(--space-md); }
}
</style>
