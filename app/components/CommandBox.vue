<template>
  <div class="glass-inset flex items-start gap-2 px-3 py-2.5">
    <code class="min-w-0 flex-1 font-mono text-[0.75rem] leading-relaxed break-all whitespace-pre-wrap text-paper-dim">{{ command }}</code>
    <button type="button" class="shrink-0 cursor-pointer rounded-md px-2 py-0.5 text-[0.6875rem] text-brass-400 transition-colors hover:bg-white/5 hover:text-brass-300" @click="copyText(command)">
      Copy
    </button>
  </div>
</template>

<script setup lang="ts">
defineProps<{ command: string }>()
</script>
