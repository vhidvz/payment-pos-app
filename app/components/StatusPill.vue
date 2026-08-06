<template>
  <span
    class="inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[0.6875rem] font-medium tracking-wide uppercase"
    :class="cls"
  >
    <span class="size-1.5 rounded-full bg-current" aria-hidden="true" />
    {{ label }}
  </span>
</template>

<script setup lang="ts">
const props = defineProps<{ tone: 'ok' | 'danger' | 'warn' | 'muted' | 'brass'; label: string }>()

const cls = computed(
  () =>
    ({
      ok: 'text-ok border-ok/30 bg-ok/5',
      danger: 'text-danger border-danger/30 bg-danger/5',
      warn: 'text-warn border-warn/30 bg-warn/5',
      muted: 'text-paper-mute border-line-bright bg-white/2',
      brass: 'text-brass-300 border-brass-500/35 bg-brass-500/8',
    })[props.tone],
)
</script>
