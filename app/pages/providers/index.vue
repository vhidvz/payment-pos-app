<template>
  <div ref="root">
    <PageHeader
      kicker="Providers · 02"
      title="Interchangeable engines."
      lede="Each provider drives a family of terminals behind the same self-describing interface. Select one as active, configure it, and every function becomes available over REST."
    />

    <div class="grid gap-5 lg:grid-cols-2">
      <GlassCard v-for="p in providers" :key="p.id" data-reveal class="hover-glow flex flex-col">
        <div class="mb-3 flex items-start justify-between gap-3">
          <div>
            <h2 class="display text-2xl text-paper">{{ p.name }}</h2>
            <p class="mt-0.5 text-xs text-paper-mute">{{ p.vendor }}<span v-if="p.model"> · {{ p.model }}</span> · v{{ p.version }}</p>
          </div>
          <div class="flex shrink-0 flex-col items-end gap-1.5">
            <StatusPill v-if="p.active" tone="brass" label="active" />
            <StatusPill
              :tone="p.status.configured ? 'ok' : 'warn'"
              :label="p.status.configured ? 'configured' : 'needs setup'"
            />
          </div>
        </div>

        <p class="mb-4 text-sm leading-relaxed text-paper-dim">{{ p.description }}</p>

        <div class="mb-5 flex flex-wrap gap-1.5">
          <span v-for="c in p.capabilities" :key="c" class="rounded-full border hairline bg-white/2 px-2.5 py-0.5 text-[0.6875rem] text-paper-mute">
            {{ c }}
          </span>
        </div>

        <div class="mt-auto flex items-center justify-between border-t hairline pt-4">
          <span class="font-mono text-xs text-paper-mute tabular">
            {{ p.functionCount }} functions · {{ p.transports.join(' / ') }}
          </span>
          <div class="flex gap-2">
            <button v-if="!p.active" class="btn-ghost !py-1.5 text-xs" type="button" @click="activate(p.id)">
              Make active
            </button>
            <NuxtLink :to="`/providers/${p.id}`" class="btn-primary !py-1.5 text-xs">Open</NuxtLink>
          </div>
        </div>
      </GlassCard>
    </div>
  </div>
</template>

<script setup lang="ts">
import type { ProviderSummary } from '~/composables/api'

const api = useApi()
const root = ref<HTMLElement>()
useReveal(root)

const providers = ref<ProviderSummary[]>([])

async function refresh() {
  try {
    providers.value = await api.providers()
  } catch (e) {
    toast('error', e instanceof Error ? e.message : 'Failed to load providers')
  }
}

async function activate(id: string) {
  try {
    await api.setActive(id)
    toast('ok', `“${id}” is now the active provider`)
    refresh()
  } catch (e) {
    toast('error', e instanceof Error ? e.message : 'Failed to switch provider')
  }
}

onMounted(refresh)
</script>
