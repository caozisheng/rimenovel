<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NAlert, NButton, NEmpty, NForm, NFormItem, NInput, NInputNumber, NModal, NSelect, type FormInst } from "naive-ui";
import { closePanel, deleteProvider, editProvider, providerPanel, resetForm, saveProvider, testProvider } from "../lib/providerPanel";

const form = ref<FormInst | null>(null);
const protocolOptions = [
  { label: "OpenAI 兼容", value: "openai" },
  { label: "Anthropic", value: "anthropic" },
];
const rules = computed(() => ({
  name: { required: true, message: "请输入名称", trigger: ["input", "blur"] },
  base_url: { required: true, message: "请输入 Base URL", trigger: ["input", "blur"] },
  api_key: { required: providerPanel.form.id === null, message: "请输入 API Key", trigger: ["input", "blur"] },
}));
watch(() => providerPanel.form.id, () => form.value?.restoreValidation());
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
    <NForm ref="form" :model="providerPanel.form" :rules="rules" :disabled="providerPanel.saving" @submit.prevent="submit">
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
        <NInput v-model:value="providerPanel.form.api_key" type="password" show-password-on="click" :placeholder="providerPanel.form.id === null ? 'sk-...' : '留空保留已有密钥；输入新密钥则替换'" :input-props="{ autocomplete: 'off' }" />
      </NFormItem>
      <NFormItem label="联网检索" path="tavily_enabled">
        <label class="toggle"><input v-model="providerPanel.form.tavily_enabled" type="checkbox" /> 默认使用 DuckDuckGo，免费、无需配置</label>
      </NFormItem>
      <NFormItem v-if="providerPanel.form.tavily_enabled || providerPanel.form.tavily_api_key" label="Tavily API Key（可选增强）" path="tavily_api_key">
        <div>
          <NInput v-model:value="providerPanel.form.tavily_api_key" type="password" show-password-on="click" :placeholder="providerPanel.form.id === null ? '留空仅使用 DDG' : '编辑时留空保留已有 Key'" :input-props="{ autocomplete: 'off' }" />
          <p class="hint">可选增强：正文质量与稳定性更好；未保存 Key 时留空仅使用 DDG，编辑时留空保留已有 Key。</p>
        </div>
      </NFormItem>
      <div class="row three">
        <NFormItem label="抽取模型"><NInput v-model:value="providerPanel.form.model_extract" placeholder="低价档" /></NFormItem>
        <NFormItem label="合并模型"><NInput v-model:value="providerPanel.form.model_merge" placeholder="中价档" /></NFormItem>
        <NFormItem label="写作模型"><NInput v-model:value="providerPanel.form.model_write" placeholder="高质档" /></NFormItem>
      </div>
      <div class="row">
        <NFormItem label="上下文上限"><NInputNumber v-model:value="providerPanel.form.context_limit" :min="1" placeholder="32768" /></NFormItem>
        <NFormItem label="输出预留"><NInputNumber v-model:value="providerPanel.form.output_reserve" :min="1" placeholder="4096" /></NFormItem>
      </div>
      <div class="item-ops">
        <NButton attr-type="submit" type="primary" :loading="providerPanel.saving" :disabled="providerPanel.saving">{{ providerPanel.form.id === null ? '保存' : '保存更改' }}</NButton>
        <NButton v-if="providerPanel.form.id !== null" :disabled="providerPanel.saving" @click="resetForm">取消编辑</NButton>
      </div>
    </NForm>
    <ul v-if="providerPanel.rows.length" class="list">
      <li v-for="p in providerPanel.rows" :key="p.id" class="item">
        <div class="item-main">
          <strong>{{ p.name }}</strong>
          <span class="meta">{{ p.protocol }} · {{ p.base_url }}</span>
          <span class="meta">抽取 {{ p.model_extract ?? '—' }} / 合并 {{ p.model_merge ?? '—' }} / 写作 {{ p.model_write ?? '—' }}</span>
          <span class="meta">联网检索 {{ p.tavily_enabled ? (p.has_tavily_key ? 'DDG + Tavily 可选增强' : '仅 DDG') : '未启用' }}</span>
          <span class="meta">预算 {{ p.context_limit ?? 32768 }} tokens · 预留 {{ p.output_reserve ?? 4096 }}</span>
        </div>
        <div class="item-ops">
          <NButton size="small" :disabled="providerPanel.saving" @click="editProvider(p)">编辑</NButton>
          <NButton size="small" :disabled="providerPanel.testing" @click="testProvider(p.id)">测连</NButton>
          <NButton size="small" type="error" secondary :disabled="providerPanel.saving" @click="deleteProvider(p.id)">删除</NButton>
        </div>
      </li>
    </ul>
    <NEmpty v-else description="还没有 Provider，先添加一个" class="empty" />
    <NAlert v-if="providerPanel.testResult" :type="providerPanel.testResult === '连接成功' ? 'success' : 'error'" class="notice">{{ providerPanel.testResult }}</NAlert>
    <p class="hint">DDG 和 Tavily 只接收书名、作者和章节标题；已配置的 LLM Provider 将接收本机首个实质章节与候选网页各自的一段正文来判别版别。判断不明确或章节来源不足时回退全文速读；原文不发给搜索引擎。</p>
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
