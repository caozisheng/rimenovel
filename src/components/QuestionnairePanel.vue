<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { NAlert, NButton, NInput, NTag } from "naive-ui";
import { parseQuestionnaireSubmission, type QuestionBatch, type QuestionnaireRecord, type QuestionnaireSubmission } from "../lib/wiki";

const props = withDefaults(defineProps<{
  batch: QuestionBatch | null;
  records?: readonly QuestionnaireRecord[];
  loading?: boolean;
  submitting?: boolean;
  error?: string | null;
  answeredCount?: number;
  effectiveAnswerCount?: number;
}>(), { records: () => [], loading: false, submitting: false, error: null });
const emit = defineEmits<{
  submit: [submission: QuestionnaireSubmission];
  end: [submission: QuestionnaireSubmission];
  change: [submission: QuestionnaireSubmission];
}>();
const selections = ref<Record<string, string>>({});
const freeText = ref("");
const answered = computed(() => props.answeredCount ?? props.batch?.answered_count ?? props.records.reduce((total, record) => total + record.answers.length, 0));
const effective = computed(() => props.effectiveAnswerCount ?? props.batch?.effective_answer_count ?? props.records.reduce((total, record) => total + record.answers.filter(answer => answer.status === "effective").length, 0));
const selectedCount = computed(() => Object.keys(selections.value).length);
const invalidReasons = computed(() => {
  const reasons: Record<string, string> = {};
  for (const id of props.batch?.invalid_option_ids ?? []) reasons[id] = "该选项已被判定无效";
  return { ...reasons, ...props.batch?.invalid_reasons };
});

function submission(): QuestionnaireSubmission {
  return parseQuestionnaireSubmission({
    batch_index: props.batch?.batch_index ?? null,
    question_batch_id: props.batch?.question_batch_id ?? null,
    selected_options: (props.batch?.questions ?? []).flatMap(question => {
      const option = selections.value[question.id];
      return option === undefined ? [] : [{ question_id: question.id, option_id: option }];
    }),
    free_text: freeText.value,
  }, props.batch);
}
function select(questionId: string, optionId: string): void {
  selections.value = { ...selections.value, [questionId]: optionId };
  emit("change", submission());
}
function clear(questionId: string): void {
  const next = { ...selections.value };
  delete next[questionId];
  selections.value = next;
  emit("change", submission());
}
function updateText(value: string): void {
  freeText.value = value;
  emit("change", submission());
}
watch([() => props.batch?.question_batch_id, () => props.batch?.batch_index], () => { selections.value = {}; });
</script>

