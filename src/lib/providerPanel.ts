// Provider 管理面板状态（dev-plan Task 3.5 UI）
import { reactive } from "vue";
import type { ProviderRow } from "./types";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";

const emptyForm = {
  name: "",
  protocol: "openai",
  base_url: "",
  model_extract: "",
  model_write: "",
  model_merge: "",
  api_key: "",
};

export const providerPanel = reactive({
  visible: false,
  rows: [] as ProviderRow[],
  form: { ...emptyForm },
  testing: false,
  testResult: "",
  saving: false,
});

export function resetForm(): void {
  providerPanel.form = { ...emptyForm };
  providerPanel.testResult = "";
}

async function refreshProviders(): Promise<void> {
  providerPanel.rows = await tauriInvoke<ProviderRow[]>("provider_list");
}

export async function saveProvider(): Promise<void> {
  providerPanel.saving = true;
  try {
    await tauriInvoke("provider_save", {
      input: {
        ...providerPanel.form,
        model_extract: providerPanel.form.model_extract || null,
        model_write: providerPanel.form.model_write || null,
        model_merge: providerPanel.form.model_merge || null,
      },
    });
    resetForm();
    await refreshProviders();
  } finally {
    providerPanel.saving = false;
  }
}

export async function testProvider(id: number): Promise<void> {
  providerPanel.testing = true;
  providerPanel.testResult = "";
  try {
    providerPanel.testResult = await tauriInvoke<string>("provider_test", { id });
  } catch (e) {
    providerPanel.testResult = String(e);
  } finally {
    providerPanel.testing = false;
  }
}

export async function deleteProvider(id: number): Promise<void> {
  await tauriInvoke("provider_delete", { id });
  await refreshProviders();
}

export async function openPanel(): Promise<void> {
  providerPanel.visible = true;
  await refreshProviders();
}

export function closePanel(): void {
  providerPanel.visible = false;
}
