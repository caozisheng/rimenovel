<script setup lang="ts">
import { computed } from "vue";
import { NButton, NTag } from "naive-ui";
import type { StoryMapView } from "../lib/wiki";

const props = withDefaults(defineProps<{ view: StoryMapView; canJump?: boolean }>(), { canJump: false });
const emit = defineEmits<{ jump: [chapterIdx: number] }>();
const cards = computed(() => [...props.view.chapter_cards].sort((left, right) => left.chapter_idx - right.chapter_idx));
</script>

<template>
  <section class="story-map" aria-label="StoryMap 章节地图">
    <h3>StoryMap</h3>
    <p class="muted">按章节浏览已公开的故事线索。此处仅供阅读，不支持修改。</p>
    <div class="coverage">
      <NTag>已访问 {{ view.coverage.visited.length }} 章</NTag>
      <NTag type="success">Wiki 已就绪 {{ view.coverage.ready.length }} 章</NTag>
      <NTag v-if="view.coverage.missing.length" type="warning">待整理 {{ view.coverage.missing.length }} 章</NTag>
    </div>
    <p v-if="!cards.length" class="muted">暂无已公开的章节卡片。</p>
    <ol v-else>
      <li v-for="card in cards" :key="card.chapter_idx">
        <header>
          <h4>第 {{ card.chapter_idx }} 章 · {{ card.title }}</h4>
          <NTag v-if="!card.ready" size="small" type="warning">Wiki 待整理</NTag>
        </header>
        <p v-if="card.headline" class="headline">{{ card.headline }}</p>
        <p v-if="card.summary" class="summary">{{ card.summary }}</p>
        <p v-if="!card.headline && !card.summary" class="muted">暂无公开摘要。</p>
        <small v-if="card.entry_ids.length" class="muted">关联 {{ card.entry_ids.length }} 个 Wiki 条目</small>
        <NButton v-if="canJump && view.coverage.visited.includes(card.chapter_idx)" text type="primary" @click="emit('jump', card.chapter_idx)">阅读此章</NButton>
      </li>
    </ol>
  </section>
</template>

<style scoped>
.story-map h3 { margin: 0 0 8px; }
.muted { color: var(--color-text-secondary); font-size: 13px; }
.coverage { display: flex; flex-wrap: wrap; gap: 8px; margin: 12px 0; }
ol { list-style: none; margin: 0; padding: 0; display: grid; gap: 12px; }
li { padding: 16px; border: 1px solid var(--color-border); border-left: 3px solid var(--color-accent); border-radius: 6px; }
header { display: flex; align-items: baseline; flex-wrap: wrap; gap: 8px; }
h4 { margin: 0; }
.headline { font-weight: 600; }
.summary { white-space: pre-wrap; overflow-wrap: anywhere; line-height: 1.7; }
li :deep(.n-button) { display: block; margin-top: 8px; }
</style>
