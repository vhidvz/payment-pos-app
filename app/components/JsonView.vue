<template>
  <div class="relative">
    <button
      type="button"
      class="btn-ghost absolute top-2.5 right-2.5 z-10 !px-2.5 !py-1 text-[0.6875rem]"
      @click="copyText(pretty)"
    >
      Copy
    </button>
    <pre class="code-block max-h-[420px] overflow-auto" tabindex="0"><code>{{ pretty }}</code></pre>
  </div>
</template>

<script setup lang="ts">
const props = defineProps<{ value: unknown }>()
const pretty = computed(() => JSON.stringify(props.value, null, 2))
</script>
