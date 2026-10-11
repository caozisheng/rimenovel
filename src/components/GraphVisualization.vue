<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, toRefs, useId, watch } from "vue";
import CyGraph from "./CyGraph.vue";
import TimelineView from "./TimelineView.vue";
import { entityTypes, eventNodeKeys, filterGraph, provenanceLabels, provenanceTokens, purposeLabels, type EntityType, type GraphEdge, type GraphElement, type GraphEvent, type GraphNode, type GraphPurpose } from "../lib/graph";
import { useGraphSelectionStore } from "../lib/graphSelection";

const props = withDefaults(defineProps<{
  nodes: GraphNode[];
  edges: GraphEdge[];
  events: GraphEvent[];
  title?: string;
  canJump?: boolean;
  purpose: GraphPurpose;
  showProperties?: boolean;
}>(), { title: "图谱与时间线", canJump: false, showProperties: true });
const { nodes, events, title, canJump, purpose, showProperties } = toRefs(props);
const emit = defineEmits<{ jump: [chapterIdx: number] }>();
const selection = useGraphSelectionStore();
const activeTypes = ref<EntityType[]>(entityTypes.map((type) => type.value));
const showTools = ref(false);
const toolsId = useId();
const toolsPane = ref<HTMLElement>();
const toolsButton = ref<HTMLButtonElement>();
const propertyClose = ref<HTMLButtonElement>();
let selectionOrigin: HTMLElement | null = null;
const filtered = computed(() => filterGraph(props, activeTypes.value));
const names = computed(() => Object.fromEntries(props.nodes.map((node) => [node.key, node.name])));
const selected = computed<GraphElement | null>(() => {
  const current = selection.selection;
  if (!current) return null;
  const elements = current.kind === "node" ? filtered.value.nodes : current.kind === "edge" ? filtered.value.edges : props.events;
  return elements.find((element) => element.key === current.key) ?? null;
});
const selectedTitle = computed(() => {
  const element = selected.value;
  if (!element) return "属性";
  if ("name" in element) return element.name;
  if ("title" in element) return element.title;
  return element.rel_type;
});
const entityLabels = Object.fromEntries(entityTypes.map((type) => [type.value, type.label]));

watch(selected, (element) => {
  if (!element) selection.clear();
  else if ("participants" in element) selection.highlightedNodeKeys = eventNodeKeys(element);
}, { deep: true, immediate: true });
watch(() => selection.selection, async (current) => {
  if (current) {
    const active = document.activeElement;
    const transferFocus = showTools.value && active instanceof HTMLElement && !!toolsPane.value?.contains(active);
    if (transferFocus) selectionOrigin = active;
    showTools.value = false;
    if (transferFocus) {
      await nextTick();
      if (selected.value) propertyClose.value?.focus({ preventScroll: true });
    }
  } else if (selectionOrigin) {
    const origin = selectionOrigin;
    selectionOrigin = null;
    showTools.value = true;
    await nextTick();
    if (origin.isConnected) origin.focus({ preventScroll: true });
    else toolsButton.value?.focus({ preventScroll: true });
  }
});
onBeforeUnmount(() => selection.clear());
</script>

