<template>
  <div
    v-if="view"
    class="mb-6 rounded-xl border border-brass-500/25 bg-brass-500/6 px-4 py-3 text-xs leading-relaxed text-paper-dim"
    role="status"
  >
    <div class="flex flex-wrap items-center justify-between gap-3">
      <span class="font-medium text-brass-300">{{ headline }}</span>
      <button type="button" class="btn-ghost shrink-0 text-xs" :disabled="busy" @click="dismiss">Got it</button>
    </div>
    <ul v-if="view.migrations.length" class="mt-2 space-y-1 font-mono text-[0.6875rem]">
      <li v-for="m in view.migrations" :key="`${m.file}-${m.at}`">
        {{ m.file }}: schema {{ m.from }} → {{ m.to }}<span v-if="m.backup"> · original kept as {{ m.backup }}</span>
      </li>
    </ul>
  </div>
</template>

<script setup lang="ts">
import type { NoticeView } from '~/composables/api'

const view = ref<NoticeView | null>(null)
const busy = ref(false)

const headline = computed(() => {
  const n = view.value?.notice
  if (!n) return 'Your data files were brought up to date.'
  const from = n.from ?? 'an earlier version'
  return n.kind === 'downgraded'
    ? `Ledger POS went back from ${from} to ${n.to}.`
    : `Ledger POS was updated from ${from} to ${n.to}.`
})

async function load() {
  if (!('__TAURI_INTERNALS__' in window)) return
  const { invoke } = await import('@tauri-apps/api/core')
  view.value = await invoke<NoticeView | null>('version_notice')
}

async function dismiss() {
  busy.value = true
  try {
    const { invoke } = await import('@tauri-apps/api/core')
    await invoke('ack_version_notice')
    view.value = null
  } catch (e) {
    toast('error', e instanceof Error ? e.message : String(e))
  } finally {
    busy.value = false
  }
}

onMounted(load)
</script>
