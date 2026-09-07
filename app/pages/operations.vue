<template>
  <div ref="root">
    <PageHeader
      kicker="Operations · 03"
      title="Run the terminal."
      lede="Pick a provider and a function; the form below is generated from its live parameter spec — the same contract the REST API publishes."
    />

    <LockBanner what="Manual terminal operations" />

    <div class="grid items-start gap-6 lg:grid-cols-[300px_1fr]">
      <!-- picker column -->
      <div class="space-y-4" data-reveal>
        <GlassCard :pad="false" class="p-4">
          <label for="op-provider" class="data-label mb-2 block">Provider</label>
          <select id="op-provider" v-model="selectedProvider" class="field">
            <option v-for="p in providers" :key="p.id" :value="p.id">
              {{ p.name }}{{ p.active ? ' · active' : '' }}
            </option>
          </select>
        </GlassCard>

        <GlassCard :pad="false" class="max-h-[520px] overflow-y-auto p-2">
          <div v-for="(fns, cat) in grouped" :key="cat" class="mb-1">
            <div class="px-3 pt-3 pb-1.5 text-[0.625rem] font-medium tracking-[0.24em] text-paper-mute uppercase">{{ cat }}</div>
            <button
              v-for="f in fns"
              :key="f.id"
              type="button"
              class="fn-item"
              :class="{ 'fn-active': selectedFn?.id === f.id }"
              @click="selectFn(f.id)"
            >
              <span class="truncate font-mono text-[0.8125rem]">{{ f.id }}</span>
              <span v-if="f.longRunning" class="ml-auto shrink-0 text-[0.5625rem] tracking-wider text-warn uppercase">slow</span>
            </button>
          </div>
        </GlassCard>
      </div>

      <!-- runner column -->
      <div class="space-y-6">
        <GlassCard v-if="selectedFn" data-reveal>
          <div class="mb-1 flex items-baseline justify-between gap-4">
            <h2 class="display text-2xl text-paper">{{ selectedFn.title }}</h2>
            <span class="rounded-full border hairline px-2.5 py-0.5 text-[0.625rem] tracking-wide text-paper-mute uppercase">{{ selectedFn.category }}</span>
          </div>
          <p class="mb-5 max-w-[64ch] text-sm leading-relaxed text-paper-dim">{{ selectedFn.description }}</p>

          <FnForm :fn="selectedFn" :busy="busy || locked" @invoke="run" />

          <p v-if="locked" class="mt-4 rounded-lg border border-warn/25 bg-warn/6 px-3 py-2 text-xs leading-relaxed text-warn">
            Running functions by hand is locked. Unlock to continue.
          </p>
        </GlassCard>

        <div v-if="lastResult && selectedFn" aria-live="polite">
          <ReceiptCard
            :result="lastResult"
            :provider="selectedProvider"
            :fn-id="lastFnId"
            :duration-ms="lastDuration"
          />
        </div>
        <div v-else-if="lastError" class="glass border-danger/30 p-5" role="alert">
          <div class="mb-1 flex items-center gap-2 text-sm font-medium text-danger">
            <span class="size-2 rounded-full bg-danger" />
            {{ lastError.code }}
          </div>
          <p class="text-sm text-paper-dim">{{ lastError.message }}</p>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import type { FunctionSpec, ProviderSummary } from '~/composables/api'
import { ApiError } from '~/composables/api'

const api = useApi()
const locked = useLocked()
const route = useRoute()
const router = useRouter()
const root = ref<HTMLElement>()
useReveal(root)

const providers = ref<ProviderSummary[]>([])
const functions = ref<FunctionSpec[]>([])
const selectedProvider = ref('')
const selectedFnId = ref('')
const busy = ref(false)
const lastResult = ref<Record<string, unknown> | null>(null)
const lastError = ref<{ code: string; message: string } | null>(null)
const lastDuration = ref<number>()
const lastFnId = ref('')

const selectedFn = computed(() => functions.value.find((f) => f.id === selectedFnId.value) ?? null)

const CATEGORY_ORDER = ['transaction', 'inquiry', 'report', 'diagnostics', 'session', 'utility']
const grouped = computed(() => {
  const out: Record<string, FunctionSpec[]> = {}
  for (const cat of CATEGORY_ORDER) {
    const fns = functions.value.filter((f) => f.category === cat)
    if (fns.length) out[cat] = fns
  }
  for (const f of functions.value) {
    if (!CATEGORY_ORDER.includes(f.category)) (out[f.category] ??= []).push(f)
  }
  return out
})

async function loadProviders() {
  providers.value = await api.providers()
  const fromQuery = String(route.query.provider ?? '')
  selectedProvider.value =
    providers.value.find((p) => p.id === fromQuery)?.id ??
    providers.value.find((p) => p.active)?.id ??
    providers.value[0]?.id ??
    ''
}

async function loadFunctions() {
  if (!selectedProvider.value) return
  functions.value = await api.get<FunctionSpec[]>(
    `/api/v1/providers/${encodeURIComponent(selectedProvider.value)}/functions`,
  )
  const fromQuery = String(route.query.fn ?? '')
  if (functions.value.some((f) => f.id === fromQuery)) {
    selectedFnId.value = fromQuery
  } else if (!functions.value.some((f) => f.id === selectedFnId.value)) {
    selectedFnId.value = functions.value[0]?.id ?? ''
  }
}

function selectFn(id: string) {
  selectedFnId.value = id
  lastResult.value = null
  lastError.value = null
  router.replace({ query: { provider: selectedProvider.value, fn: id } })
}

watch(selectedProvider, () => {
  lastResult.value = null
  lastError.value = null
  loadFunctions().catch(() => toast('error', 'Failed to load functions'))
})

async function run(params: Record<string, unknown>) {
  if (!selectedFn.value) return
  // A UI gate, not a security boundary: the same call over the REST API stays
  // open on purpose so the till keeps working while the app is locked.
  if (locked.value) {
    requestUnlock()
    return
  }
  busy.value = true
  lastError.value = null
  lastResult.value = null
  lastFnId.value = selectedFn.value.id
  const started = performance.now()
  try {
    lastResult.value = await api.invoke(selectedProvider.value, selectedFn.value.id, params)
  } catch (e) {
    if (e instanceof ApiError) lastError.value = { code: e.code, message: e.message }
    else lastError.value = { code: 'error', message: e instanceof Error ? e.message : 'Invocation failed' }
  } finally {
    lastDuration.value = performance.now() - started
    busy.value = false
  }
}

onMounted(async () => {
  try {
    await loadProviders()
    await loadFunctions()
  } catch (e) {
    toast('error', e instanceof Error ? e.message : 'Failed to load providers')
  }
})
</script>

<style scoped>
.fn-item {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  width: 100%;
  border-radius: 0.6rem;
  padding: 0.45rem 0.75rem;
  color: var(--color-paper-dim);
  cursor: pointer;
  transition: background 0.2s, color 0.2s;
  text-align: left;
}
.fn-item:hover {
  background: rgb(255 255 255 / 0.04);
  color: var(--color-paper);
}
.fn-active {
  background: linear-gradient(90deg, rgb(201 160 85 / 0.14), rgb(201 160 85 / 0.03));
  color: var(--color-brass-300);
  box-shadow: inset 2px 0 0 var(--color-brass-500);
}
</style>