<template>
  <section class="visualization" :class="{ 'without-properties': !showProperties }" :aria-label="title">
    <header class="visualization-header">
      <h2>{{ title }}</h2>
      <span class="read-only">只读</span>
      <span class="read-only">{{ purposeLabels[purpose] }}</span>
      <span class="counts">{{ filtered.nodes.length }} 实体 · {{ filtered.edges.length }} 关系 · {{ events.length }} 事件</span>
      <button ref="toolsButton" type="button" class="tools-toggle" :aria-expanded="showTools" :aria-controls="toolsId" @click="showTools = !showTools">{{ showTools ? '返回图谱' : '筛选 / 时间线' }}</button>
    </header>
    <div class="panes">
      <aside ref="toolsPane" :id="toolsId" class="tools-pane" :class="{ open: showTools }" aria-label="图谱筛选与时间线">
        <fieldset class="filters">
          <legend>实体类型</legend>
          <label v-for="type in entityTypes" :key="type.value"><input v-model="activeTypes" type="checkbox" :value="type.value" />{{ type.label }}</label>
        </fieldset>
        <section class="legend" aria-label="来源图例">
          <h3>来源</h3>
          <span v-for="(label, provenance) in provenanceLabels" :key="provenance"><i :style="{ background: `var(${provenanceTokens[provenance]})` }" />{{ label }}</span>
        </section>
        <TimelineView :events="events" :nodes="nodes" :can-jump="canJump" @jump="emit('jump', $event)" />
        <details class="accessible-list">
          <summary>可见实体（{{ filtered.nodes.length }}）</summary>
          <button v-for="node in filtered.nodes" :key="node.key" type="button" :aria-pressed="selection.selection?.kind === 'node' && selection.selection.key === node.key" @click="selection.select('node', node.key)">{{ node.name }}</button>
        </details>
        <details class="accessible-list">
          <summary>可见关系（{{ filtered.edges.length }}）</summary>
          <button v-for="edge in filtered.edges" :key="edge.key" type="button" :aria-pressed="selection.selection?.kind === 'edge' && selection.selection.key === edge.key" @click="selection.select('edge', edge.key)">{{ names[edge.src] }} → {{ names[edge.dst] }}：{{ edge.rel_type }}</button>
        </details>
      </aside>
      <main class="graph-pane" aria-label="实体关系画布">
        <CyGraph :nodes="filtered.nodes" :edges="filtered.edges" />
      </main>
      <aside v-if="showProperties" class="properties" :class="{ active: !!selected }" aria-label="所选元素属性">
        <template v-if="selected">
          <div class="properties-header"><h3>{{ selectedTitle }}</h3><button ref="propertyClose" type="button" aria-label="关闭所选元素属性" @click="selection.clear()">关闭</button></div>
          <dl>
            <dt>来源</dt><dd class="source"><i :style="{ background: `var(${provenanceTokens[selected.provenance]})` }" />{{ provenanceLabels[selected.provenance] }}</dd>
            <dt>章节</dt><dd>{{ selected.chapter_idx }}</dd>
            <template v-if="'type' in selected">
              <dt>实体类型</dt><dd>{{ entityLabels[selected.type] }}</dd>
              <dt>别名</dt><dd>{{ selected.aliases.join('、') || '无' }}</dd>
              <dt>摘要</dt><dd>{{ selected.summary || '无' }}</dd>
              <dt v-if="selected.type === 'character'">生命状态</dt><dd v-if="selected.type === 'character'">{{ selected.life_status === 'alive' ? '生存' : selected.life_status === 'dead' ? '死亡' : '未知' }}</dd>
              <dt>所在地</dt><dd>{{ selected.location ? names[selected.location] ?? '未公开实体' : '无' }}</dd>
              <dt v-if="selected.type === 'item'">所有者</dt><dd v-if="selected.type === 'item'">{{ selected.owner ? names[selected.owner] ?? '未公开实体' : '无' }}</dd>
            </template>
            <template v-else-if="'src' in selected">
              <dt>起点</dt><dd>{{ names[selected.src] ?? '未公开实体' }}</dd>
              <dt>终点</dt><dd>{{ names[selected.dst] ?? '未公开实体' }}</dd>
              <dt>关系说明</dt><dd>{{ selected.description || '无' }}</dd>
            </template>
            <template v-else>
              <dt>章内顺序</dt><dd>{{ selected.order }}</dd>
              <dt>参与者</dt><dd>{{ selected.participants.map(key => names[key] ?? '未公开实体').join('、') || '无' }}</dd>
              <dt>地点</dt><dd>{{ selected.location ? names[selected.location] ?? '未公开实体' : '无' }}</dd>
              <dt>时间</dt><dd>{{ selected.time || '无' }}</dd>
              <dt>摘要</dt><dd>{{ selected.summary || '无' }}</dd>
            </template>
          </dl>
          <button v-if="canJump && 'participants' in selected" type="button" @click="emit('jump', selected.chapter_idx)">阅读此章</button>
        </template>
        <template v-else><h3>属性</h3><p class="help">点击实体、关系或时间线事件查看详情。时间线选择会高亮参与者和地点。</p></template>
      </aside>
    </div>
  </section>
</template>

