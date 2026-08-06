<template>
  <button
    type="button"
    role="switch"
    :aria-checked="modelValue"
    :aria-label="label"
    class="relative h-6 w-11 shrink-0 cursor-pointer rounded-full border transition-colors duration-300"
    :class="modelValue ? 'border-brass-500/60 bg-brass-500/30' : 'border-line-bright bg-ink-800'"
    @click="$emit('update:modelValue', !modelValue)"
  >
    <span
      class="absolute top-1/2 left-0.5 size-4.5 -translate-y-1/2 rounded-full transition-transform duration-300"
      :class="modelValue ? 'translate-x-[20px] bg-brass-300' : 'translate-x-0 bg-paper-mute'"
      style="transition-timing-function: var(--ease-swift)"
    />
  </button>
</template>

<script setup lang="ts">
defineProps<{ modelValue: boolean; label: string }>()
defineEmits<{ 'update:modelValue': [value: boolean] }>()
</script>
