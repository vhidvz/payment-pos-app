<template>
  <GlassCard v-if="!hidden" data-reveal class="mb-6">
    <div class="mb-1 flex items-start justify-between gap-4">
      <div>
        <h2 class="display text-xl text-paper">Getting started</h2>
        <p class="mt-1 text-xs text-paper-mute">
          Everything Ledger POS can do, in the order you will need it. Each step ticks itself when it is done.
        </p>
      </div>
      <div class="flex shrink-0 items-center gap-3">
        <span class="font-mono text-xs text-paper-mute tabular">{{ doneCount }}/{{ steps.length }}</span>
        <button type="button" class="text-xs text-paper-mute hover:text-paper-dim" @click="setHidden(true)">Hide</button>
      </div>
    </div>
    <div class="mb-5 h-1 overflow-hidden rounded-full bg-white/5" aria-hidden="true">
      <div class="h-full rounded-full bg-brass-500 transition-all duration-700" :style="{ width: `${(doneCount / steps.length) * 100}%` }" />
    </div>

    <div class="grid gap-x-8 gap-y-1 md:grid-cols-2">
      <div v-for="col in columns" :key="col.title">
        <div class="data-label mb-2">{{ col.title }}</div>
        <ol class="space-y-1">
          <li v-for="s in col.steps" :key="s.title">
            <component
              :is="s.to ? NuxtLink : 'div'"
              :to="s.to"
              class="step"
              :class="s.done && 'step-done'"
            >
              <span class="step-mark" aria-hidden="true">
                <svg v-if="s.done" class="size-3" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5" /></svg>
              </span>
              <span class="min-w-0">
                <span class="block text-sm text-paper">{{ s.title }}<span v-if="s.done" class="sr-only"> (done)</span></span>
                <span class="block text-xs leading-relaxed text-paper-mute">{{ s.text }}</span>
              </span>
              <span v-if="s.to" class="ml-auto shrink-0 self-center text-xs text-brass-400">→</span>
            </component>
          </li>
        </ol>
      </div>
    </div>
  </GlassCard>
  <button v-else type="button" class="mb-6 text-xs text-brass-400 hover:text-brass-300" @click="setHidden(false)">
    Show getting started
  </button>
</template>

<script setup lang="ts">
import type { ActivityEntry, EventsStatus, ProviderSummary, Settings } from '~/composables/api'

const props = defineProps<{ providers: ProviderSummary[]; activity: ActivityEntry[] }>()

const NuxtLink = resolveComponent('NuxtLink')
const api = useApi()
const auth = useAuth()
const { info: install } = useInstall()

const settings = ref<Settings | null>(null)
const events = ref<EventsStatus | null>(null)
const active = computed(() => props.providers.find((p) => p.active) ?? null)
const realTerminal = computed(() => props.providers.find((p) => p.id !== 'sandbox') ?? null)
const { status: link } = useProviderLink(computed(() => (active.value && active.value.id !== 'sandbox' ? active.value.id : '')), 30000)

const STORAGE = 'ledger-pos:getting-started-hidden'
const hidden = ref(false)
function setHidden(v: boolean) {
  hidden.value = v
  try {
    localStorage.setItem(STORAGE, v ? '1' : '0')
  } catch {
    /* private mode: it just shows again next time */
  }
}

interface Step {
  title: string
  text: string
  done: boolean
  to?: string
}

const steps = computed<Step[]>(() => [...columns.value[0]!.steps, ...columns.value[1]!.steps])
const doneCount = computed(() => steps.value.filter((s) => s.done).length)

const columns = computed<{ title: string; steps: Step[] }[]>(() => {
  const onRealTerminal = Boolean(active.value && active.value.id !== 'sandbox' && active.value.status.configured)
  const run: Step[] = [
    {
      title: 'Try a sale on the sandbox',
      text: 'The built-in sandbox answers like a terminal, so you can see a receipt before any hardware is involved.',
      done: props.activity.some((a) => a.ok),
      to: '/operations?provider=sandbox&fn=purchase',
    },
    {
      title: `Connect your ${realTerminal.value?.name ?? 'terminal'}`,
      text: 'Give it the terminal\'s address and make it active. After every power-on, insert a card once (you can cancel) — it only opens its link then.',
      done: onRealTerminal,
      to: realTerminal.value ? `/providers/${realTerminal.value.id}` : '/providers',
    },
    {
      title: 'See the terminal light turn green',
      text: 'The sidebar and the terminal page show whether it answers right now.',
      done: onRealTerminal && link.value?.state === 'up',
      to: active.value ? `/providers/${active.value.id}` : '/providers',
    },
    {
      title: 'Take a payment on your terminal',
      text: 'Usually from your till: it asks over HTTP and gets the receipt back as JSON. The guide has ready-to-copy examples.',
      done: props.activity.some((a) => a.ok && a.provider !== 'sandbox'),
      to: '/guide#api',
    },
  ]
  const protect: Step[] = [
    ...(install.value.available
      ? [{ title: 'Install it for your account', text: 'The first run does this by itself — menu entry, icon and the background API. No password needed.', done: install.value.installed, to: '/settings' }]
      : []),
    {
      title: 'Lock the console',
      text: 'A password for changing settings; payments keep working while locked.',
      done: auth.status.value.passwordSet,
      to: '/security#lock',
    },
    {
      title: 'Tell your server when this machine changes',
      text: 'Signed webhooks for the lock, the settings and the terminal.',
      done: Boolean(events.value?.subscriptions.some((s) => s.enabled)),
      to: '/security#events',
    },
    {
      title: 'Sign every sale',
      text: 'Receivers can verify that a receipt came from this machine.',
      done: Boolean(events.value?.receiptKeyId),
      to: '/security#receipts',
    },
  ]
  return [
    { title: 'Take payments', steps: run },
    { title: 'Protect it (optional)', steps: protect },
  ]
})

async function load() {
  try {
    ;[settings.value, events.value] = await Promise.all([api.settings(), api.events()])
  } catch {
    /* the sidebar reports an unreachable API */
  }
}

onMounted(() => {
  try {
    hidden.value = localStorage.getItem(STORAGE) === '1'
  } catch {
    /* default: shown */
  }
  load()
})
</script>

<style scoped>
.step {
  display: flex;
  align-items: flex-start;
  gap: 0.75rem;
  border-radius: 0.75rem;
  padding: 0.55rem 0.6rem;
  transition: background 0.2s;
}
a.step:hover {
  background: rgb(255 255 255 / 0.035);
}
.step-mark {
  display: flex;
  flex-shrink: 0;
  align-items: center;
  justify-content: center;
  margin-top: 0.15rem;
  width: 1.1rem;
  height: 1.1rem;
  border-radius: 9999px;
  border: 1.5px solid var(--color-line-bright);
}
.step-done .step-mark {
  border-color: transparent;
  background: var(--color-ok);
  color: var(--color-ink-950);
}
.step-done .text-paper {
  color: var(--color-paper-dim);
}
</style>
