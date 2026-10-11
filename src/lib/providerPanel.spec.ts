import { beforeEach, describe, expect, it, vi } from "vitest";
import { editProvider, providerPanel, resetForm } from "./providerPanel";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const stored = { id: 4, name: "gateway", protocol: "openai", base_url: "https://old.example/v1", model_extract: "extract", model_write: "write", model_merge: "merge", context_limit: 65536, output_reserve: 8192 };
beforeEach(() => { resetForm(); providerPanel.rows = [{ ...stored }]; });

describe("editing stored Providers", () => {
  it("keeps credentials out of the draft and isolates unsaved changes", () => {
    providerPanel.form.api_key = "previous-unsaved-secret";
    editProvider(providerPanel.rows[0]);
    expect(providerPanel.form.api_key).toBe("");
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
    expect(providerPanel.form.name).toBe("");
    expect(providerPanel.rows[0].id).toBe(stored.id);
  });
});
