<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { NButton, NModal, NTag } from "naive-ui";
import GraphVisualization from "./GraphVisualization.vue";
import ElementForm from "./ElementForm.vue";
import Director from "./Director.vue";
import Fog from "./Fog.vue";
import { applyGraphEdit, cancelBookJobs, getExtractionStatuses, getGraph, getGraphEvidence, listGraphEdits, onKgEvent, parseGraphChanged, parseJobStatus, revealGraph, revertGraphEdit, startBookExtraction } from "../lib/kg";
import type { EditInput, EditRecord, EvidenceView, GraphElement, GraphJobStatus, GraphPurpose, GraphQuery, GraphView } from "../lib/graph";
import { useGraphSelectionStore } from "../lib/graphSelection";

const props = defineProps<{ bookId: number; currentIdx: number; chapterCount: number }>();
const emit = defineEmits<{ close: []; jump: [chapterIdx: number] }>();
const selection = useGraphSelectionStore();
const purpose = ref<GraphPurpose>("actual");
const globalActual = ref(false);
const globalSpec = ref(false);
const target = ref(props.currentIdx);
const view = ref<GraphView | null>(null);
const knownView = ref<GraphView | null>(null);
const knownNodes = computed(() => {
  const nodes = new Map((knownView.value?.nodes ?? []).map(node => [node.key, node]));
  for (const node of view.value?.nodes ?? []) nodes.set(node.key, node);
  return [...nodes.values()];
});
const edits = ref<EditRecord[]>([]);
const evidence = ref<EvidenceView[]>([]);
const busy = ref(false);
const error = ref("");
const revealPending = ref(false);
const showHistory = ref(false);
let stopGraphChanged: (() => void) | null = null;
let loadToken = 0;
let disposed = false;
let evidenceToken = 0;
const job = ref<GraphJobStatus | null>(null);
const jobError = ref("");
const jobActionError = ref("");
const cancellingJobs = ref(false);
const startingExtraction = ref(false);
let stopJobProgress: (() => void) | null = null;
let pollTimer: number | null = null;
let jobToken = 0;
const jobActive = computed(() => !!job.value && ["queued", "running", "paused"].includes(job.value.state));
const jobPercent = computed(() => Math.round(Math.max(0, Math.min(1, job.value?.progress ?? 0)) * 100));
const jobLabel = computed(() => {
  const labels: Record<string, string> = { queued: "等待执行", running: "后台处理中", paused: "已暂停", done: "任务完成", failed: "任务失败", cancelled: "已取消" };
  return job.value ? labels[job.value.state] ?? job.value.state : "尚未开始";
});
const jobKind = computed(() => {
  const kinds: Record<string, string> = { global_extract: "整书抽取", chapter_extract: "章节抽取", generate: "正文生成与回抽", reconcile: "规格调和" };
  return job.value?.kind ? kinds[job.value.kind] ?? job.value.kind : "知识图谱抽取";
});
const jobPhaseLabels: Record<string, string> = {
  queued: "等待执行", book_wait: "等待同书任务", provider_wait: "等待模型请求名额", paused: "已暂停",
  chapter: "章节处理", stage_a: "抽取实体", identity: "实体对齐", stage_b: "抽取关系与事件",
  adjudication: "事实裁定", write: "生成正文", reconcile: "规格调和", reduce: "整理图谱",
  request: "请求模型", retry_wait: "等待重试", done: "完成", failed: "失败", cancelled: "已取消",
};
async function refreshJob(): Promise<void> {
  const token = ++jobToken;
  try {
    const statuses = await getExtractionStatuses();
    if (disposed || token !== jobToken) return;
    const previous = job.value;
    job.value = statuses.find(status => status.book_id === props.bookId) ?? null;
    jobError.value = "";
    if (job.value?.state === "done" && (previous?.job_id !== job.value.job_id || previous?.state !== "done")) void load();
  } catch (e) { if (!disposed && token === jobToken) jobError.value = String(e); }
}
async function startExtraction(): Promise<void> {
  if (startingExtraction.value || cancellingJobs.value || jobActive.value) return;
  startingExtraction.value = true;
  jobActionError.value = "";
  try { await startBookExtraction(props.bookId); await refreshJob(); }
  catch (e) { if (!disposed) jobActionError.value = `启动失败：${String(e)}`; }
  finally { startingExtraction.value = false; }
}
async function cancelJobs(): Promise<void> {
  if (!jobActive.value || cancellingJobs.value) return;
  cancellingJobs.value = true;
  jobActionError.value = "";
  try { await cancelBookJobs(props.bookId); await refreshJob(); }
  catch (e) { if (!disposed) jobActionError.value = `取消失败：${String(e)}`; }
  finally { cancellingJobs.value = false; }
}
const nextTarget = computed(() => (view.value?.reader_progress ?? props.currentIdx) + 1);
const hasNext = computed(() => nextTarget.value <= props.chapterCount);
const editableTarget = computed(() => purpose.value === "spec" && hasNext.value && target.value === nextTarget.value);
const editable = computed(() => editableTarget.value && !!selected.value && (selected.value.provenance === "user" || !!view.value?.revealed || globalSpec.value));
const selected = computed<GraphElement | null>(() => {
  const current = selection.selection;
  if (!current || !view.value) return null;
  const collection = current.kind === "node" ? view.value.nodes : current.kind === "edge" ? view.value.edges : view.value.events;
  return collection.find((item) => item.key === current.key) ?? null;
});
const query = computed<GraphQuery>(() => ({ book_id: props.bookId, scope: (purpose.value === "actual" && globalActual.value) || (purpose.value === "spec" && globalSpec.value) ? "global" : "chapter", purpose: purpose.value, chapter_idx: target.value }));
async function load(): Promise<void> {
  const token = ++loadToken;
  busy.value = true;
  error.value = "";
  evidence.value = [];
  try {
    const request = { ...query.value };
    const [nextView, nextEdits, actual] = await Promise.all([getGraph(request), listGraphEdits(props.bookId), getGraph({ book_id: props.bookId, scope: "global", purpose: "actual", chapter_idx: props.currentIdx })]);
    if (disposed || token !== loadToken) return;
    view.value = nextView;
    knownView.value = actual;
    edits.value = nextEdits;
  } catch (e) {
    if (token === loadToken) error.value = String(e);
  } finally {
    if (token === loadToken) busy.value = false;
  }
}
async function reveal(): Promise<void> {
  busy.value = true;
  error.value = "";
  try { await revealGraph(props.bookId, target.value); revealPending.value = false; await load(); }
  catch (e) { error.value = String(e); }
  finally { busy.value = false; }
}
async function save(input: EditInput): Promise<void> {
  busy.value = true;
  error.value = "";
  try { await applyGraphEdit(input); await load(); }
  catch (e) { error.value = String(e); }
  finally { busy.value = false; }
}
async function showEvidence(kind: string, key: string): Promise<void> {
  const version = ++evidenceToken;
  const request = { ...query.value };
  evidence.value = [];
  try {
    const result = await getGraphEvidence(request, kind, key);
    if (!disposed && version === evidenceToken && selection.selection?.key === key) evidence.value = result;
  } catch (e) { if (version === evidenceToken) error.value = String(e); }
}
async function revert(id: number): Promise<void> {
  busy.value = true;
  try { await revertGraphEdit(props.bookId, id); await load(); }
  catch (e) { error.value = String(e); }
  finally { busy.value = false; }
}
function setTab(key: string): void {
  if (key !== "actual" && key !== "reference" && key !== "spec") return;
  purpose.value = key;
  globalSpec.value = false;
  selection.clear(); evidenceToken++; evidence.value = []; view.value = null;
  target.value = key === "spec" ? (knownView.value?.reader_progress ?? props.currentIdx) + 1 : props.currentIdx;
  void load();
}
function setActualScope(scope: string): void { globalActual.value = scope === "global"; target.value = props.currentIdx; selection.clear(); void load(); }
function setSpecScope(scope: string): void { globalSpec.value = scope === "global"; target.value = nextTarget.value; selection.clear(); void load(); }
watch(() => props.currentIdx, () => { target.value = purpose.value === "spec" ? nextTarget.value : props.currentIdx; selection.clear(); void load(); });
watch(() => selection.selection, () => { evidenceToken++; evidence.value = []; });
onMounted(async () => {
  try {
    const stop = await onKgEvent("graph:changed", parseGraphChanged, event => { if (event.book_id === props.bookId) void load(); });
    if (disposed) { stop(); return; }
    stopGraphChanged = stop;
    const stopProgress = await onKgEvent("job:progress", parseJobStatus, event => { if (event.book_id === props.bookId) void refreshJob(); });
    if (disposed) { stopProgress(); return; }
    stopJobProgress = stopProgress;
  } catch (e) { if (!disposed) jobError.value = String(e); }
  if (disposed) return;
  await Promise.all([load(), refreshJob()]);
  if (!disposed) pollTimer = window.setInterval(() => { void refreshJob(); }, 3000);
});
onUnmounted(() => { disposed = true; loadToken++; evidenceToken++; jobToken++; stopGraphChanged?.(); stopJobProgress?.(); if (pollTimer !== null) window.clearInterval(pollTimer); selection.clear(); });
</script>

