<template>
  <div class="flex h-screen overflow-hidden">
    <GlowBackdrop />

    <!-- ------------------------------------------------------- sidebar -->
    <aside
      class="flex w-[232px] shrink-0 flex-col border-r hairline bg-ink-900/40 backdrop-blur-xl"
      aria-label="Primary"
    >
      <!-- brand -->
      <div class="flex items-center gap-3 px-5 pt-6 pb-8">
        <div
          class="flex size-9 items-center justify-center rounded-xl text-ink-950 shadow-glow-brass"
          style="background: linear-gradient(135deg, #e8c37a, #a87f3b)"
          aria-hidden="true"
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round">
            <rect x="2.5" y="5.5" width="19" height="13" rx="2.6" />
            <path d="M2.5 10h19" />
          </svg>
        </div>
        <div class="leading-tight">
          <div class="display text-[1.05rem] text-paper">Ledger</div>
          <div class="text-[0.625rem] uppercase tracking-[0.3em] text-paper-mute">POS Bridge</div>
        </div>
      </div>

      <!-- nav -->
      <nav class="flex-1 space-y-1 px-3" aria-label="Sections">
        <NuxtLink v-for="item in nav" :key="item.to" :to="item.to" class="nav-item" :class="{ 'nav-active': isActive(item.to) }">
          <span v-html="item.icon" class="[&>svg]:size-[17px]" aria-hidden="true" />
          <span>{{ item.label }}</span>
        </NuxtLink>

        <button class="nav-item w-full" type="button" @click="openApiDocs()">
          <span aria-hidden="true">
            <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20" /><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z" />
            </svg>
          </span>
          <span>API Docs</span>
          <svg class="ml-auto size-3 opacity-50" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
            <path d="M7 17 17 7M9 7h8v8" />
          </svg>
        </button>
      </nav>

      <!-- server status -->
      <div class="px-5 pb-5">
        <div class="glass-inset flex items-center gap-2.5 px-3.5 py-3">
          <span class="dot" :class="serverUp ? 'text-ok bg-ok' : 'text-danger bg-danger'" />
          <div class="min-w-0 leading-tight">
            <div class="text-xs font-medium" :class="serverUp ? 'text-paper-dim' : 'text-danger'">
              {{ serverUp ? 'API serving' : 'API offline' }}
            </div>
            <div class="truncate font-mono text-[0.6875rem] text-paper-mute tabular">{{ baseHost }}</div>
          </div>
        </div>
      </div>
    </aside>

    <!-- --------------------------------------------------------- content -->
    <main class="min-w-0 flex-1 overflow-y-auto">
      <div class="mx-auto max-w-[1060px] px-10 pt-10 pb-20">
        <slot />
      </div>
    </main>

    <!-- ---------------------------------------------------------- toasts -->
    <div class="pointer-events-none fixed right-5 bottom-5 z-50 flex w-[320px] flex-col gap-2" aria-live="polite">
      <TransitionGroup name="toast">
        <div
          v-for="t in toasts"
          :key="t.id"
          class="glass pointer-events-auto flex items-start gap-2.5 px-4 py-3 text-sm"
        >
          <span
            class="mt-1 size-2 shrink-0 rounded-full"
            :class="{ 'bg-ok': t.kind === 'ok', 'bg-danger': t.kind === 'error', 'bg-info': t.kind === 'info' }"
          />
          <span class="text-paper-dim">{{ t.text }}</span>
        </div>
      </TransitionGroup>
    </div>
  </div>
</template>

<script setup lang="ts">
const route = useRoute()
const toasts = useToasts()
const api = useApi()
const base = useApiBase()

const serverUp = ref(false)
const baseHost = computed(() => base.value.replace(/^https?:\/\//, ''))

const nav = [
  {
    to: '/',
    label: 'Overview',
    icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="7" height="9" rx="1.5"/><rect x="14" y="3" width="7" height="5" rx="1.5"/><rect x="14" y="12" width="7" height="9" rx="1.5"/><rect x="3" y="16" width="7" height="5" rx="1.5"/></svg>`,
  },
  {
    to: '/providers',
    label: 'Providers',
    icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"/><path d="M3.3 7 12 12l8.7-5M12 22V12"/></svg>`,
  },
  {
    to: '/operations',
    label: 'Operations',
    icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/></svg>`,
  },
  {
    to: '/settings',
    label: 'Settings',
    icon: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>`,
  },
]

function isActive(to: string) {
  if (to === '/') return route.path === '/'
  return route.path.startsWith(to)
}

let timer: ReturnType<typeof setInterval> | undefined

async function poll() {
  try {
    await api.health()
    serverUp.value = true
  } catch {
    serverUp.value = false
    // The shell may have rebound to a new port — re-resolve.
    await resolveApiBase()
  }
}

onMounted(() => {
  poll()
  timer = setInterval(poll, 5000)
})
onBeforeUnmount(() => clearInterval(timer))
</script>

<style scoped>
.nav-item {
  display: flex;
  align-items: center;
  gap: 0.7rem;
  width: 100%;
  border-radius: 0.75rem;
  padding: 0.55rem 0.8rem;
  font-size: 0.85rem;
  color: var(--color-paper-dim);
  transition: color 0.25s, background 0.25s;
  cursor: pointer;
}
.nav-item:hover {
  color: var(--color-paper);
  background: rgb(255 255 255 / 0.04);
}
.nav-active {
  color: var(--color-brass-300);
  background: linear-gradient(90deg, rgb(201 160 85 / 0.12), rgb(201 160 85 / 0.03));
  box-shadow: inset 2px 0 0 var(--color-brass-500);
}

.toast-enter-active,
.toast-leave-active {
  transition: all 0.35s var(--ease-swift);
}
.toast-enter-from {
  opacity: 0;
  transform: translateY(8px);
}
.toast-leave-to {
  opacity: 0;
  transform: translateX(12px);
}
</style>