<style scoped>
.visualization { display: flex; flex-direction: column; width: 100%; height: 100dvh; min-height: 0; background: var(--color-surface); color: var(--color-text-primary); padding-top: env(safe-area-inset-top); }
.visualization-header { display: flex; flex-wrap: wrap; align-items: center; gap: 8px 12px; padding: 12px max(16px, env(safe-area-inset-right)) 12px max(16px, env(safe-area-inset-left)); border-bottom: 1px solid var(--color-border); }
h2 { font-size: 18px; margin: 0; }
h3 { font-size: 16px; margin: 0 0 12px; }
.read-only { padding: 2px 6px; font-size: 12px; color: var(--color-text-secondary); border: 1px solid var(--color-border); border-radius: var(--radius-xs); }
.counts { margin-left: auto; font-size: 13px; color: var(--color-text-secondary); }
button { font: inherit; font-size: 14px; padding: 7px 10px; border: 1px solid var(--color-border); border-radius: var(--radius-xs); background: var(--color-surface-raised); color: var(--color-text-primary); cursor: pointer; }
button[aria-pressed="true"] { border-color: var(--color-accent); color: var(--color-accent); }
button:focus-visible, input:focus-visible, summary:focus-visible { outline: 2px solid var(--color-accent); outline-offset: 2px; }
.panes { position: relative; display: grid; grid-template-columns: 260px minmax(0, 1fr) 310px; flex: 1; min-height: 0; }
.without-properties .panes { grid-template-columns: 260px minmax(0, 1fr); }
.tools-pane, .properties { padding: 16px; overflow-y: auto; min-height: 0; padding-bottom: max(16px, env(safe-area-inset-bottom)); }
.tools-pane { border-right: 1px solid var(--color-border); }
.properties { border-left: 1px solid var(--color-border); background: var(--color-surface-raised); }
.graph-pane { min-width: 0; min-height: 0; }
.filters { display: flex; flex-wrap: wrap; gap: 10px; padding: 0; border: 0; margin: 0 0 20px; }
.filters legend { margin-bottom: 10px; font-size: 16px; font-weight: 600; }
.filters label { display: flex; align-items: center; gap: 4px; font-size: 14px; }
.legend { display: flex; flex-wrap: wrap; gap: 8px 12px; margin-bottom: 20px; }
.legend h3 { flex: 0 0 100%; margin-bottom: 0; }
.legend span, .source { display: flex; align-items: center; gap: 5px; font-size: 12px; }
.legend i, .source i { display: inline-block; width: 9px; height: 9px; border-radius: 50%; flex-shrink: 0; }
.properties-header { display: flex; align-items: flex-start; gap: 8px; }
.properties-header h3 { flex: 1; overflow-wrap: anywhere; }
dl { font-size: 14px; margin-top: 0; overflow-wrap: anywhere; }
dt { color: var(--color-text-secondary); margin-top: 12px; }
dd { margin: 4px 0; white-space: pre-wrap; }
.help { color: var(--color-text-secondary); font-size: 14px; line-height: 1.6; }
.accessible-list { margin: 16px 0; }
.accessible-list summary { cursor: pointer; font-size: 14px; }
.accessible-list button { display: block; width: 100%; margin-top: 5px; text-align: left; overflow-wrap: anywhere; }
.tools-toggle { display: none; }
@media (max-width: 900px) {
  .visualization-header { padding: 10px max(12px, env(safe-area-inset-right)) 10px max(12px, env(safe-area-inset-left)); }
  .visualization-header h2 { flex: 1; }
  .counts { order: 2; flex: 0 0 100%; margin-left: 0; }
  .tools-toggle { display: block; }
  .panes { display: flex; flex-direction: column; }
  .graph-pane { flex: 1; }
  .graph-pane :deep(.canvas-wrap) { min-height: 0; }
  .tools-pane { display: none; position: absolute; inset: 0; z-index: 3; width: min(380px, 100%); background: var(--color-surface); border-right: 1px solid var(--color-border); }
  .tools-pane.open { display: block; }
  .properties { display: none; flex-shrink: 0; max-height: 45dvh; border: 1px solid var(--color-border); border-radius: 16px 16px 0 0; box-shadow: 0 -4px 20px #0002; }
  .properties.active { display: block; }
}
</style>
