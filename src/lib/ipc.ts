// 统一 Tauri IPC 封装（薄 API 层：稳定命令名 + 参数类型）
import type { Book, ChapterMeta, ChapterContent } from "./types";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

export async function importBook(path: string): Promise<number> {
  return tauriInvoke<number>("import_book", { path });
}

export async function listBooks(): Promise<Book[]> {
  return tauriInvoke<Book[]>("list_books_cmd");
}

export async function getChapterTitles(bookId: number): Promise<ChapterMeta[]> {
  return tauriInvoke<ChapterMeta[]>("get_chapter_titles", { bookId });
}

export async function getChapter(bookId: number, idx: number): Promise<ChapterContent> {
  return tauriInvoke<ChapterContent>("get_chapter_cmd", { bookId, idx });
}

/** 桌面端文件选择；用户取消返回 null */
export async function pickNovelFile(): Promise<string | null> {
  const sel = await open({
    multiple: false,
    filters: [{ name: "小说", extensions: ["txt", "epub"] }],
  });
  return typeof sel === "string" ? sel : null;
}
