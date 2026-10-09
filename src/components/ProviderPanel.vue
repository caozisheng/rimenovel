<script setup lang="ts">
// Provider 管理面板（dev-plan Task 3.5 UI）
import {
  closePanel,
  deleteProvider,
  providerPanel,
  saveProvider,
  testProvider,
} from "../lib/providerPanel";
</script>

<template>
  <div v-if="providerPanel.visible" class="panel-mask" @click.self="closePanel">
    <section class="panel">
      <header class="panel-head">
        <h2>LLM Provider</h2>
        <button @click="closePanel">✕</button>
      </header>

      <form class="form" @submit.prevent="saveProvider">
        <div class="row">
          <label>名称 <input v-model="providerPanel.form.name" required placeholder="my-openrouter" /></label>
          <label>
            协议
            <select v-model="providerPanel.form.protocol">
              <option value="openai">OpenAI 兼容</option>
              <option value="anthropic">Anthropic</option>
            </select>
          </label>
        </div>
        <label>Base URL <input v-model="providerPanel.form.base_url" required placeholder="https://api.example.com/v1" /></label>
        <label>API Key <input v-model="providerPanel.form.api_key" type="password" required placeholder="sk-..." /></label>
        <div class="row three">
          <label>抽取模型 <input v-model="providerPanel.form.model_extract" placeholder="低价档" /></label>
          <label>合并模型 <input v-model="providerPanel.form.model_merge" placeholder="中价档" /></label>
          <label>写作模型 <input v-model="providerPanel.form.model_write" placeholder="高质档" /></label>
        </div>
        <button type="submit" :disabled="providerPanel.saving">
          {{ providerPanel.saving ? "保存中…" : "保存" }}
        </button>
      </form>

      <ul v-if="providerPanel.rows.length" class="list">
        <li v-for="p in providerPanel.rows" :key="p.id" class="item">
          <div class="item-main">
            <strong>{{ p.name }}</strong>
            <span class="meta">{{ p.protocol }} · {{ p.base_url }}</span>
            <span class="meta">抽取 {{ p.model_extract ?? "—" }} / 写作 {{ p.model_write ?? "—" }}</span>
          </div>
          <div class="item-ops">
            <button :disabled="providerPanel.testing" @click="testProvider(p.id)">测连</button>
            <button class="danger" @click="deleteProvider(p.id)">删除</button>
          </div>
        </li>
      </ul>
      <p v-else class="empty">还没有 provider，先添加一个。</p>

      <p v-if="providerPanel.testResult" class="test-result" :class="{ ok: providerPanel.testResult === '连接成功' }">
        {{ providerPanel.testResult }}
      </p>
      <p class="hint">未填写的模型档位将在调用时报「未配置」错误。</p>
    </section>
  </div>
</template>

<style scoped>
.panel-mask { position: fixed; inset: 0; background: #0008; display: flex; align-items: center; justify-content: center; z-index: 50; }
.panel { background: var(--vv-bg, #fff); border-radius: 12px; width: min(560px, 92vw); max-height: 86vh; overflow: auto; padding: 20px 24px; }
.panel-head { display: flex; justify-content: space-between; align-items: center; }
.form { display: grid; gap: 10px; margin: 12px 0 20px; }
.form label { display: grid; gap: 4px; font-size: 0.9em; color: #666; }
.row { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
.row.three { grid-template-columns: 1fr 1fr 1fr; }
input, select { padding: 6px 8px; border: 1px solid #8886; border-radius: 6px; font-size: 0.95em; }
.list { list-style: none; padding: 0; display: grid; gap: 8px; }
.item { border: 1px solid #8884; border-radius: 8px; padding: 10px 12px; display: flex; justify-content: space-between; gap: 8px; }
.item-main { display: grid; gap: 2px; }
.meta { color: #888; font-size: 0.82em; }
.item-ops { display: flex; gap: 6px; align-items: center; }
.danger { color: crimson; }
.empty { color: #888; }
.test-result { font-weight: 600; }
.test-result.ok { color: seagreen; }
.hint { color: #aaa; font-size: 0.8em; }
</style>
