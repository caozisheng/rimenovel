import type { ReadingMode } from "./types";

function key(bookId: number): string { return `rn-reading-mode-${bookId}`; }

export function readingMode(bookId: number): ReadingMode {
  return localStorage.getItem(key(bookId)) === "rewrite" ? "rewrite" : "original";
}

export function saveReadingMode(bookId: number, mode: ReadingMode): void {
  localStorage.setItem(key(bookId), mode);
}
export function readingChapter(bookId: number, mode: ReadingMode): number {
  const value = Number.parseInt(localStorage.getItem(`rn-reading-chapter-${bookId}-${mode}`) ?? "1", 10);
  return Number.isSafeInteger(value) && value >= 1 ? value : 1;
}

export function saveReadingChapter(bookId: number, mode: ReadingMode, chapterIdx: number): void {
  if (Number.isSafeInteger(chapterIdx) && chapterIdx >= 1) localStorage.setItem(`rn-reading-chapter-${bookId}-${mode}`, String(chapterIdx));
}
