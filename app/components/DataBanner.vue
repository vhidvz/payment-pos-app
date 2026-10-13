<template>
  <div
    v-if="lines.length"
    class="mb-6 rounded-xl border border-warn/25 bg-warn/6 px-4 py-3 text-xs leading-relaxed text-warn"
    role="alert"
  >
    <p v-for="line in lines" :key="line">{{ line }}</p>
  </div>
</template>

<script setup lang="ts">
import type { FileStatus, SystemInfo } from '~/composables/api'

const api = useApi()
const boot = useBootInfo()
const apiVersion = useApiVersion()
const system = ref<SystemInfo | null>(null)

const MESSAGES: Record<string, Partial<Record<FileStatus, string>>> = {
  settings: {
    readOnlyNewer: 'Settings were written by a newer Ledger POS — changes are refused until you open that version or restore the .bak backup beside settings.json.',
    recovered: 'settings.json could not be read; it was kept as settings.json.invalid and defaults are in use.',
  },
  admin: {
    failClosed: 'The administrator file is from a newer Ledger POS — the console stays locked. Open that version.',
  },
  outbox: {
    readOnlyNewer: 'The webhook outbox belongs to a newer Ledger POS — it is left untouched; this run queues events in memory only.',
    recovered: 'The webhook outbox could not be read; it was kept as events-outbox.json.invalid.',
  },
  state: {
    readOnlyNewer: 'The run state belongs to a newer Ledger POS — update notices are not tracked this run.',
    recovered: 'The run state could not be read; it was kept as state.json.invalid.',
  },
}

const lines = computed(() => {
  const out: string[] = []
  const data = system.value?.data
  if (data) {
    for (const [file, status] of Object.entries(data)) {
      const msg = MESSAGES[file]?.[status as FileStatus]
      if (msg) out.push(msg)
    }
  }
  const mine = boot.value?.current
  if (mine && apiVersion.value && apiVersion.value !== mine) {
    out.push(`The API on this port is served by Ledger POS ${apiVersion.value}, not this window's ${mine} — restart it (posd, or the other copy) to finish the update.`)
  }
  return out
})

let timer: ReturnType<typeof setInterval> | undefined
async function load() {
  try {
    system.value = await api.system()
  } catch {
    /* the layout's poll reports an unreachable API */
  }
}
onMounted(() => {
  load()
  timer = setInterval(load, 30_000)
})
onBeforeUnmount(() => clearInterval(timer))
</script>
