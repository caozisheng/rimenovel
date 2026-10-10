<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { NAlert, NButton, NConfigProvider, NEmpty, NSelect, darkTheme } from "naive-ui";
import type { ChapterMeta } from "../lib/types";
import { getChapter, getChapterTitles } from "../lib/ipc";

const props = defineProps<{ bookId: number }>();
const emit = defineEmits<{ back: [] }>();
const titles = ref<ChapterMeta[]>([]);
const currentIdx = ref(1);
const chapterText = ref("");
const chapterTitle = ref("");
const loading = ref(false);
const error = ref("");
const fontSize = ref(18);
const readerTheme = ref<"paper" | "sepia" | "night">("paper");
const paragraphs = computed(() => chapterText.value.split(/\r?\n/).map((line) => line.trim()).filter(Boolean));
const fontOptions = [16, 18, 20, 22].map((size) => ({ label: `${size} px`, value: size }));
const themeOptions = [
  { label: "纸白", value: "paper" },
  { label: "米黄", value: "sepia" },
  { label: "夜间", value: "night" },
];
const readerOverrides = computed(() => ({
  common: {
    primaryColor: readerTheme.value === "night" ? "#818cf8" : "#6366f1",
    primaryColorHover: readerTheme.value === "night" ? "#a5b4fc" : "#818cf8",
    primaryColorPressed: "#4f46e5",
    bodyColor: readerTheme.value === "sepia" ? "#f5efdc" : readerTheme.value === "night" ? "#18181b" : "#ffffff",
    cardColor: readerTheme.value === "sepia" ? "#f5efdc" : readerTheme.value === "night" ? "#18181b" : "#ffffff",
    textColor2: readerTheme.value === "sepia" ? "#5b4a32" : readerTheme.value === "night" ? "#d4d4d8" : "#27272a",
  },
}));

async function loadChapter(idx: number): Promise<void> {
  if (idx < 1 || idx > titles.value.length || loading.value) return;
  loading.value = true;
  error.value = "";
  try {
    const ch = await getChapter(props.bookId, idx);
    currentIdx.value = ch.idx;
    chapterTitle.value = ch.title;
    chapterText.value = ch.content;
    window.scrollTo({ top: 0 });
  } catch (e) {
    error.value = String(e);
  } finally {
    loading.value = false;
  }
}

async function loadBook(): Promise<void> {
  error.value = "";
  chapterText.value = "";
  chapterTitle.value = "";
  titles.value = [];
  currentIdx.value = 1;
  try {
    titles.value = await getChapterTitles(props.bookId);
    if (titles.value.length) await loadChapter(1);
  } catch (e) {
    error.value = String(e);
  }
}
watch(() => props.bookId, loadBook);
onMounted(loadBook);
</script>

<template>
  <NConfigProvider :theme="readerTheme === 'night' ? darkTheme : null" :theme-overrides="readerOverrides">
    <main class="reader" :data-reader-theme="readerTheme">
      <header class="reader-bar">
        <NButton quaternary @click="emit('back')">← 书架</NButton>
        <span class="ch-title">{{ chapterTitle }}</span>
        <div class="bar-ops">
          <NSelect v-model:value="fontSize" :options="fontOptions" aria-label="字号" class="font-select" size="small" />
          <NSelect v-model:value="readerTheme" :options="themeOptions" aria-label="阅读主题" class="theme-select" size="small" />
        </div>
      </header>
      <NAlert v-if="error" type="error" class="error">{{ error }}</NAlert>
      <p v-if="loading" class="loading" role="status">加载中…</p>
      <article v-else-if="titles.length" class="chapter-text" :style="{ fontSize: `${fontSize}px` }">
        <p v-for="(paragraph, idx) in paragraphs" :key="idx">{{ paragraph }}</p>
      </article>
      <NEmpty v-else-if="!error" description="本书暂无章节" class="loading" />
      <nav v-if="titles.length" class="pager" aria-label="章节导航">
        <NButton :disabled="loading || currentIdx <= 1" @click="loadChapter(currentIdx - 1)">上一章</NButton>
        <span>{{ currentIdx }} / {{ titles.length }}</span>
        <NButton :disabled="loading || currentIdx >= titles.length" @click="loadChapter(currentIdx + 1)">下一章</NButton>
      </nav>
    </main>
  </NConfigProvider>
</template>

<style scoped>
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
.ch-title { flex: 1; min-width: 0; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.bar-ops { display: flex; gap: var(--space-xs); }
.font-select { width: 88px; }
.theme-select { width: 92px; }
.chapter-text { max-width: var(--reader-measure); margin: var(--space-xl) auto 0; font-family: var(--reader-font-serif); line-height: var(--reader-line-height); letter-spacing: var(--reader-letter-spacing); overflow-wrap: anywhere; }
.chapter-text p { margin: 0 0 0.8em; text-indent: 2em; white-space: pre-wrap; }
.loading { padding: var(--space-xl) 0; text-align: center; }
.error { max-width: var(--reader-measure); margin: var(--space-md) auto; }
.pager { position: fixed; bottom: 0; left: 0; right: 0; display: flex; justify-content: center; align-items: center; gap: var(--space-lg); padding: 12px 12px max(12px, env(safe-area-inset-bottom)); background: var(--reader-bg); color: var(--reader-text); border-top: 1px solid currentColor; }
@media (max-width: 480px) {
  .reader { padding-inline: max(var(--space-md), env(safe-area-inset-left), env(safe-area-inset-right)); }
  .bar-ops { width: 100%; justify-content: flex-end; }
  .pager { gap: var(--space-md); }
}
</style>
