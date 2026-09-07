<template>
  <div
    v-if="locked"
    class="mb-6 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-warn/25 bg-warn/6 px-4 py-3"
  >
    <div class="text-xs leading-relaxed text-warn">
      <span class="font-medium">{{ what }} are locked.</span>
      Unlock with the application password to make changes.
    </div>
    <button type="button" class="btn-ghost shrink-0 text-xs" @click="requestUnlock()">Unlock</button>
  </div>
</template>

<script setup lang="ts">
withDefaults(defineProps<{ what?: string }>(), { what: 'Changes' })
const locked = useLocked()
</script>
