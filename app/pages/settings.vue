<template>
  <div ref="root">
    <PageHeader
      kicker="Settings · 04"
      title="Tuned to your desk."
      lede="Everything here is stored in a plain JSON file you also own — edit it from this screen, over the REST API, or in a text editor."
    >
      <template #actions>
        <button
          class="btn-primary"
          type="button"
          :disabled="saving || !model"
          @click="locked ? requestUnlock() : save()"
        >
          {{ locked ? 'Unlock to save' : saving ? 'Saving…' : 'Save changes' }}
        </button>
      </template>
    </PageHeader>

    <LockBanner what="Settings" />

    <div v-if="model" class="space-y-6" :class="locked && 'pointer-events-none opacity-60'" :aria-disabled="locked">
      <!-- server -->
      <GlassCard data-reveal>
        <h2 class="display mb-1 text-xl text-paper">REST API server</h2>
        <p class="mb-5 text-xs text-paper-mute">
          Changing these rebinds the server — API consumers may see a brief drop.
        </p>
        <div class="grid gap-5 sm:grid-cols-2">
          <div class="space-y-1">
            <label for="srv-host" class="font-mono text-[0.8125rem] text-paper">host</label>
            <input id="srv-host" v-model.trim="model.server.host" class="field font-mono" spellcheck="false" />
            <p class="text-xs text-paper-mute">Keep 127.0.0.1 unless other machines must reach the API.</p>
          </div>
          <div class="space-y-1">
            <label for="srv-port" class="font-mono text-[0.8125rem] text-paper">port</label>
            <input id="srv-port" v-model.number="model.server.port" type="number" min="1" max="65535" class="field font-mono tabular" />
            <p class="text-xs text-paper-mute">Default 4373.</p>
          </div>
          <div class="space-y-1 sm:col-span-2">
            <label for="srv-cors" class="font-mono text-[0.8125rem] text-paper">corsAllowedOrigins</label>
            <textarea id="srv-cors" v-model="corsText" rows="3" class="field font-mono text-[0.8125rem]" spellcheck="false" />
            <p class="text-xs text-paper-mute">One browser origin per line. Only affects browsers — curl/scripts are never blocked by CORS.</p>
          </div>
          <div class="flex items-center justify-between sm:col-span-2">
            <div>
              <div class="text-sm text-paper">Allow every origin</div>
              <p class="text-xs text-paper-mute">Any web page you visit could then call this API from your browser. Leave off unless you understand that.</p>
            </div>
            <TSwitch v-model="model.server.corsAllowAll" label="Allow every origin" />
          </div>
        </div>
      </GlassCard>

      <!-- app behavior -->
      <GlassCard data-reveal>
        <h2 class="display mb-5 text-xl text-paper">Desktop behavior</h2>
        <div class="space-y-5">
          <div class="flex items-center justify-between gap-6">
            <div>
              <div class="text-sm text-paper">Minimize to tray on close</div>
              <p class="text-xs text-paper-mute">Closing the window keeps the API serving from the system tray.</p>
            </div>
            <TSwitch v-model="model.app.minimizeToTrayOnClose" label="Minimize to tray on close" />
          </div>
          <div class="flex items-center justify-between gap-6">
            <div>
              <div class="text-sm text-paper">Start at boot</div>
              <p class="text-xs text-paper-mute">Registers with the OS autostart; launches quietly with <code class="font-mono">--background</code>.</p>
            </div>
            <TSwitch v-model="model.app.startAtBoot" label="Start at boot" />
          </div>
          <div class="flex items-center justify-between gap-6">
            <div>
              <div class="text-sm text-paper">Start minimized</div>
              <p class="text-xs text-paper-mute">Every launch starts hidden in the tray, window on demand.</p>
            </div>
            <TSwitch v-model="model.app.startMinimized" label="Start minimized" />
          </div>
        </div>
      </GlassCard>

      <!-- appearance -->
      <GlassCard data-reveal>
        <h2 class="display mb-5 text-xl text-paper">Appearance</h2>
        <div class="flex items-center justify-between gap-6">
          <div>
            <div class="text-sm text-paper">Reduced visual effects</div>
            <p class="text-xs text-paper-mute">
              Solid panels instead of blur, glow and animation — for weak GPUs and Raspberry Pi class boards.
              Detected automatically on first run; applies immediately and is remembered per device.
            </p>
          </div>
          <TSwitch v-model="reducedFx" label="Reduced visual effects" />
        </div>
      </GlassCard>

      <!-- provider -->
      <GlassCard data-reveal>
        <h2 class="display mb-1 text-xl text-paper">Active provider</h2>
        <p class="mb-4 text-xs text-paper-mute">
          The default engine behind <code class="font-mono">/api/v1/providers/active</code>. Per-provider connection settings live on each
          <NuxtLink to="/providers" class="text-brass-400 hover:text-brass-300">provider page</NuxtLink>.
        </p>
        <select v-model="model.providers.active" class="field max-w-sm" aria-label="Active provider">
          <option v-for="p in providerList" :key="p.id" :value="p.id">{{ p.name }} ({{ p.id }})</option>
        </select>
      </GlassCard>

      <!-- desktop integration -->
      <GlassCard v-if="installInfo.available" data-reveal>
        <h2 class="display mb-1 text-xl text-paper">Desktop integration</h2>
        <p class="mb-5 text-xs leading-relaxed text-paper-mute">
          You are running the portable file. Installing adds it to your applications menu and makes
          the <code class="font-mono">posd</code> command available — all in your home directory,
          no password needed.
        </p>

        <div class="flex flex-wrap items-center justify-between gap-4">
          <div class="min-w-0">
            <div class="data-label mb-1">Status</div>
            <div class="truncate font-mono text-xs text-paper-dim">
              {{ installInfo.installed ? installInfo.installedPath : 'Not installed' }}
            </div>
          </div>
          <button
            type="button"
            :class="installInfo.installed ? 'btn-ghost' : 'btn-primary'"
            :disabled="installBusy"
            @click="installInfo.installed ? removeApp() : addApp()"
          >
            {{ installBusy ? 'Working…' : installInfo.installed ? 'Remove' : 'Install' }}
          </button>
        </div>

        <p class="mt-4 text-[0.6875rem] leading-relaxed text-paper-mute">
          Removing takes back only what was installed. Your settings stay where they are.
        </p>
      </GlassCard>

      <!-- security -->
      <GlassCard data-reveal>
        <h2 class="display mb-1 text-xl text-paper">Application lock</h2>
        <p class="mb-5 text-xs leading-relaxed text-paper-mute">
          Set by the machine administrator, not from here — the password lives in a root-owned file
          so it cannot be removed with a text editor.
        </p>

        <dl class="mb-5 grid grid-cols-2 gap-4">
          <div>
            <dt class="data-label mb-1">Password</dt>
            <dd class="text-sm text-paper-dim">{{ auth.status.value.passwordSet ? 'Set' : 'Not set' }}</dd>
          </div>
          <div>
            <dt class="data-label mb-1">Auto-lock</dt>
            <dd class="text-sm text-paper-dim">
              {{ auth.status.value.autoLockMinutes > 0 ? `${auth.status.value.autoLockMinutes} min idle` : 'Disabled' }}
            </dd>
          </div>
        </dl>

        <div class="glass-inset space-y-1 px-4 py-3 font-mono text-[0.6875rem] text-paper-mute">
          <div>sudo posd admin set-password</div>
          <div>sudo posd admin clear-password</div>
          <div>sudo posd admin auto-lock 15</div>
          <div>sudo posd admin status</div>
          <div class="pt-2 text-paper-dim">AppImage install? prefix with the full path:</div>
          <div>sudo ~/.local/bin/posd admin status</div>
        </div>

        <p class="mt-4 text-[0.6875rem] leading-relaxed text-paper-mute">
          While locked, this screen, provider configuration and the Operations runner are read-only.
        </p>
      </GlassCard>

      <!-- file location -->
      <GlassCard data-reveal :pad="false" class="px-6 py-4">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div>
            <div class="data-label mb-1">Settings file</div>
            <button
              type="button"
              class="cursor-pointer text-left font-mono text-xs text-paper-dim transition-colors hover:text-brass-300"
              :title="'Click to copy'"
              @click="settingsFile && copyText(settingsFile)"
            >
              {{ settingsFile || '—' }}
            </button>
          </div>
          <button class="btn-ghost text-xs" type="button" @click="openApiDocs()">API docs ↗</button>
        </div>
      </GlassCard>
    </div>

    <div v-else-if="loadError" class="glass p-8 text-center text-danger">{{ loadError }}</div>
  </div>