<template>
  <section class="questionnaire-panel" aria-label="下一章选择">
    <header>
      <h3>下一章选择</h3>
      <p>每批 3 个模糊问题，可回答 0–3 题，也可随时结束。文字补充与选项冲突时，以文字为准。</p>
      <div class="counts" aria-live="polite">
        <NTag>已回答 {{ answered }}</NTag>
        <NTag type="success">有效答案 {{ effective }}</NTag>
        <NTag v-if="batch">本批已选 {{ selectedCount }} / 3</NTag>
      </div>
    </header>

    <NAlert v-if="error" type="error" :show-icon="false">{{ error }}</NAlert>
    <p v-if="loading" role="status">正在准备模糊问题，你仍可结束选择并继续阅读。</p>
    <p v-else-if="!batch" class="muted">暂无可用问题。可以只填写文字补充，或直接继续阅读。</p>

    <div v-if="batch" class="questions">
      <p class="muted">第 {{ batch.batch_index + 1 }} 批 · 题目与选项仅展示模糊方向，不揭示下一章 Wiki。</p>
      <fieldset v-for="(question, index) in batch.questions" :key="question.id" :disabled="submitting">
        <legend>{{ index + 1 }}. {{ question.prompt }}</legend>
        <label v-for="option in question.options" :key="option.id" class="option" :class="{ invalid: invalidReasons[option.id] }">
          <input type="radio" :name="`${batch.question_batch_id}-${question.id}`" :value="option.id" :checked="selections[question.id] === option.id" @change="select(question.id, option.id)" />
          <span class="option-content">
            <span class="option-label">{{ option.label }}</span>
            <small v-if="invalidReasons[option.id]" class="invalid-reason">无效 · {{ invalidReasons[option.id] }}</small>
          </span>
        </label>
        <NButton size="small" quaternary :disabled="!selections[question.id] || submitting" :data-clear="question.id" @click="clear(question.id)">跳过此题 / 清除选择</NButton>
      </fieldset>
    </div>

    <section v-if="records.length" class="answer-history" aria-label="已提交答案">
      <h4>已提交答案</h4>
      <article v-for="record in records" :key="record.question_batch_id" class="batch-record">
        <h5>第 {{ record.batch_index + 1 }} 批</h5>
        <p v-if="!record.answers.length" class="muted">本批未选择选项</p>
        <ul v-else>
          <li v-for="answer in record.answers" :key="answer.question_id" :class="{ invalid: answer.status !== 'effective' }">
            <span>{{ answer.prompt }}：</span>
            <span class="option-label">{{ answer.label }}</span>
            <NTag size="small" :type="answer.status === 'effective' ? 'success' : 'warning'">{{ answer.status === 'effective' ? '有效' : answer.status === 'overridden' ? '被文字覆盖 · 无效' : '无效' }}</NTag>
            <p v-if="answer.reason" class="invalid-reason">{{ answer.reason }}</p>
          </li>
        </ul>
        <p v-if="record.free_text" class="record-text">文字补充：{{ record.free_text }}</p>
      </article>
    </section>

    <label class="free-text">
      <span>文字补充（可选）</span>
      <NInput type="textarea" :value="freeText" :autosize="{ minRows: 3, maxRows: 8 }" placeholder="说说你希望下一章如何发展；与选项冲突时以这段文字为准。" @update:value="updateText" />
    </label>
    <p class="muted">有效答案数量由提交后的结果确认；被覆盖的选项会保留显示，并标记为无效。</p>

    <footer>
      <NButton v-if="batch" type="primary" :loading="submitting" :disabled="loading || submitting" data-action="submit" @click="emit('submit', submission())">{{ batch.has_more ? '提交本批，继续选择' : '提交本批' }}</NButton>
      <NButton data-action="end" @click="emit('end', submission())">结束选择，继续阅读</NButton>
    </footer>
  </section>
</template>

<style scoped>
.questionnaire-panel { display: grid; gap: 16px; color: var(--color-text-primary); }
h3, h4, h5 { margin: 0 0 8px; }
header p, .muted { color: var(--color-text-secondary); font-size: 13px; line-height: 1.6; }
.counts, footer { display: flex; flex-wrap: wrap; gap: 8px; }
.questions { display: grid; gap: 12px; }
fieldset { min-width: 0; margin: 0; padding: 12px; border: 1px solid var(--color-border); border-radius: 6px; }
legend { padding: 0 4px; font-weight: 600; overflow-wrap: anywhere; }
.option { display: flex; align-items: flex-start; gap: 8px; padding: 8px 0; cursor: pointer; }
.option input { margin-top: 5px; accent-color: var(--color-accent); }
.option-content { display: grid; gap: 4px; }
.invalid .option-label { text-decoration: line-through; color: var(--color-text-secondary); }
.invalid-reason { display: block; margin: 4px 0; color: var(--color-text-secondary); font-size: 13px; }
.batch-record { padding: 12px; margin-top: 8px; border: 1px solid var(--color-border); border-radius: 6px; }
.batch-record ul { padding-left: 20px; }
.batch-record li { margin: 8px 0; overflow-wrap: anywhere; }
.batch-record li :deep(.n-tag) { margin-left: 8px; }
.record-text { white-space: pre-wrap; overflow-wrap: anywhere; }
.free-text { display: grid; gap: 8px; }
footer { position: sticky; bottom: 0; padding: 12px 0; background: var(--color-surface); }
</style>
