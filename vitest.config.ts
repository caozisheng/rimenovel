import { defineConfig } from "vitest/config";
import vue from "@vitejs/plugin-vue";

export default defineConfig({
  plugins: [vue()],
  test: {
    environment: "jsdom",
    include: ["src/**/*.spec.ts"],
    // 过渡期允许空测试集通过（Rust 侧已有 26 个测试守护核心逻辑）
    passWithNoTests: true,
  },
});
