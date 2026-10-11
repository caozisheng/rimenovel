<script setup lang="ts">
import { computed } from "vue";
import { provenanceLabels, provenanceTokens, sortEvents, type GraphEvent, type GraphNode } from "../lib/graph";
import { useGraphSelectionStore } from "../lib/graphSelection";
const props = withDefaults(defineProps<{ events: GraphEvent[]; nodes: GraphNode[]; canJump?: boolean }>(), { canJump: false });
const emit = defineEmits<{ jump: [chapterIdx: number] }>();
const selection = useGraphSelectionStore();
const events = computed(() => sortEvents(props.events));
const names = computed(() => Object.fromEntries(props.nodes.map((node) => [node.key, node.name])));
</script>

<template>
  <section class="timeline" aria-label="事件时间线">
    <h3>事件时间线</h3>
    <p v-if="!events.length">暂无可见事件</p>
    <ol>
      <li v-for="event in events" :key="event.key" :style="{ '--event-source': `var(${provenanceTokens[event.provenance]})` }" :class="{ selected: selection.selection?.kind === 'event' && selection.selection.key === event.key }">
        <button class="event-title" @click="selection.selectEvent(event)">{{ event.title }}</button>
        <small>第 {{ event.chapter_idx }} 章 · 顺序 {{ event.order }}<template v-if="event.time"> · {{ event.time }}</template> · {{ provenanceLabels[event.provenance] }}</small>
        <p v-if="event.summary">{{ event.summary }}</p>
        <p v-if="event.participants.length" class="participants">{{ event.participants.map(key => names[key] ?? '未公开实体').join('、') }}</p>
        <button v-if="canJump" type="button" class="jump" @click="emit('jump', event.chapter_idx)">阅读此章</button>
      </li>
    </ol>
  </section>
</template>

<style scoped>
.timeline h3 { margin-top: 0; }
.timeline ol { list-style: none; margin: 0; padding: 0; }
.timeline li { padding: 12px; margin-bottom: 8px; border-left: 3px solid var(--event-source); background: var(--color-surface); overflow-wrap: anywhere; }
.timeline li.selected { outline: 2px solid var(--color-accent); }
.event-title { display: block; font: inherit; font-weight: 600; text-align: left; width: 100%; border: 0; padding: 0; background: none; color: var(--color-text-primary); cursor: pointer; }
small, .participants { color: var(--color-text-secondary); }
.timeline p { margin: 6px 0; font-size: 14px; }
.jump { border: 0; background: none; padding: 4px 0; color: var(--color-accent); cursor: pointer; }
</style>
