<script setup lang="ts">
// RimeNovel 入口：书架 ↔ 阅读器（后续里程碑逐步加入图谱工作台/时间线）
import { onMounted, onUnmounted, ref } from "vue";
import type { Book, ExtractionStatus } from "./lib/types";
import ReaderView from "./components/ReaderView.vue";
import ProviderPanel from "./components/ProviderPanel.vue";
import { openPanel } from "./lib/providerPanel";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { importBook, listBooks, pickNovelFile } from "./lib/ipc";

const books = ref<Book[]>([]);
const activeBookId = ref<number | null>(null);
const importing = ref(false);
const error = ref("");
const extraction = ref<Record<number, ExtractionStatus>>({});
let pollTimer: ReturnType<typeof setInterval> | null = null;

async function refreshExtraction(): Promise<void> {
  try {
    const list = await tauriInvoke<ExtractionStatus[]>("get_extraction_statuses");
    extraction.value = Object.fromEntries(list.map((s) => [s.book_id, s]));
  } catch {
    /* 非 Tauri 环境忽略 */
  }
}

async function onImport(): Promise<void> {
  const path = await pickNovelFile();
  if (path === null) return;
  importing.value = true;
  error.value = "";
  try {
    await importBook(path);
    books.value = await listBooks();
    await refreshExtraction();
  } catch (e) {
    error.value = String(e);
  } finally {
    importing.value = false;
  }
}

function openBook(book: Book): void {
  activeBookId.value = book.id;
}

onMounted(async () => {
  try {
    books.value = await listBooks();
  } catch {
    books.value = []; // 非 Tauri 环境（纯浏览器 dev）静默空书架
  }
  await refreshExtraction();
  pollTimer = setInterval(() => void refreshExtraction(), 3000);
});

onUnmounted(() => {
  if (pollTimer !== null) clearInterval(pollTimer);
});
</script>

<template>
  <ReaderView v-if="activeBookId !== null" :book-id="activeBookId" @back="activeBookId = null" />
  <main v-else class="bookshelf">
    <header class="shelf-header">
      <h1>RimeNovel</h1>
      <div class="header-ops">
        <button class="ghost" @click="openPanel()">⚙ LLM Provider</button>
        <button :disabled="importing" @click="onImport">
          {{ importing ? "导入中…" : "导入小说 (txt/epub)" }}
        </button>
      </div>
    </header>
    <p v-if="error" class="error">{{ error }}</p>
    <ul v-if="books.length" class="book-list">
      <li v-for="b in books" :key="b.id" class="book-card" @click="openBook(b)">
        <span class="title">{{ b.title }}</span>
        <span v-if="extraction[b.id]" class="extract-state" :data-state="extraction[b.id].state">
          {{ extraction[b.id].state === "done"
            ? "图谱就绪"
            : `${extraction[b.id].state} ${Math.round(extraction[b.id].progress * 100)}%` }}
        </span>
        <span class="meta">{{ b.author ?? "佚名" }} · {{ b.format }}</span>
      </li>
    </ul>
    <p v-else class="empty">书架为空，导入一本小说开始阅读。</p>
  </main>
  <ProviderPanel />
</template>

<style scoped>
.bookshelf { max-width: 720px; margin: 0 auto; padding: 24px; }
.shelf-header { display: flex; justify-content: space-between; align-items: center; }
.header-ops { display: flex; gap: 8px; }
.ghost { background: none; border: 1px solid #8886; }
.book-list { list-style: none; padding: 0; display: grid; gap: 12px; }
.book-card { border: 1px solid #8884; border-radius: 8px; padding: 12px 16px; display: flex; justify-content: space-between; gap: 8px; cursor: pointer; align-items: center; }
.book-card:hover { border-color: #888c; }
.title { font-weight: 600; }
.meta { color: #888; font-size: 0.9em; }
.extract-state { font-size: 0.78em; padding: 2px 8px; border-radius: 999px; background: #8882; }
.extract-state[data-state="done"] { background: seagreen3; color: #fff; }
.extract-state[data-state="running"], .extract-state[data-state="queued"] { background: steelblue3; color: #fff; }
.extract-state[data-state="failed"] { background: crimson; color: #fff; }
.empty { color: #888; }
.error { color: crimson; }
</style>
