import { beforeEach, describe, expect, it, vi } from "vitest";
import { mount } from "@vue/test-utils";
import { invoke } from "@tauri-apps/api/core";
import ProviderPanel from "../components/ProviderPanel.vue";
import { editProvider, providerPanel, resetForm, saveProvider } from "./providerPanel";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const stored = { id: 4, name: "gateway", protocol: "openai", base_url: "https://old.example/v1", model_extract: "extract", model_write: "write", model_merge: "merge", context_limit: 65536, output_reserve: 8192, tavily_enabled:true, has_tavily_key:true };
beforeEach(() => { resetForm(); providerPanel.rows = [{ ...stored }]; });

describe("editing stored Providers", () => {
  it("keeps credentials out of the draft and isolates unsaved changes", () => {
    providerPanel.form.api_key = "previous-unsaved-secret";
    editProvider(providerPanel.rows[0]);
    expect(providerPanel.form.api_key).toBe("");
    expect(providerPanel.form.tavily_api_key).toBe("");
    expect(providerPanel.form.tavily_enabled).toBe(true);
    providerPanel.form.name = "renamed";
    providerPanel.form.base_url = "https://new.example/v1";
    expect(providerPanel.rows[0].name).toBe("gateway");
    expect(providerPanel.rows[0].base_url).toBe("https://old.example/v1");
  });
  it("cancelling edit clears the update target and draft secret", () => {
    editProvider(providerPanel.rows[0]);
    providerPanel.form.api_key = "new-unsaved-secret";
    resetForm();
    expect(providerPanel.form.id).toBeNull();
    expect(providerPanel.form.api_key).toBe("");
    expect(providerPanel.form.tavily_api_key).toBe("");
    expect(providerPanel.form.tavily_enabled).toBe(false);
    expect(providerPanel.form.name).toBe("");
    expect(providerPanel.rows[0].id).toBe(stored.id);
  });
});

describe("optional enhanced online search", () => {
  it("saves enabled search with no Tavily key", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(5).mockResolvedValueOnce([]);
    providerPanel.form.tavily_enabled = true;
    await saveProvider();
    expect(invoke).toHaveBeenCalledWith("provider_save", {
      input: expect.objectContaining({ tavily_enabled: true, tavily_api_key: "" }),
    });
    expect(providerPanel.error).toBe("");
  });

  it("submits a blank edit key so the backend preserves the stored enhancement", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(4).mockResolvedValueOnce([]);
    editProvider(stored);
    await saveProvider();
    expect(invoke).toHaveBeenCalledWith("provider_save", {
      input: expect.objectContaining({ id: 4, tavily_enabled: true, tavily_api_key: "" }),
    });
  });

  it("describes free default search and optional Tavily enhancement without a missing-key error", () => {
    providerPanel.visible = true;
    editProvider({ ...stored, has_tavily_key: false });
    providerPanel.rows = [{ ...stored, has_tavily_key: false }];
    const wrapper = mount(ProviderPanel, { attachTo: document.body });
    try {
      const text = document.body.textContent ?? "";
      expect(text).toContain("联网检索");
      expect(text).toContain("DuckDuckGo");
      expect(text).toContain("免费、无需配置");
      expect(text).toContain("Tavily API Key（可选增强）");
      expect(text).toContain("正文质量与稳定性更好");
      expect(text).toContain("留空仅使用 DDG");
      expect(text).toContain("编辑时留空保留已有 Key");
      expect(text).not.toContain("缺 Key");
    } finally {
      wrapper.unmount();
      providerPanel.visible = false;
    }
  });
});
