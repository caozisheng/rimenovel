import { defineStore } from "pinia";
import { ref } from "vue";
import { eventNodeKeys, type ElementKind, type GraphEvent } from "./graph";

export const useGraphSelectionStore = defineStore("graph-selection", () => {
  const selection = ref<{ kind: ElementKind; key: string } | null>(null);
  const highlightedNodeKeys = ref<string[]>([]);
  function select(kind: ElementKind, key: string): void {
    selection.value = { kind, key };
    highlightedNodeKeys.value = [];
  }
  function selectEvent(event: GraphEvent): void {
    selection.value = { kind: "event", key: event.key };
    highlightedNodeKeys.value = eventNodeKeys(event);
  }
  function clear(): void {
    selection.value = null;
    highlightedNodeKeys.value = [];
  }
  return { selection, highlightedNodeKeys, select, selectEvent, clear };
});
