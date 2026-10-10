<script setup lang="ts">
// RimeNovel 入口：书架 ↔ 阅读器；Naive UI 主题联动 CSS 令牌
import { computed, onMounted, onUnmounted, ref } from "vue";
import type { Book, ExtractionStatus } from "./lib/types";
import ReaderView from "./components/ReaderView.vue";
import ProviderPanel from "./components/ProviderPanel.vue";
import { openPanel } from "./lib/providerPanel";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { deleteBook, importBook, listBooks, pickNovelFile } from "./lib/ipc";
import { applyTheme, currentTheme, type ThemeMode } from "./lib/theme";
import {
  NButton,
  NConfigProvider,
  NEmpty,
  NModal,
  NSelect,
  NSpace,
  NTag,
  darkTheme,
  zhCN,
  dateZhCN,
} from "naive-ui";

const books = ref<Book[]>([]);
const activeBookId = ref<number | null>(null);
const importing = ref(false);
const error = ref("");

const deletingBookId = ref<number | null>(null);
const pendingDeleteBook = ref<Book | null>(null);
const showDeleteDialog = ref(false);
const extraction = ref<Record<number, ExtractionStatus>>({});
const themeMode = ref<ThemeMode>(currentTheme());
const colorScheme = window.matchMedia("(prefers-color-scheme: dark)");
const systemDark = ref(colorScheme.matches);
function onSystemThemeChange(event: MediaQueryListEvent): void {
  systemDark.value = event.matches;
}
colorScheme.addEventListener("change", onSystemThemeChange);
let pollTimer: ReturnType<typeof setInterval> | null = null;

// Naive UI 主题与 CSS 令牌同步: dark 显式, auto 跟随系统
const naiveTheme = computed(() =>
  themeMode.value === "dark" ||
  (themeMode.value === "auto" && systemDark.value)
    ? darkTheme
    : undefined,
);
const themeOverrides = computed(() => ({
  common: {
    primaryColor: naiveTheme.value ? "#818cf8" : "#6366f1",
    primaryColorHover: naiveTheme.value ? "#a5b4fc" : "#818cf8",
    primaryColorPressed: naiveTheme.value ? "#6366f1" : "#4f46e5",
    borderRadius: "6px",
  },
}));

const themeOptions = [
  { label: "☀ 亮色", value: "light" },
  { label: "☾ 暗色", value: "dark" },
  { label: "◐ 自动", value: "auto" },
];

function onThemeChange(mode: ThemeMode): void {
  themeMode.value = mode;
  applyTheme(mode);
}

async function refreshExtraction(): Promise<void> {
  try {
    const list = await tauriInvoke<ExtractionStatus[]>("get_extraction_statuses");
    extraction.value = Object.fromEntries(list.map((s) => [s.book_id, s]));
  } catch {
    /* 非 Tauri 环境忽略 */
  }
}