</template>

<script setup lang="ts">
import type { ProviderSummary, Settings } from '~/composables/api'

const api = useApi()
const auth = useAuth()
const locked = useLocked()
const { info: installInfo, install: installApp, uninstall: uninstallApp, refresh: refreshInstall } = useInstall()
const installBusy = ref(false)

async function addApp() {
  installBusy.value = true
  try {
    await installApp()
    toast('ok', 'Added to your applications menu')
  } catch (e) {
    toast('error', e instanceof Error ? e.message : 'Install failed')
  } finally {
    installBusy.value = false
  }
}

async function removeApp() {
  installBusy.value = true
  try {
    await uninstallApp()
    toast('ok', 'Removed from your applications menu')
  } catch (e) {
    toast('error', e instanceof Error ? e.message : 'Removal failed')
  } finally {
    installBusy.value = false
  }
}
const root = ref<HTMLElement>()
useReveal(root)

const model = ref<Settings | null>(null)
const providerList = ref<ProviderSummary[]>([])
const settingsFile = ref('')
const saving = ref(false)
const loadError = ref('')

const reducedFx = computed({
  get: () => useReducedEffects().value,
  set: (v: boolean) => setReducedEffects(v),
})

const corsText = computed({
  get: () => model.value?.server.corsAllowedOrigins.join('\n') ?? '',
  set: (v: string) => {
    if (model.value) {
      model.value.server.corsAllowedOrigins = v
        .split('\n')
        .map((s) => s.trim())
        .filter(Boolean)
    }
  },
})

async function load() {
  try {
    const [s, p, sys] = await Promise.all([api.settings(), api.providers(), api.system()])
    model.value = s
    providerList.value = p
    settingsFile.value = sys.settingsFile
  } catch (e) {
    loadError.value = e instanceof Error ? e.message : 'Failed to load settings'
  }
}

async function save() {
  if (!model.value) return
  saving.value = true
  try {
    const res = await api.saveSettings(model.value)
    model.value = res.settings
    if (res.serverRestarting) {
      toast('info', 'Settings saved — server is rebinding to the new address')
      // Give the supervisor a moment, then follow the server to its new port.
      setTimeout(async () => {
        await resolveApiBase()
        load()
      }, 1200)
    } else {
      toast('ok', 'Settings saved')
    }
  } catch (e) {
    toast('error', e instanceof Error ? e.message : 'Failed to save settings')
  } finally {
    saving.value = false
  }
}

onMounted(() => {
  load()
  refreshInstall()
})
</script>
