<template>
  <div ref="root">
    <PageHeader
      kicker="Overview · 01"
      title="The terminal, on your desk."
      lede="Every capability of your payment terminals, served as a local REST API — always on, fully documented, provider-agnostic."
    >
      <template #actions>
        <button class="btn-ghost" type="button" @click="openApiDocs()">Open Swagger docs</button>
      </template>
    </PageHeader>

    <!-- status band -->
    <div class="mb-6 grid gap-4 md:grid-cols-3">
      <GlassCard data-reveal class="hover-glow">
        <div class="mb-3 flex items-center justify-between">
          <span class="data-label">REST API</span>
          <StatusPill :tone="sys?.server.running ? 'ok' : 'danger'" :label="sys?.server.running ? 'serving' : 'offline'" />
        </div>
        <div class="font-mono text-lg text-paper tabular">:{{ sys?.server.port ?? '—' }}</div>
        <button
          class="mt-1 max-w-full cursor-pointer truncate text-left font-mono text-xs text-paper-mute transition-colors hover:text-brass-300"
          :title="`${base} — click to copy`"
          type="button"
          @click="copyText(base)"
        >
          {{ base }}
        </button>
        <p v-if="sys?.server.error" class="mt-2 text-xs text-danger">{{ sys.server.error }}</p>
      </GlassCard>

      <GlassCard data-reveal class="hover-glow">
        <div class="mb-3 flex items-center justify-between">
          <span class="data-label">Active provider</span>
          <StatusPill
            v-if="activeProvider"
            :tone="activeProvider.status.configured ? 'brass' : 'warn'"
            :label="activeProvider.status.configured ? 'ready' : 'needs setup'"
          />
        </div>
        <div class="text-lg text-paper">{{ activeProvider?.name ?? sys?.activeProvider ?? '—' }}</div>
        <div class="mt-1 text-xs text-paper-mute">
          {{ activeProvider ? `${activeProvider.vendor} · ${activeProvider.functionCount} functions` : ' ' }}
        </div>
        <NuxtLink v-if="activeProvider" :to="`/providers/${activeProvider.id}`" class="mt-2 inline-block text-xs text-brass-400 hover:text-brass-300">
          Configure →
        </NuxtLink>
      </GlassCard>

      <GlassCard data-reveal class="hover-glow">
        <div class="mb-3 flex items-center justify-between">
          <span class="data-label">Session</span>
          <span class="font-mono text-[0.6875rem] text-paper-mute">v{{ sys?.version ?? '—' }}</span>
        </div>
        <div class="font-mono text-lg text-paper tabular">{{ uptime }}</div>
        <div class="mt-1 text-xs text-paper-mute">uptime · {{ sys?.platform }}/{{ sys?.arch }}</div>
        <button class="mt-2 cursor-pointer text-left text-xs text-brass-400 hover:text-brass-300" type="button" :disabled="testing" @click="runConnectionTest">
          {{ testing ? 'Testing…' : 'Run connection test →' }}
        </button>
      </GlassCard>
    </div>

    <!-- connection test result -->
    <div v-if="testResult" class="mb-6" data-reveal>
      <ReceiptCard :result="testResult" :provider="sys?.activeProvider ?? ''" fn-id="connectionTest" :duration-ms="testDuration" />
    </div>

    <!-- activity -->
    <GlassCard data-reveal :pad="false">
      <div class="flex items-center justify-between px-6 pt-5 pb-3">
        <h2 class="display text-xl text-paper">Recent activity</h2>
        <span class="text-xs text-paper-mute">last {{ entries.length }} calls</span>
      </div>
      <div v-if="!entries.length" class="px-6 pb-8 pt-2 text-sm text-paper-mute">
        Nothing yet. Invoke a function from
        <NuxtLink to="/operations" class="text-brass-400 hover:text-brass-300">Operations</NuxtLink>
        or call the API directly — every invocation lands here.
      </div>
      <ul v-else class="divide-y divide-white/5">
        <li v-for="e in entries" :key="e.id" class="flex items-center gap-4 px-6 py-3">
          <span class="size-2 shrink-0 rounded-full" :class="e.ok ? 'bg-ok' : 'bg-danger'" />
          <div class="min-w-0 flex-1">
            <div class="flex items-baseline gap-2">
              <span class="font-mono text-[0.8125rem] text-paper">{{ e.function }}</span>
              <span class="text-[0.6875rem] text-paper-mute">{{ e.provider }}</span>
            </div>
            <div class="truncate text-xs text-paper-mute">
              {{ e.error ?? e.summary ?? (e.ok ? 'ok' : 'failed') }}
            </div>
          </div>
          <div class="shrink-0 text-right">
            <div class="font-mono text-xs text-paper-dim tabular">{{ (e.durationMs / 1000).toFixed(2) }}s</div>
            <div class="text-[0.6875rem] text-paper-mute">{{ timeOf(e.timestamp) }}</div>
          </div>
        </li>
      </ul>
    </GlassCard>
  </div>
</template>

<script setup lang="ts">
import type { ActivityEntry, ProviderSummary, SystemInfo } from '~/composables/api'

const api = useApi()
const base = useApiBase()
const root = ref<HTMLElement>()
useReveal(root)

const sys = ref<SystemInfo | null>(null)
const providers = ref<ProviderSummary[]>([])
const entries = ref<ActivityEntry[]>([])
const testing = ref(false)
const testResult = ref<Record<string, unknown> | null>(null)
const testDuration = ref<number>()

const activeProvider = computed(() => providers.value.find((p) => p.active) ?? null)

const uptime = computed(() => {
  const s = sys.value ? upSecs.value : 0
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  return h ? `${h}h ${m}m` : `${m}m ${s % 60}s`
})
const upSecs = ref(0)

async function refresh() {
  try {
    const [s, p, a] = await Promise.all([api.system(), api.providers(), api.activity(8)])
    sys.value = s
    providers.value = p
    entries.value = a
  } catch {
    /* layout badge already reflects unreachability */
  }
  try {
    const h = await api.health()
    upSecs.value = h.uptimeSecs
  } catch { /* ignore */ }
}

async function runConnectionTest() {
  if (!sys.value) return
  testing.value = true
  testResult.value = null
  const started = performance.now()
  try {
    testResult.value = await api.invoke(sys.value.activeProvider, 'connectionTest', {})
  } catch (e) {
    toast('error', e instanceof Error ? e.message : 'Connection test failed')
  } finally {
    testDuration.value = performance.now() - started
    testing.value = false
    refresh()
  }
}

function timeOf(ts: string) {
  try {
    return new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
  } catch {
    return ts
  }
}

let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  refresh()
  timer = setInterval(refresh, 6000)
})
onBeforeUnmount(() => clearInterval(timer))
</script>