async function onImport(): Promise<void> {
  importing.value = true;
  error.value = "";
  try {
    const path = await pickNovelFile();
    if (path === null) return;
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

function requestDelete(book: Book): void {
  error.value = "";
  pendingDeleteBook.value = book;
  showDeleteDialog.value = true;
}

async function confirmDelete(): Promise<void> {
  const book = pendingDeleteBook.value;
  if (!book || deletingBookId.value !== null) return;
  deletingBookId.value = book.id;
  error.value = "";
  try {
    await deleteBook(book.id);
    books.value = books.value.filter((item) => item.id !== book.id);
    const nextExtraction = { ...extraction.value };
    delete nextExtraction[book.id];
    extraction.value = nextExtraction;
    showDeleteDialog.value = false;
    pendingDeleteBook.value = null;
  } catch (e) {
    error.value = String(e);
  } finally {
    deletingBookId.value = null;
  }
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
  colorScheme.removeEventListener("change", onSystemThemeChange);
});
</script>

<template>
  <NConfigProvider :theme="naiveTheme" :theme-overrides="themeOverrides" :locale="zhCN" :date-locale="dateZhCN">
    <ReaderView v-if="activeBookId !== null" :book-id="activeBookId" @back="activeBookId = null" />
    <main v-else class="bookshelf">
      <header class="shelf-header">
        <img class="brand-icon" src="/rimenovel-icon.png" alt="" width="128" height="128" />
        <div class="brand-content">
          <h1>RimeNovel</h1>
          <NSpace :size="8" align="center">
            <NSelect
              aria-label="界面主题"
              :value="themeMode"
              :options="themeOptions"
              size="small"
              style="width: 108px"
              @update:value="onThemeChange"
            />
            <NButton size="small" quaternary @click="openPanel()">⚙ Provider</NButton>
            <NButton size="small" type="primary" :loading="importing" @click="onImport">
              {{ importing ? "导入中…" : "导入小说" }}
            </NButton>
          </NSpace>
        </div>
      </header>
      <p v-if="error && !showDeleteDialog" class="error">{{ error }}</p>
      <ul v-if="books.length" class="book-list">
        <li v-for="b in books" :key="b.id" class="book-card">
          <div class="card-main">
            <button class="book-title" @click="openBook(b)">{{ b.title }}</button>
            <span class="meta">{{ b.author ?? "佚名" }} · {{ b.format }}</span>
          </div>
          <div class="card-actions">
            <NTag
              v-if="extraction[b.id]"
              size="small"
              :type="
                extraction[b.id].state === 'done' ? 'success'
                : extraction[b.id].state === 'failed' ? 'error'
                : 'info'"
              round
            >
              {{ extraction[b.id].state === "done"
                ? "图谱就绪"
                : `${extraction[b.id].state} ${Math.round(extraction[b.id].progress * 100)}%` }}
            </NTag>
            <NButton
              size="small"
              quaternary
              circle
              type="error"
              :loading="deletingBookId === b.id"
              :aria-label="`删除《${b.title}》`"
              title="删除"
              @click="requestDelete(b)"
            >
              <svg viewBox="0 0 24 24" aria-hidden="true" class="delete-icon">
                <path d="M4 7h16M10 11v6m4-6v6M6 7l1 13h10l1-13M9 7V4h6v3" />
              </svg>
            </NButton>
          </div>
        </li>
      </ul>
      <NEmpty v-else description="书架为空，导入一本小说开始阅读" class="empty" />
    </main>
    <NModal
      v-model:show="showDeleteDialog"
      preset="dialog"
      title="删除书籍"
      type="warning"
      :mask-closable="deletingBookId === null"
      :close-on-esc="deletingBookId === null"
      :closable="deletingBookId === null"
    >
      <p>确定删除《{{ pendingDeleteBook?.title }}》？</p>
      <p>章节、图谱及阅读数据将一并删除，原始 TXT/EPUB 文件不会删除。</p>
      <p v-if="error" class="error" role="alert">{{ error }}</p>
      <template #action>
        <NSpace :size="8">
          <NButton :disabled="deletingBookId !== null" @click="showDeleteDialog = false">取消</NButton>
          <NButton type="error" :loading="deletingBookId !== null" @click="confirmDelete">确认删除</NButton>
        </NSpace>
      </template>
    </NModal>
    <ProviderPanel />
  </NConfigProvider>
</template>

<style scoped>
.bookshelf { max-width: 720px; margin: 0 auto; padding: var(--space-lg); }
.shelf-header { display: flex; gap: var(--space-md); align-items: center; margin-bottom: var(--space-md); }
.brand-content { display: flex; flex-direction: column; gap: var(--space-sm); min-width: 0; }
.brand-content h1 { margin: 0; }
.brand-icon { display: block; flex-shrink: 0; object-fit: contain; border-radius: var(--radius-sm); }
.book-list { list-style: none; padding: 0; display: grid; gap: 12px; }
.book-card {
  position: relative;
  border: 1px solid var(--color-border);
  background: var(--color-surface-raised);
  border-radius: var(--radius-md);
  padding: 12px 16px;
  display: flex; justify-content: space-between; gap: var(--space-sm);
  align-items: center;
  transition: background var(--transition-fast), border-color var(--transition-fast);
}
.book-card:hover { border-color: var(--color-accent); background: var(--color-surface-hover); }
.card-main { display: grid; gap: 2px; min-width: 0; }
.card-actions { position: relative; z-index: 1; display: flex; align-items: center; gap: var(--space-sm); flex-shrink: 0; }
.delete-icon { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; }
.book-title { font: inherit; font-weight: 600; text-align: left; color: inherit; border: 0; background: none; padding: 0; cursor: pointer; overflow-wrap: anywhere; }
.book-title::after { content: ""; position: absolute; inset: 0; border-radius: var(--radius-md); }
.book-title:hover { color: var(--color-accent); }
.book-card:focus-within { outline: 2px solid var(--color-accent); outline-offset: 4px; }
.book-title:focus-visible { outline: none; }
.meta { color: var(--color-text-secondary); font-size: 0.9em; }
.empty { margin-top: var(--space-xl); }
.error { color: var(--color-danger); }
@media (max-width: 480px) {
  .bookshelf { padding: var(--space-md); }
  .shelf-header { gap: var(--space-sm); }
  .brand-content h1 { font-size: var(--font-size-xl); }
  .book-card { flex-wrap: wrap; }
}
</style>
