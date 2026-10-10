<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import cytoscape from "cytoscape";
import fcose from "cytoscape-fcose";
import { provenanceTokens, type GraphNode, type GraphEdge } from "../lib/graph";
import { useGraphSelectionStore } from "../lib/graphSelection";

cytoscape.use(fcose);
const props = defineProps<{ nodes: GraphNode[]; edges: GraphEdge[] }>();
const host = ref<HTMLDivElement>();
const selection = useGraphSelectionStore();
let cy: cytoscape.Core | undefined;
let resize: ResizeObserver | undefined;
let themeObserver: MutationObserver | undefined;
let themeMedia: MediaQueryList | undefined;
const nodeId = (key: string) => `node:${key}`;
const edgeId = (key: string) => `edge:${key}`;

function applyStyles(): void {
  if (!cy || !host.value) return;
  const tokens = getComputedStyle(host.value);
  const text = tokens.getPropertyValue("--color-text-primary").trim();
  const accent = tokens.getPropertyValue("--color-accent").trim();
  cy.style([
    { selector: "node", style: { label: "data(label)", "background-color": "data(color)", color: text, "font-size": 12, "text-valign": "bottom", "text-margin-y": 8, width: 36, height: 36, "text-wrap": "wrap", "text-max-width": "100px" } },
    { selector: 'node[type = "location"]', style: { shape: "round-rectangle" } },
    { selector: 'node[type = "item"]', style: { shape: "diamond" } },
    { selector: 'node[type = "faction"]', style: { shape: "hexagon" } },
    { selector: 'node[type = "concept"]', style: { shape: "ellipse", "border-width": 2, "border-style": "dashed", "border-color": text } },
    { selector: "edge", style: { label: "data(label)", width: 2, "line-color": "data(color)", "target-arrow-color": "data(color)", "target-arrow-shape": "triangle", "curve-style": "bezier", color: text, "font-size": 10, "text-rotation": "autorotate", "text-background-color": tokens.getPropertyValue("--color-surface-raised").trim(), "text-background-opacity": 0.85, "text-background-padding": "3px" } },
    { selector: 'edge[status = "broken"], edge[status = "former"]', style: { "line-style": "dashed", opacity: 0.6 } },
    { selector: ".chosen, .event-highlight", style: { "border-width": 4, "border-color": accent, "overlay-color": accent, "overlay-opacity": 0.15, "overlay-padding": 8 } },
    { selector: "edge.chosen", style: { width: 5, "line-color": accent, "target-arrow-color": accent } },
  ]);
  for (const element of cy.elements()) {
    element.data("color", tokens.getPropertyValue(provenanceTokens[element.data("provenance") as keyof typeof provenanceTokens]).trim());
  }
}
function highlight(): void {
  if (!cy) return;
  cy.elements().removeClass("chosen event-highlight");
  const current = selection.selection;
  if (current?.kind === "node") cy.getElementById(nodeId(current.key)).addClass("chosen");
  if (current?.kind === "edge") cy.getElementById(edgeId(current.key)).addClass("chosen");
  for (const key of selection.highlightedNodeKeys) cy.getElementById(nodeId(key)).addClass("event-highlight");
}
function fitGraph(): void {
  if (!cy || !cy.nodes().length) return;
  cy.fit(undefined, 48);
  // Auto-fit should not inflate one or two entities into screen-sized labels.
  if (cy.zoom() > 1.5) cy.zoom({ level: 1.5, renderedPosition: { x: cy.width() / 2, y: cy.height() / 2 } });
}
function zoomGraph(factor: number): void {
  if (!cy) return;
  const level = Math.min(cy.maxZoom(), Math.max(cy.minZoom(), cy.zoom() * factor));
  cy.zoom({ level, renderedPosition: { x: cy.width() / 2, y: cy.height() / 2 } });
}
function renderGraph(): void {
  if (!cy || !host.value) return;
  const tokens = getComputedStyle(host.value);
  const colors = Object.fromEntries(Object.entries(provenanceTokens).map(([source, token]) => [source, tokens.getPropertyValue(token).trim()]));
  cy.batch(() => {
    cy?.elements().remove();
    cy?.add([
      ...props.nodes.map((node) => ({ data: { id: nodeId(node.key), key: node.key, label: node.name, type: node.type, provenance: node.provenance, color: colors[node.provenance] } })),
      ...props.edges.map((edge) => ({ data: { id: edgeId(edge.key), key: edge.key, source: nodeId(edge.src), target: nodeId(edge.dst), label: edge.rel_type, status: edge.status, provenance: edge.provenance, color: colors[edge.provenance] } })),
    ]);
  });
  applyStyles();
  if (props.nodes.length) {
    cy.layout({ name: "fcose", animate: false, quality: "default", fit: false } as cytoscape.LayoutOptions).run();
    fitGraph();
  }
  highlight();
}
onMounted(() => {
  if (!host.value) return;
  cy = cytoscape({ container: host.value, elements: [], minZoom: 0.15, maxZoom: 4, wheelSensitivity: 0.25, boxSelectionEnabled: false });
  cy.on("tap", "node", (event) => selection.select("node", event.target.data("key")));
  cy.on("tap", "edge", (event) => selection.select("edge", event.target.data("key")));
  cy.on("tap", (event) => { if (event.target === cy) selection.clear(); });
  resize = new ResizeObserver(() => { cy?.resize(); fitGraph(); });
  resize.observe(host.value);
  themeObserver = new MutationObserver(applyStyles);
  themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  themeMedia = window.matchMedia("(prefers-color-scheme: dark)");
  themeMedia.addEventListener("change", applyStyles);
  renderGraph();
});
watch(() => [props.nodes, props.edges], renderGraph, { deep: true });
watch(() => [selection.selection, selection.highlightedNodeKeys], highlight, { deep: true });
onBeforeUnmount(() => {
  resize?.disconnect();
  themeObserver?.disconnect();
  themeMedia?.removeEventListener("change", applyStyles);
  cy?.destroy();
});
</script>

<template>
  <div class="canvas-wrap">
    <div ref="host" class="canvas" role="img" aria-label="实体关系图，可拖动和双指缩放，点击实体或关系查看属性" />
    <p v-if="!nodes.length" class="empty">暂无可见实体</p>
    <div class="canvas-tools"><button type="button" @click="fitGraph">适应画布</button><button type="button" aria-label="放大图谱" @click="zoomGraph(1.2)">＋</button><button type="button" aria-label="缩小图谱" @click="zoomGraph(1 / 1.2)">－</button></div>
    <small class="hint">拖动平移 · 双指缩放</small>
  </div>
</template>
<style scoped>
.canvas-wrap { position: relative; height: 100%; min-height: 280px; background: var(--color-surface-raised); }
.canvas { position: absolute; inset: 0; touch-action: none; }
.empty { position: absolute; top: 40%; width: 100%; text-align: center; pointer-events: none; color: var(--color-text-secondary); }
.canvas-tools { position: absolute; top: 10px; right: 10px; display: flex; gap: 4px; }
button { padding: 6px 10px; background: var(--color-surface); border: 1px solid var(--color-border); color: var(--color-text-primary); border-radius: 4px; cursor: pointer; }
.hint { position: absolute; bottom: 10px; left: 10px; pointer-events: none; color: var(--color-text-secondary); }
</style>
