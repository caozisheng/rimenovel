<script setup lang="ts">
import { computed } from "vue";
import { NAlert, NButton, NTabPane, NTabs, NTag } from "naive-ui";
import StoryMap from "./StoryMap.vue";
import type { WikiEntryKind, WikiView } from "../lib/wiki";

const props = withDefaults(defineProps<{
  view: WikiView | null;
  loading?: boolean;
  error?: string | null;
  canJump?: boolean;
}>(), { loading: false, error: null, canJump: false });
const emit = defineEmits<{ jump: [chapterIdx: number] }>();
const kindLabels: Record<WikiEntryKind, string> = { character: "人物", faction: "势力", location: "地点", item: "物品", setting: "设定", plot: "情节" };
const entryNames = computed(() => Object.fromEntries((props.view?.entries ?? []).map(entry => [entry.id, entry.name])));
</script>

<template>
  <section class="wiki-workbench" aria-label="只读 Wiki 工作台">
    <header>
      <h2>Wiki / StoryMap</h2>
      <NTag>只读</NTag>
    </header>
    <p class="muted">整理已公开的故事信息。下一章的意向通过问题选择或文字补充提出，不在这里编辑条目。</p>
    <NAlert v-if="error" type="error" :show-icon="false">{{ error }}</NAlert>
    <p v-if="loading" role="status">正在读取 Wiki…</p>
    <p v-else-if="!view" class="muted">暂无 Wiki 数据。</p>
    <template v-if="view">
      <p v-if="!view.ready" class="muted" role="status">Wiki 尚在整理中；以下展示已公开的部分。</p>
      <NTabs type="line" animated>
        <NTabPane name="wiki" tab="Wiki">
          <p v-if="!view.entries.length" class="muted">暂无已公开的 Wiki 条目。</p>
          <div v-else class="entries">
            <article v-for="entry in view.entries" :key="entry.id">
              <header>
                <h3>{{ entry.name }}</h3>
                <NTag size="small">{{ kindLabels[entry.kind] }}</NTag>
              </header>
              <p v-if="entry.aliases.length" class="muted">别名：{{ entry.aliases.join('、') }}</p>
              <p v-if="entry.summary" class="summary">{{ entry.summary }}</p>
              <section v-if="entry.state_items.length" class="entry-section">
                <h4>当前已知状态</h4>
                <ul><li v-for="(item, index) in entry.state_items" :key="index">{{ item }}</li></ul>
              </section>
              <p v-if="entry.related_entries.length" class="muted">关联条目：{{ entry.related_entries.map(id => entryNames[id] ?? '未公开条目').join('、') }}</p>
              <section v-if="entry.open_threads.length" class="entry-section">
                <h4>未解线索</h4>
                <ul><li v-for="(thread, index) in entry.open_threads" :key="index">{{ thread }}</li></ul>
              </section>
              <div v-if="entry.source_chapters.length" class="sources">
                <span class="muted">来源章节：</span>
                <template v-for="chapter in entry.source_chapters" :key="chapter">
                  <NButton v-if="canJump && view.coverage.visited.includes(chapter)" text type="primary" @click="emit('jump', chapter)">第 {{ chapter }} 章</NButton>
                  <span v-else class="muted">第 {{ chapter }} 章</span>
                </template>
              </div>
            </article>
          </div>
        </NTabPane>
        <NTabPane name="story-map" tab="StoryMap">
          <StoryMap :view="view" :can-jump="canJump" @jump="emit('jump', $event)" />
        </NTabPane>
      </NTabs>
    </template>
  </section>
</template>

<style scoped>
.wiki-workbench { color: var(--color-text-primary); }
header { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
h2, h3 { margin: 0; }
h2 { font-size: 20px; }
.muted { color: var(--color-text-secondary); font-size: 13px; line-height: 1.6; }
.entries { display: grid; gap: 12px; }
article { min-width: 0; padding: 16px; border: 1px solid var(--color-border); border-radius: 6px; overflow-wrap: anywhere; }
.summary { white-space: pre-wrap; line-height: 1.7; }
.entry-section h4 { margin-bottom: 4px; font-size: 14px; }
.entry-section ul { margin-top: 4px; padding-left: 20px; }
.entry-section li { margin: 4px 0; }
.sources { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; }
</style>
