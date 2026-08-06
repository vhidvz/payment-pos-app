<template>
  <header class="mb-10">
    <p class="kicker mb-3" data-reveal>{{ kicker }}</p>
    <h1 class="display text-[2.6rem] leading-[1.08] text-paper" data-reveal>
      {{ title }}
    </h1>
    <div class="mt-4 flex items-end justify-between gap-6">
      <p v-if="lede" class="max-w-[58ch] text-[0.95rem] leading-relaxed text-paper-dim" data-reveal>
        {{ lede }}
      </p>
      <div v-if="$slots.actions" class="shrink-0" data-reveal>
        <slot name="actions" />
      </div>
    </div>
  </header>
</template>

<script setup lang="ts">
defineProps<{ kicker: string; title: string; lede?: string }>()
</script>
