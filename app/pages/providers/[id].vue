<template>
  <div ref="root">
    <div v-if="detail">
      <PageHeader
        :kicker="`Provider · ${detail.metadata.id}`"
        :title="detail.metadata.name"
        :lede="detail.metadata.description"
      >
        <template #actions>
          <div class="flex items-center gap-2">
            <StatusPill v-if="detail.active" tone="brass" label="active" />
            <button v-else class="btn-ghost" type="button" @click="activate">Make active</button>
          </div>
        </template>
      </PageHeader>

      <!-- meta strip -->
      <GlassCard data-reveal class="mb-6">
        <dl class="grid grid-cols-2 gap-x-8 gap-y-4 md:grid-cols-4">
          <div>
            <dt class="data-label mb-1">Vendor</dt>
            <dd class="text-sm text-paper-dim">{{ detail.metadata.vendor }}</dd>
          </div>
          <div>
            <dt class="data-label mb-1">Model</dt>
            <dd class="text-sm text-paper-dim">{{ detail.metadata.model ?? '—' }}</dd>
          </div>
          <div>
            <dt class="data-label mb-1">Transports</dt>
            <dd class="font-mono text-sm text-paper-dim">{{ detail.metadata.transports.join(' / ') }}</dd>
          </div>
          <div>
            <dt class="data-label mb-1">Status</dt>
            <dd class="flex items-center gap-2 text-sm">
              <StatusPill :tone="detail.status.configured ? 'ok' : 'warn'" :label="detail.status.configured ? 'configured' : 'needs setup'" />
              <StatusPill v-if="detail.status.sessionOpen" tone="warn" label="session open" />
            </dd>
          </div>
          <div class="col-span-2 md:col-span-4">
            <dt class="data-label mb-1">Protocol</dt>
            <dd class="text-sm leading-relaxed text-paper-dim">{{ detail.metadata.protocol ?? '—' }}</dd>
          </div>
        </dl>
        <p v-if="detail.status.configurationHint" class="mt-4 rounded-lg border border-warn/25 bg-warn/6 px-3 py-2 text-xs text-warn">
          {{ detail.status.configurationHint }}
        </p>
        <a
          v-if="detail.metadata.docsUrl"
          :href="detail.metadata.docsUrl"
          target="_blank"
          rel="noopener"
          class="mt-4 inline-block text-xs text-brass-400 hover:text-brass-300"
        >
          Provider documentation ↗
        </a>
      </GlassCard>

      <div class="grid items-start gap-6 lg:grid-cols-[380px_1fr]">
        <!-- configuration -->
        <GlassCard data-reveal>
          <h2 class="display mb-1 text-xl text-paper">Configuration</h2>
          <p class="mb-5 text-xs text-paper-mute">Persisted to settings; applied immediately.</p>

          <form class="space-y-4" @submit.prevent="saveConfig">
            <div v-for="(prop, key) in schemaProps" :key="key" class="space-y-1">
              <label :for="`cfg-${key}`" class="flex items-baseline gap-2">
                <span class="font-mono text-[0.8125rem] text-paper">{{ key }}</span>
                <span class="text-[0.6875rem] text-paper-mute">{{ prop.type }}</span>
              </label>

              <TSwitch
                v-if="prop.type === 'boolean'"
                :model-value="Boolean(cfg[key])"
                :label="String(key)"
                @update:model-value="cfg[key] = $event"
              />
              <select v-else-if="prop.enum" :id="`cfg-${key}`" v-model="cfg[key]" class="field">
                <option v-for="v in prop.enum" :key="String(v)" :value="v">{{ v }}</option>
              </select>
              <input
                v-else-if="prop.type === 'integer' || prop.type === 'number'"
                :id="`cfg-${key}`"
                v-model.number="cfg[key]"
                type="number"
                class="field font-mono tabular"
                :placeholder="prop.default !== undefined ? String(prop.default) : ''"
              />
              <input
                v-else
                :id="`cfg-${key}`"
                v-model="cfg[key]"
                class="field font-mono"
                :placeholder="placeholderOf(prop)"
                autocomplete="off"
                spellcheck="false"
              />
              <p class="text-xs leading-relaxed text-paper-mute">{{ prop.description }}</p>
            </div>

            <div class="pt-2">
              <button type="submit" class="btn-primary w-full" :disabled="saving">
                {{ saving ? 'Saving…' : 'Save configuration' }}
              </button>
            </div>
          </form>
        </GlassCard>

        <!-- functions -->
        <div class="min-w-0 space-y-3" data-reveal>
          <div class="flex items-baseline justify-between">
            <h2 class="display text-xl text-paper">Function catalog</h2>
            <span class="text-xs text-paper-mute">{{ detail.functions.length }} functions</span>
          </div>

          <details v-for="f in detail.functions" :key="f.id" class="glass !rounded-xl group">
            <summary class="flex cursor-pointer items-center gap-3 px-5 py-3.5 select-none">
              <svg class="size-3.5 shrink-0 text-paper-mute transition-transform duration-300 group-open:rotate-90" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9 18 6-6-6-6"/></svg>
              <span class="font-mono text-sm text-paper">{{ f.id }}</span>
              <span class="rounded-full border hairline px-2 py-0.5 text-[0.625rem] tracking-wide text-paper-mute uppercase">{{ f.category }}</span>
              <span v-if="f.longRunning" class="text-[0.625rem] tracking-wide text-warn uppercase" title="May block waiting for the cardholder">slow</span>
              <span class="ml-auto hidden truncate text-xs text-paper-mute sm:block">{{ f.summary }}</span>
            </summary>
            <div class="space-y-4 border-t hairline px-5 py-4">
              <p class="text-sm leading-relaxed text-paper-dim">{{ f.description }}</p>

              <div v-if="f.params.length" class="glass-inset overflow-x-auto">
                <table class="w-full text-left text-[0.8125rem]">
                  <thead>
                    <tr class="border-b hairline text-[0.6875rem] tracking-[0.14em] text-paper-mute uppercase">
                      <th class="px-3 py-2 font-medium">Param</th>
                      <th class="px-3 py-2 font-medium">Type</th>
                      <th class="px-3 py-2 font-medium">Req.</th>
                      <th class="px-3 py-2 font-medium">Description</th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr v-for="p in f.params" :key="p.name" class="border-b hairline align-top last:border-0">
                      <td class="px-3 py-2 font-mono text-paper">{{ p.name }}</td>
                      <td class="px-3 py-2 font-mono text-paper-mute">
                        {{ p.type }}<template v-if="p.enum"> ∈ {{ p.enum.join(' | ') }}</template>
                      </td>
                      <td class="px-3 py-2">{{ p.required ? '●' : '' }}</td>
                      <td class="px-3 py-2 text-paper-dim">{{ p.description }}</td>
                    </tr>
                  </tbody>
                </table>
              </div>
              <p v-else class="text-xs text-paper-mute">No parameters.</p>

              <div>
                <div class="data-label mb-1">Returns</div>
                <p class="font-mono text-xs leading-relaxed text-paper-mute">{{ f.returns }}</p>
              </div>

              <div class="flex items-center justify-between gap-3">
                <code class="min-w-0 break-all rounded bg-ink-900 px-2 py-1 font-mono text-[0.6875rem] text-paper-mute">
                  POST /api/v1/providers/{{ detail.metadata.id }}/functions/{{ f.id }}/invoke
                </code>
                <NuxtLink :to="`/operations?provider=${detail.metadata.id}&fn=${f.id}`" class="shrink-0 text-xs text-brass-400 hover:text-brass-300">
                  Try it →
                </NuxtLink>
              </div>
            </div>
          </details>
        </div>
      </div>
    </div>

    <div v-else-if="error" class="glass p-8 text-center">
      <p class="text-danger">{{ error }}</p>
      <NuxtLink to="/providers" class="mt-3 inline-block text-sm text-brass-400 hover:text-brass-300">← Back to providers</NuxtLink>
    </div>
  </div>
