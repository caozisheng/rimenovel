<script setup lang="ts">
// 阅读器（P2 基础版：单章滚动 + 章节导航；阅读线/混合翻页在 P6/P7 接入）
import { onMounted, ref, watch } from "vue";
import type { ChapterMeta } from "../lib/types";
import { getChapter, getChapterTitles } from "../lib/ipc";

const props = defineProps<{ bookId: number }>();
const emit = defineEmits<{ back: [] }>();

const titles = ref<ChapterMeta[]>([]);
const currentIdx = ref(1);
const chapterText = ref("");
const chapterTitle = ref("");
const loading = ref(false);
const fontSize = ref(18);

async function loadChapter(idx: number): Promise<void> {
  if (idx < 1 || idx > titles.value.length) return;
  loading.value = true;
  try {
    const ch = await getChapter(props.bookId, idx);
    currentIdx.value = ch.idx;
    chapterTitle.value = ch.title;
    chapterText.value = ch.content;
    window.scrollTo({ top: 0 });
  } finally {
    loading.value = false;
  }
}

watch(() => props.bookId, () => void loadChapter(currentIdx.value));

onMounted(async () => {
  titles.value = await getChapterTitles(props.bookId);
  if (titles.value.length > 0) await loadChapter(1);
});

function hasPrev(): boolean {
  return currentIdx.value > 1;
}
function hasNext(): boolean {
  return currentIdx.value < titles.value.length;
}
</script>

<template>
  <main class="reader">
    <header class="reader-bar">
      <button @click="emit('back')">← 书架</button>
      <span class="ch-title">{{ chapterTitle }}</span>
      <select v-model.number="fontSize" class="font-ctl">
        <option :value="14">14px</option>
        <option :value="18">18px</option>
        <option :value="22">22px</option>
      </select>
    </header>
    <article
      v-if="!loading"
      class="chapter-text"
      :style="{ fontSize: `${fontSize}px`, lineHeight: 1.8 }"
    >
      {{ chapterText }}
    </article>
    <p v-else class="loading">加载中…</p>
    <nav class="pager" v-if="titles.length">
      <button :disabled="!hasPrev()" @click="loadChapter(currentIdx - 1)">上一章</button>
      <span>{{ currentIdx }} / {{ titles.length }}</span>
      <button :disabled="!hasNext()" @click="loadChapter(currentIdx + 1)">下一章</button>
    </nav>
  </main>
</template>

<style scoped>
.reader { max-width: 720px; margin: 0 auto; padding: 16px 24px 96px; }
.reader-bar { display: flex; justify-content: space-between; align-items: center; gap: 12px; }
.ch-title { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.chapter-text { white-space: pre-wrap; margin-top: 16px; text-indent: 2em; }
.loading { color: #888; padding: 32px 0; text-align: center; }
.pager { position: fixed; bottom: 0; left: 0; right: 0; display: flex; justify-content: center; gap: 24px; padding: 12px; background: var(--vv-bg, #fff); border-top: 1px solid #8884; }
</style>
