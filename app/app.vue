<template>
  <UpdateScreen v-if="boot && boot.mode !== 'normal'" :boot="boot" />
  <NuxtLayout v-else-if="boot">
    <NuxtPage />
  </NuxtLayout>
</template>

<script setup lang="ts">
const boot = useBootInfo()

onMounted(async () => {
  // Ask the shell first: an update screen talks to no API at all.
  boot.value = await loadBootInfo()
  if (boot.value.mode === 'normal') await resolveApiBase()
})
</script>