</template>

<script setup lang="ts">
import type { ProviderDetail } from '~/composables/api'

interface SchemaProp {
  type: string
  description?: string
  default?: unknown
  enum?: unknown[]
  examples?: unknown[]
}

const api = useApi()
const route = useRoute()
const root = ref<HTMLElement>()
useReveal(root)

const id = computed(() => String(route.params.id))
const detail = ref<ProviderDetail | null>(null)
const error = ref('')
const cfg = reactive<Record<string, unknown>>({})
const saving = ref(false)

const schemaProps = computed<Record<string, SchemaProp>>(() => {
  const props = detail.value?.configSchema?.properties
  return (props && typeof props === 'object' ? props : {}) as Record<string, SchemaProp>
})

function placeholderOf(prop: SchemaProp): string {
  if (prop.examples?.length) return String(prop.examples[0])
  if (prop.default !== undefined) return String(prop.default)
  return ''
}

async function load() {
  try {
    detail.value = await api.provider(id.value)
    for (const k of Object.keys(cfg)) delete cfg[k]
    const current = detail.value.config ?? {}
    for (const [k, prop] of Object.entries(schemaProps.value)) {
      cfg[k] = current[k] ?? prop.default ?? (prop.type === 'boolean' ? false : '')
    }
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Failed to load provider'
  }
}

async function saveConfig() {
  saving.value = true
  try {
    await api.saveProviderConfig(id.value, { ...cfg })
    toast('ok', 'Configuration saved')
    await load()
  } catch (e) {
    toast('error', e instanceof Error ? e.message : 'Failed to save configuration')
  } finally {
    saving.value = false
  }
}

async function activate() {
  try {
    await api.setActive(id.value)
    toast('ok', `“${id.value}” is now the active provider`)
    await load()
  } catch (e) {
    toast('error', e instanceof Error ? e.message : 'Failed to switch provider')
  }
}

onMounted(load)
</script>
