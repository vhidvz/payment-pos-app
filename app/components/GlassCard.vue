<template>
  <section class="glass" :class="pad ? 'p-6' : ''">
    <slot />
  </section>
</template>

<script setup lang="ts">
withDefaults(defineProps<{ pad?: boolean }>(), { pad: true })
</script>
