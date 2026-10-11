<script setup lang="ts">
import { NButton, NTag } from "naive-ui";
import type { FogCounts, GraphPurpose } from "../lib/graph";
defineProps<{ fog: FogCounts; revealed: boolean; purpose: GraphPurpose; canReveal?: boolean }>();
const emit = defineEmits<{ reveal: [] }>();
</script>
<template><section class="fog" aria-label="未揭示内容"><div><NTag type="warning">{{ purpose === 'spec' ? '未来规格' : '部分内容未揭示' }}</NTag><span v-if="!revealed">实体 {{ fog.entities }} · 关系 {{ fog.relations }} · 事件 {{ fog.events }} 尚未揭示</span><span v-else>可见内容已揭示</span></div><NButton v-if="!revealed && purpose === 'spec' && canReveal" size="small" @click="emit('reveal')">揭示下一章</NButton></section></template>
<style scoped>.fog { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 8px 12px; border: 1px solid var(--color-border); color: var(--color-text-secondary); font-size: 13px; }.fog > div { display: flex; align-items: center; gap: 8px; }</style>
