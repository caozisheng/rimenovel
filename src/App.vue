<script setup lang="ts">
// RimeNovel 书架（P2 雏形：导入 + 列表）
import { onMounted, ref } from "vue";
import type { Book } from "./lib/types";
import { importBook, listBooks, pickNovelFile } from "./lib/ipc";

const books = ref<Book[]>([]);
const importing = ref(false);
const error = ref("");

async function onImport(): Promise<void> {
  const path = await pickNovelFile();
  if (path === null) return;
  importing.value = true;
  error.value = "";
  try {
    await importBook(path);
    books.value = await listBooks();
  } catch (e) {
    error.value = String(e);
  } finally {
    importing.value = false;
  }
}

onMounted(async () => {
  try {
    books.value = await listBooks();
  } catch {
    books.value = []; // 非 Tauri 环境（纯浏览器 dev）静默空书架
  }
});
</script>

<template>
  <main class="bookshelf">
    <header class="shelf-header">
      <h1>RimeNovel</h1>
      <button :disabled="importing" @click="onImport">
        {{ importing ? "导入中…" : "导入小说 (txt/epub)" }}
      </button>
    </header>
    <p v-if="error" class="error">{{ error }}</p>
    <ul v-if="books.length" class="book-list">
      <li v-for="b in books" :key="b.id" class="book-card">
        <span class="title">{{ b.title }}</span>
        <span class="meta">{{ b.author ?? "佚名" }} · {{ b.format }}</span>
      </li>
    </ul>
    <p v-else class="empty">书架为空，导入一本小说开始阅读。</p>
  </main>
</template>

<style scoped>
.bookshelf { max-width: 720px; margin: 0 auto; padding: 24px; }
.shelf-header { display: flex; justify-content: space-between; align-items: center; }
.book-list { list-style: none; padding: 0; display: grid; gap: 12px; }
.book-card { border: 1px solid #8884; border-radius: 8px; padding: 12px 16px; display: flex; justify-content: space-between; }
.title { font-weight: 600; }
.meta { color: #888; font-size: 0.9em; }
.empty { color: #888; }
.error { color: crimson; }
</style>
