<script setup lang="ts">
import { ref } from "vue";
import { NAlert, NButton, NEmpty, NForm, NFormItem, NInput, NModal, NSelect, type FormInst } from "naive-ui";
import { closePanel, deleteProvider, providerPanel, saveProvider, testProvider } from "../lib/providerPanel";

const form = ref<FormInst | null>(null);
const protocolOptions = [
  { label: "OpenAI 兼容", value: "openai" },
  { label: "Anthropic", value: "anthropic" },
];
const rules = {
  name: { required: true, message: "请输入名称", trigger: ["input", "blur"] },
  base_url: { required: true, message: "请输入 Base URL", trigger: ["input", "blur"] },
  api_key: { required: true, message: "请输入 API Key", trigger: ["input", "blur"] },
};
async function submit(): Promise<void> {
  try {
    await form.value?.validate();
  } catch {
    return;
  }
  await saveProvider();
}
</script>

<template>
  <NModal
    :show="providerPanel.visible"
    preset="card"
    title="LLM Provider"
    class="provider-modal"
    :bordered="false"
    :style="{ width: 'min(600px, calc(100vw - 32px))', maxHeight: 'calc(100dvh - 32px)', overflow: 'auto' }"
    @update:show="(show: boolean) => { if (!show) closePanel(); }"
    @close="closePanel"
  >
    <NAlert v-if="providerPanel.error" type="error" class="notice">{{ providerPanel.error }}</NAlert>
    <NForm ref="form" :model="providerPanel.form" :rules="rules" @submit.prevent="submit">
      <div class="row">
        <NFormItem label="名称" path="name">
          <NInput v-model:value="providerPanel.form.name" placeholder="my-openrouter" />
        </NFormItem>
        <NFormItem label="协议" path="protocol">
          <NSelect v-model:value="providerPanel.form.protocol" :options="protocolOptions" aria-label="协议" />
        </NFormItem>
      </div>
      <NFormItem label="Base URL" path="base_url">
        <NInput v-model:value="providerPanel.form.base_url" placeholder="https://api.example.com/v1" />
      </NFormItem>
      <NFormItem label="API Key" path="api_key">
        <NInput v-model:value="providerPanel.form.api_key" type="password" show-password-on="click" placeholder="sk-..." :input-props="{ autocomplete: 'off' }" />
      </NFormItem>
      <div class="row three">
        <NFormItem label="抽取模型"><NInput v-model:value="providerPanel.form.model_extract" placeholder="低价档" /></NFormItem>
        <NFormItem label="合并模型"><NInput v-model:value="providerPanel.form.model_merge" placeholder="中价档" /></NFormItem>
        <NFormItem label="写作模型"><NInput v-model:value="providerPanel.form.model_write" placeholder="高质档" /></NFormItem>
      </div>
      <NButton attr-type="submit" type="primary" :loading="providerPanel.saving" :disabled="providerPanel.saving">保存</NButton>
    </NForm>
    <ul v-if="providerPanel.rows.length" class="list">
      <li v-for="p in providerPanel.rows" :key="p.id" class="item">
        <div class="item-main">
          <strong>{{ p.name }}</strong>
          <span class="meta">{{ p.protocol }} · {{ p.base_url }}</span>
          <span class="meta">抽取 {{ p.model_extract ?? '—' }} / 合并 {{ p.model_merge ?? '—' }} / 写作 {{ p.model_write ?? '—' }}</span>
        </div>
        <div class="item-ops">
          <NButton size="small" :disabled="providerPanel.testing" @click="testProvider(p.id)">测连</NButton>
          <NButton size="small" type="error" secondary @click="deleteProvider(p.id)">删除</NButton>
        </div>
      </li>
    </ul>
    <NEmpty v-else description="还没有 Provider，先添加一个" class="empty" />
    <NAlert v-if="providerPanel.testResult" :type="providerPanel.testResult === '连接成功' ? 'success' : 'error'" class="notice">{{ providerPanel.testResult }}</NAlert>
    <p class="hint">未填写的模型档位将在调用时报「未配置」错误。</p>
  </NModal>
</template>

<style scoped>
.row { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: var(--space-sm); }
.row.three { grid-template-columns: repeat(3, minmax(0, 1fr)); }
.list { list-style: none; padding: 0; display: grid; gap: var(--space-sm); margin-top: var(--space-lg); }
.item { border: 1px solid var(--color-border); border-radius: var(--radius-md); padding: 12px; display: flex; justify-content: space-between; gap: var(--space-sm); }
.item-main { display: grid; gap: 2px; min-width: 0; overflow-wrap: anywhere; }
.meta { color: var(--color-text-secondary); font-size: 0.82em; }
.item-ops { display: flex; gap: var(--space-xs); align-items: center; flex-shrink: 0; }
.empty { margin: var(--space-lg) 0; }
.notice { margin-bottom: var(--space-md); }
.hint { color: var(--color-text-secondary); font-size: 0.8em; }
@media (max-width: 480px) {
  .row, .row.three { grid-template-columns: minmax(0, 1fr); gap: 0; }
  .item { flex-direction: column; }
}
</style>
