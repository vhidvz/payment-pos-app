<template>
  <div class="mb-5">
    <div class="mb-1.5 flex items-center justify-between gap-4">
      <h2 class="display text-xl text-paper">{{ title }}</h2>
      <StatusPill v-if="state" :tone="tone ?? 'muted'" :label="state" />
    </div>
    <p v-if="$slots.default" class="max-w-[68ch] text-xs leading-relaxed text-paper-mute">
      <slot />
    </p>
  </div>
</template>

<script setup lang="ts">
defineProps<{ title: string; state?: string; tone?: 'ok' | 'danger' | 'warn' | 'muted' | 'brass' }>()
</script>
