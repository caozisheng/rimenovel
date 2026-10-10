// 主题三态切换（dark|light|auto）——UI 与 CSS 令牌的单一桥梁
export type ThemeMode = "dark" | "light" | "auto";

export function applyTheme(mode: ThemeMode): void {
  if (mode === "auto") {
    delete document.documentElement.dataset.theme;
  } else {
    document.documentElement.dataset.theme = mode;
  }
  localStorage.setItem("rn-theme", mode);
}

export function currentTheme(): ThemeMode {
  const v = localStorage.getItem("rn-theme");
  return v === "dark" || v === "light" ? v : "auto";
}
