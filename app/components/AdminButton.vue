<template>
  <!-- An action the computer's administrator authorises: the shield says so before the click. -->
  <button
    type="button"
    :class="[danger ? 'btn-danger' : primary ? 'btn-primary' : 'btn-ghost', small && '!px-2.5 !py-1 !text-xs']"
    :title="'Needs the computer\'s administrator password'"
  >
    <svg class="size-3.5 shrink-0 opacity-80" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
    </svg>
    <slot />
  </button>
</template>

<script setup lang="ts">
defineProps<{ primary?: boolean; danger?: boolean; small?: boolean }>()
</script>
