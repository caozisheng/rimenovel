<script setup lang="ts">
import { reactive, ref, watch } from "vue";
import { NButton, NInput, NInputNumber, NSelect, NSpace } from "naive-ui";
import type { EditInput, GraphNode } from "../lib/graph";
const props = defineProps<{ bookId: number; chapterIdx: number; driftScore: number; scope: "global" | "chapter"; blind: boolean; nodes: GraphNode[] }>();
const emit = defineEmits<{ save: [input: EditInput] }>();
const state = reactive({ directive: "", drift: 0.5, kind: "node" as "node" | "edge" | "event", nodeType: "character", name: "", aliases: "", summary: "", lifeStatus: null as "alive" | "dead" | null, nodeLocation: null as string | null, owner: null as string | null, src: "", dst: "", relType: "", description: "", title: "", order: 0, participants: [] as string[], eventLocation: null as string | null, time: "", eventSummary: "" });
watch(() => props.driftScore, value => { state.drift = value; }, { immediate: true });
function submit(input: EditInput): void { emit("save", { ...input, chapter_idx: input.scope === "global" ? null : props.chapterIdx }); }
const nodeOptions = () => props.nodes.map((node) => ({ label: node.name, value: node.key }));
const locationOptions = () => nodeOptions().filter(option => props.nodes.find(node => node.key === option.value)?.type === "location");
const ownerOptions = () => nodeOptions().filter(option => ["character", "faction"].includes(props.nodes.find(node => node.key === option.value)?.type ?? ""));
const typeOptions = [{ label: "人物", value: "character" }, { label: "地点", value: "location" }, { label: "物品", value: "item" }, { label: "势力", value: "faction" }, { label: "概念（手动）", value: "concept" }];
const lifeStatusOptions = [{ label: "生存", value: "alive" }, { label: "死亡", value: "dead" }];
const showCreate = ref(false);
function openCreate(kind: "node" | "edge" | "event"): void { state.kind = kind; showCreate.value = true; }
function create(): void {
  if (state.kind === "node") submit({ book_id: props.bookId, scope: props.scope, chapter_idx: props.chapterIdx, kind: "node", action: "upsert", key: null, data: { type: state.nodeType, name: state.name.trim(), aliases: state.aliases.split(",").map((item) => item.trim()).filter(Boolean), summary: state.summary.trim() || null, life_status: state.nodeType === "character" ? state.lifeStatus : null, location: state.nodeLocation, owner: state.nodeType === "item" ? state.owner : null } });
  if (state.kind === "edge") submit({ book_id: props.bookId, scope: props.scope, chapter_idx: props.chapterIdx, kind: "edge", action: "upsert", key: null, data: { src: state.src, dst: state.dst, rel_type: state.relType.trim(), description: state.description.trim() || null } });
  if (state.kind === "event") submit({ book_id: props.bookId, scope: props.scope, chapter_idx: props.chapterIdx, kind: "event", action: "upsert", key: null, data: { title: state.title.trim(), order: state.order, participants: state.participants, location: state.eventLocation, time: state.time.trim() || null, summary: state.eventSummary.trim() } });
  showCreate.value = false;
}
function saveDirective(): void { const directive = state.directive.trim(); if (!directive) return; submit({ book_id: props.bookId, scope: "global", chapter_idx: null, kind: "directive", action: "upsert", key: null, data: { text: directive, polarity: directive.startsWith("-") ? "negative" : "positive" } }); state.directive = ""; }
function saveDrift(): void { submit({ book_id: props.bookId, scope: "global", chapter_idx: null, kind: "drift_pref", action: "upsert", key: null, data: { value: state.drift } }); }
</script>
<template>
  <section class="director"><h2>写作导演</h2><p v-if="blind" class="blind-warning">下一章尚未揭示；新增意图会以盲写方式保存，确认后才显示实体。</p><NSpace><NButton size="small" @click="openCreate('node')">新增实体</NButton><NButton size="small" @click="openCreate('edge')">新增关系</NButton><NButton size="small" @click="openCreate('event')">新增事件</NButton></NSpace>
    <div v-if="showCreate" class="create-form"><template v-if="state.kind === 'node'"><NSelect v-model:value="state.nodeType" :options="typeOptions" /><NInput v-model:value="state.name" placeholder="实体名称" /><NInput v-model:value="state.aliases" placeholder="别名，用逗号分隔" /><NInput v-model:value="state.summary" placeholder="摘要" /><NSelect v-if="state.nodeType === 'character'" v-model:value="state.lifeStatus" clearable :options="lifeStatusOptions" placeholder="生命状态（未知）" /><NSelect v-model:value="state.nodeLocation" clearable :options="locationOptions()" placeholder="所在地" /><NSelect v-if="state.nodeType === 'item'" v-model:value="state.owner" clearable :options="ownerOptions()" placeholder="所有者" /></template>
      <template v-if="state.kind === 'edge'"><NSelect v-model:value="state.src" :options="nodeOptions()" placeholder="起点（已知实体）" /><NSelect v-model:value="state.dst" :options="nodeOptions()" placeholder="终点（已知实体）" /><NInput v-model:value="state.relType" placeholder="关系类型" /><NInput v-model:value="state.description" placeholder="关系说明" /></template>
      <template v-if="state.kind === 'event'"><NInput v-model:value="state.title" placeholder="事件标题" /><NInputNumber v-model:value="state.order" :min="0" /><NSelect v-model:value="state.participants" multiple :options="nodeOptions()" placeholder="参与者" /><NSelect v-model:value="state.eventLocation" :options="locationOptions()" clearable placeholder="地点" /><NInput v-model:value="state.time" placeholder="时间" /><NInput v-model:value="state.eventSummary" placeholder="事件摘要" /></template><NButton size="small" type="primary" @click="create">保存新增意图</NButton>
    </div>
    <label>导演指令（以 - 开头表示禁止）<NInput v-model:value="state.directive" type="textarea" placeholder="仅约束未来生成" /></label><NButton size="small" type="primary" @click="saveDirective">加入指令</NButton><label>漂移偏好（0–1）<NInputNumber v-model:value="state.drift" :min="0" :max="1" :step="0.05" /></label><NButton size="small" @click="saveDrift">保存漂移偏好</NButton>
  </section>
</template>
<style scoped>.director { display: grid; gap: 8px; margin-top: 12px; border: 1px solid var(--color-border); padding: 12px; }.director h2 { margin: 0; font-size: 16px; }.director label { display: grid; gap: 4px; font-size: 13px; color: var(--color-text-secondary); }.create-form { display: grid; gap: 6px; padding: 8px; border: 1px dashed var(--color-border); }.blind-warning { color: var(--color-warning); font-size: 13px; }</style>
