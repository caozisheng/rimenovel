import { beforeEach, describe, expect, it } from "vitest";
import { readingMode, saveReadingMode } from "./readingMode";

describe("per-book reading mode", () => {
  beforeEach(() => localStorage.clear());

  it("defaults to original and persists rewrite independently per book", () => {
    expect(readingMode(1)).toBe("original");
    saveReadingMode(1, "rewrite");
    saveReadingMode(2, "original");
    expect(readingMode(1)).toBe("rewrite");
    expect(readingMode(2)).toBe("original");
  });

  it("rejects invalid stored values by falling back to original", () => {
    localStorage.setItem("rn-reading-mode-1", "unknown");
    expect(readingMode(1)).toBe("original");
  });
});