<template>
  <section class="workbench" aria-label="知识图谱工作台">
    <header class="workbench-header">
      <NButton quaternary @click="emit('close')">← 返回阅读</NButton>
      <h1>知识图谱</h1>
      <span v-if="view" class="status">{{ view.ready ? "已就绪" : "抽取未完成" }}</span>
      <NButton class="close" @click="emit('close')">关闭</NButton>
    </header>
    <section class="extraction-status" aria-label="后台图谱任务">
      <div class="extraction-heading"><strong>{{ jobKind }}</strong><span role="status">{{ jobLabel }}<template v-if="job?.chapter_idx"> · 第 {{ job.chapter_idx }} 章</template><template v-if="job"> · {{ jobPercent }}%</template></span>
        <NButton v-if="jobActive" size="small" :loading="cancellingJobs" :disabled="cancellingJobs" aria-label="取消当前书籍后台任务" @click="cancelJobs">{{ cancellingJobs ? '取消中…' : '取消任务' }}</NButton>
        <NButton v-else-if="!view?.ready || job?.state === 'failed' || job?.state === 'cancelled'" size="small" :loading="startingExtraction" :disabled="cancellingJobs" @click="startExtraction">{{ job?.state === 'failed' ? '重试抽取' : '开始抽取' }}</NButton>
      </div>
      <progress v-if="job" max="100" :value="jobActive && jobPercent === 0 ? undefined : jobPercent" aria-label="后台任务进度" />
      <dl v-if="job?.phase || job?.attempt != null || (jobActive && job?.retry_after_ms != null)" class="extraction-observations" aria-label="后台任务运行详情" aria-live="polite">
        <div v-if="job?.phase"><dt>阶段：</dt><dd>{{ jobPhaseLabels[job.phase] ?? job.phase }}</dd></div>
        <div v-if="job?.attempt != null"><dt>尝试：</dt><dd>第 {{ job.attempt }} 次尝试</dd></div>
        <div v-if="jobActive && job?.retry_after_ms != null"><dt>重试等待：</dt><dd>{{ job.retry_after_ms / 1000 }} 秒</dd></div>
      </dl>
      <p v-if="jobActive" class="extraction-note">后台处理不会阻止阅读；进度按已提交章节更新，首章或模型请求处理中可能保持 0%。</p>
      <p v-else-if="!view?.ready && !job" class="extraction-note">尚无抽取任务。Provider 连接测试不会自动抽取小说，请开始抽取。</p>
      <p v-else-if="!view?.ready && job?.state === 'done'" class="extraction-note">后台任务已完成，但当前视图仍缺少抽取结果，可开始补充抽取。</p>
      <p v-if="job?.error" class="extraction-error" role="alert">{{ job.error }}</p>
      <p v-if="jobError" class="extraction-error" role="alert">后台状态获取失败：{{ jobError }}</p>
      <p v-if="jobActionError" class="extraction-error" role="alert">{{ jobActionError }}</p>
    </section>
    <nav class="purpose-tabs" role="tablist" aria-label="图谱语义空间"><NButton size="small" :type="purpose === 'actual' ? 'primary' : 'default'" @click="setTab('actual')">当前事实</NButton><NButton size="small" :type="purpose === 'reference' ? 'primary' : 'default'" @click="setTab('reference')">原作参考</NButton><NButton size="small" :type="purpose === 'spec' ? 'primary' : 'default'" @click="setTab('spec')">未来规格</NButton></nav>
    <div class="scope-switch" role="group" aria-label="图谱范围"><template v-if="purpose === 'actual'"><NButton size="small" :type="!globalActual ? 'primary' : 'default'" @click="setActualScope('chapter')">当前章节（{{ target }}）</NButton><NButton size="small" :type="globalActual ? 'primary' : 'default'" @click="setActualScope('global')">全局当前事实</NButton></template><template v-else-if="purpose === 'spec'"><NButton size="small" :type="!globalSpec ? 'primary' : 'default'" @click="setSpecScope('chapter')">下一章规格（{{ nextTarget }}）</NButton><NButton size="small" :type="globalSpec ? 'primary' : 'default'" @click="setSpecScope('global')">全局未来意图</NButton></template></div>
    <NModal v-model:show="revealPending" preset="dialog" title="揭示下一章图谱" positive-text="揭示" negative-text="稍后" @positive-click="reveal"><p>揭示下一章会显示已确认的实体、关系和事件，可能包含剧透。要继续吗？</p></NModal>
    <div class="workbench-body" :class="{ 'without-properties': !(selected || editableTarget || evidence.length) }">
      <p v-if="error" class="error" role="alert">{{ error }}</p>
      <Fog v-if="view?.fog" :fog="view.fog" :revealed="view.revealed" :purpose="purpose" :can-reveal="editableTarget" @reveal="revealPending = true" />
      <p v-if="purpose === 'spec'" class="scope-note">下一章规格（{{ target }}）仅影响未来生成，不会改写当前事实。</p>
      <GraphVisualization v-if="view" :nodes="view.nodes" :edges="view.edges" :events="view.events" :purpose="purpose" :show-properties="false" :can-jump="purpose !== 'spec'" @jump="emit('jump', $event)" />
      <p v-else-if="busy" role="status">加载图谱中…</p>
      <p v-else>暂无图谱数据。</p>
      <aside v-if="selected || editableTarget || evidence.length" class="workbench-properties" aria-label="图谱编辑属性">
        <ElementForm v-if="selected" :element="selected" :nodes="knownNodes" :busy="busy" :editable="editable" :book-id="bookId" :chapter-idx="target" :purpose="purpose" :scope="globalSpec ? 'global' : 'chapter'" @save="save" @evidence="showEvidence" />
        <Director v-if="editableTarget" :book-id="bookId" :chapter-idx="target" :drift-score="view?.drift_score ?? 0" :scope="globalSpec ? 'global' : 'chapter'" :blind="!view?.revealed" :nodes="knownNodes" @save="save" />
        <section v-if="evidence.length" class="evidence"><h2>证据</h2><article v-for="item in evidence" :key="item.key"><small>第 {{ item.chapter_idx }} 章 · {{ item.source }}</small><p>{{ item.text }}</p></article></section>
      </aside>
    </div>
    <footer class="workbench-footer">
      <NButton size="small" @click="showHistory = !showHistory">{{ showHistory ? '隐藏编辑历史' : '编辑历史' }}</NButton>
      <div v-if="showHistory" class="history"><span v-if="!edits.length">暂无用户编辑</span><div v-for="edit in edits" :key="edit.id"><NTag :type="edit.active ? 'warning' : 'default'" size="small">{{ edit.target_type }} · {{ edit.action }}</NTag><span>从第 {{ edit.effective_from }} 章生效</span><NButton v-if="edit.active" size="tiny" :disabled="busy" @click="revert(edit.id)">撤销</NButton></div></div>
    </footer>
  </section>
