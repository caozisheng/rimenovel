<script setup lang="ts">
import { computed, reactive, watch } from "vue";
import { NButton, NInput, NInputNumber, NSelect, NSpace } from "naive-ui";
import { entityTypes, provenanceLabels, type EditInput, type GraphElement, type GraphNode, type GraphPurpose } from "../lib/graph";
const props = defineProps<{ element: GraphElement; nodes: GraphNode[]; editable: boolean; busy: boolean; bookId: number; chapterIdx: number; purpose: GraphPurpose; scope: "global" | "chapter" }>();
const emit = defineEmits<{ save: [input: EditInput]; evidence: [kind: string, key: string] }>();
const state = reactive({ label: "", summary: "", aliases: "", type: "character", src: "", dst: "", description: "", status: "unknown", outcome: "", participants: [] as string[], location: null as string | null, order: 1 });
const label = computed(() => "name" in props.element ? props.element.name : "title" in props.element ? props.element.title : props.element.rel_type);
const nodeOptions = computed(() => props.nodes.map(node => ({ label: node.name, value: node.key })));
const statusOptions = [{ label: "未知", value: "unknown" }, { label: "有效", value: "active" }, { label: "破裂", value: "broken" }, { label: "曾经", value: "former" }];
const kind = computed<EditInput["kind"]>(() => "type" in props.element ? "node" : "src" in props.element ? "edge" : "event");
watch(() => props.element, element => {
  state.label = "name" in element ? element.name : "title" in element ? element.title : element.rel_type;
  state.summary = "summary" in element ? element.summary ?? "" : "";
  if ("type" in element) { state.type = element.type; state.aliases = element.aliases.join(", "); }
  if ("src" in element) { state.src = element.src; state.dst = element.dst; state.status = element.status; state.description = element.description ?? ""; }
  if ("participants" in element) { state.participants = [...element.participants]; state.location = element.location; state.order = element.order; state.outcome = element.outcome; }
}, { immediate: true });
function save(): void {
  if (!props.editable || props.busy || !state.label.trim()) return;
  const data = kind.value === "node" ? { name: state.label.trim(), type: state.type, aliases: state.aliases.split(",").map(s => s.trim()).filter(Boolean), summary: state.summary.trim() || null }
    : kind.value === "edge" ? { src: state.src, dst: state.dst, rel_type: state.label.trim(), status: state.status, description: state.description.trim() || null }
    : { title: state.label.trim(), order: state.order, participants: state.participants, location: state.location, summary: state.summary.trim(), outcome: state.outcome.trim() };
  emit("save", { book_id: props.bookId, scope: props.scope, chapter_idx: props.scope === "global" ? null : props.chapterIdx, kind: kind.value, action: "upsert", key: props.element.key, data });
}
function remove(): void {
  if (props.editable && !props.busy) emit("save", { book_id: props.bookId, scope: props.scope, chapter_idx: props.scope === "global" ? null : props.chapterIdx, kind: kind.value, action: "delete", key: props.element.key, data: {} });
}
</script>
<template>
  <section class="element-form">
    <h2>{{ label }}</h2>
    <dl><dt>章节</dt><dd>{{ element.chapter_idx }}</dd><dt>来源</dt><dd>{{ provenanceLabels[element.provenance] }}</dd></dl>
    <NSpace v-if="editable" vertical>
      <label>{{ kind === 'node' ? '实体名称' : kind === 'edge' ? '关系类型' : '事件标题' }}<NInput v-model:value="state.label" :disabled="busy" /></label>
      <template v-if="kind === 'node'">
        <label>实体类型<NSelect v-model:value="state.type" :options="entityTypes" :disabled="busy" /></label>
        <label>别名<NInput v-model:value="state.aliases" :disabled="busy" /></label>
      </template>
      <template v-if="kind === 'edge'">
        <label>起点<NSelect v-model:value="state.src" :options="nodeOptions" :disabled="busy" /></label>
        <label>终点<NSelect v-model:value="state.dst" :options="nodeOptions" :disabled="busy" /></label>
        <label>关系状态<NSelect v-model:value="state.status" :options="statusOptions" :disabled="busy" /></label>
        <label>关系说明<NInput v-model:value="state.description" type="textarea" :disabled="busy" /></label>
      </template>
      <label v-if="kind !== 'edge'">摘要<NInput v-model:value="state.summary" type="textarea" :disabled="busy" /></label>
      <template v-if="kind === 'event'">
        <label>章内顺序<NInputNumber v-model:value="state.order" :min="1" :disabled="busy" /></label>
        <label>参与者<NSelect v-model:value="state.participants" multiple :options="nodeOptions" :disabled="busy" /></label>
        <label>地点<NSelect v-model:value="state.location" clearable :options="nodeOptions.filter(node => nodes.find(n => n.key === node.value)?.type === 'location')" :disabled="busy" /></label>
        <label>预期结果<NInput v-model:value="state.outcome" type="textarea" :disabled="busy" /></label>
      </template>
      <NSpace><NButton size="small" type="primary" :disabled="busy || !state.label.trim()" @click="save">保存意图</NButton><NButton size="small" type="error" :disabled="busy" @click="remove">删除未来意图</NButton></NSpace>
    </NSpace>
    <p v-else class="note">{{ purpose === 'spec' ? '修正 AI 规格前请揭幕；自己添加的意图可直接修改。' : '已发生事实与原作参考只读。' }}</p>
    <NButton size="small" quaternary :disabled="busy" @click="emit('evidence', kind, element.key)">查看证据</NButton>
  </section>
</template>
<style scoped>
.element-form { border: 1px solid var(--color-border); padding: 12px; }
h2 { margin: 0 0 10px; font-size: 16px; overflow-wrap: anywhere; }
dl { display: grid; grid-template-columns: auto 1fr; gap: 6px 12px; font-size: 13px; }
dt, .note { color: var(--color-text-secondary); } dd { margin: 0; }
label { display: grid; gap: 4px; font-size: 13px; }
</style>