</template>

<style scoped>
.workbench { min-height: 100dvh; background: var(--color-surface); color: var(--color-text-primary); display: flex; flex-direction: column; }
.workbench-header { display: flex; align-items: center; gap: 12px; padding: 12px max(16px, env(safe-area-inset-left)); border-bottom: 1px solid var(--color-border); }.workbench-header h1 { margin: 0; font-size: 18px; }.status { color: var(--color-text-secondary); font-size: 13px; }.close { margin-left: auto; }
.purpose-tabs, .scope-switch { display: flex; flex-wrap: wrap; gap: 8px; padding: 8px 12px; border-bottom: 1px solid var(--color-border); }
.extraction-status { padding: 12px; border-bottom: 1px solid var(--color-border); }
.extraction-heading { display: flex; align-items: center; flex-wrap: wrap; gap: 8px 12px; font-size: 13px; }
.extraction-heading > span { color: var(--color-text-secondary); }
.extraction-heading > button { margin-left: auto; }
.extraction-status progress { display: block; width: 100%; height: 8px; margin-top: 10px; accent-color: var(--color-accent); }
.extraction-observations { display: flex; flex-wrap: wrap; gap: 6px 16px; margin: 8px 0 0; font-size: 13px; }
.extraction-observations > div { display: flex; min-width: 0; overflow-wrap: anywhere; }
.extraction-observations dt { color: var(--color-text-secondary); flex-shrink: 0; }
.extraction-observations dd { margin: 0; }
.extraction-note, .extraction-error { font-size: 13px; margin: 8px 0 0; }
.extraction-note { color: var(--color-text-secondary); }
.extraction-error { color: var(--color-danger); white-space: pre-wrap; overflow-wrap: anywhere; max-height: 180px; overflow: auto; }
.workbench-body { flex: 1; display: grid; grid-template-columns: minmax(0, 1fr) minmax(260px, 340px); gap: 12px; padding: 12px; min-height: 0; }.workbench-body.without-properties { grid-template-columns: minmax(0, 1fr); }.workbench-body > .visualization { min-height: 520px; height: calc(100dvh - 190px); }.workbench-properties { overflow: auto; max-height: calc(100dvh - 190px); }.error { color: var(--color-danger); grid-column: 1 / -1; }.scope-note { grid-column: 1 / -1; margin: 0; color: var(--color-text-secondary); }.workbench-footer { border-top: 1px solid var(--color-border); padding: 8px 12px; }.history { display: grid; gap: 6px; margin-top: 8px; }.history > div { display: flex; align-items: center; gap: 8px; font-size: 13px; }.evidence { margin-top: 12px; border-top: 1px solid var(--color-border); padding-top: 12px; }.evidence h2 { font-size: 15px; }.evidence article { border: 1px solid var(--color-border); padding: 8px; margin-bottom: 8px; }.evidence p { white-space: pre-wrap; margin: 4px 0 0; }
@media (max-width: 760px) { .workbench-body, .workbench-body.without-properties { grid-template-columns: 1fr; }.workbench-properties { position: fixed; z-index: 5; left: 0; right: 0; bottom: 0; max-height: 62dvh; background: var(--color-surface); border-top: 1px solid var(--color-border); padding: 12px; }.workbench-body > .visualization { height: calc(100dvh - 220px); min-height: 380px; } }
</style>
