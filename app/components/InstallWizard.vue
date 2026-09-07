<template>
  <Teleport to="body">
    <Transition name="wizard">
      <div
        v-if="show"
        class="fixed inset-0 z-[55] flex items-center justify-center bg-ink-950/70 p-6 backdrop-blur-sm"
        role="dialog"
        aria-modal="true"
        aria-labelledby="wizard-title"
      >
        <GlassCard class="w-full max-w-md">
          <h2 id="wizard-title" class="display mb-1 text-xl text-paper">Add Ledger POS to this computer?</h2>
          <p class="mb-5 text-xs leading-relaxed text-paper-mute">
            You are running the portable file. Installing puts it in your applications menu with its
            icon, and makes the <code class="font-mono">posd</code> administrator command available —
            all under your own home directory, no password needed.
          </p>

          <ul class="mb-5 space-y-2 text-xs text-paper-dim">
            <li v-for="line in what" :key="line" class="flex gap-2">
              <span class="mt-1.5 size-1 shrink-0 rounded-full bg-brass-500" aria-hidden="true" />
              <span class="font-mono text-[0.6875rem] break-all">{{ line }}</span>
            </li>
          </ul>

          <div class="mb-5 flex items-center justify-between gap-6 border-t hairline pt-4">
            <div>
              <div class="text-sm text-paper">Start at boot</div>
              <p class="text-xs text-paper-mute">Launch quietly in the tray when you log in.</p>
            </div>
            <TSwitch v-model="startAtBoot" label="Start at boot" />
          </div>

          <p v-if="error" class="mb-3 text-xs text-danger" role="alert">{{ error }}</p>

          <div class="flex gap-2">
            <button type="button" class="btn-primary flex-1" :disabled="busy" @click="doInstall">
              {{ busy ? 'Installing…' : 'Install' }}
            </button>
            <button type="button" class="btn-ghost" :disabled="busy" @click="notNow">Not now</button>
          </div>

          <p class="mt-5 border-t hairline pt-4 text-[0.6875rem] leading-relaxed text-paper-mute">
            To put a password on this app afterwards, run
            <code class="font-mono text-paper-dim">sudo ~/.local/bin/posd admin set-password</code> —
            that one step needs administrator rights and cannot be done from here.
          </p>
        </GlassCard>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import type { Settings } from '~/composables/api'

const show = useShowInstallWizard()
const { install, dismiss } = useInstall()
const api = useApi()

const busy = ref(false)
const error = ref('')
const startAtBoot = ref(false)

const what = [
  '~/.local/bin/ledger-pos.AppImage',
  '~/.local/bin/posd',
  '~/.local/share/applications/ledger-pos.desktop',
  '~/.local/share/icons/hicolor/…/ledger-pos.png',
]

async function doInstall() {
  busy.value = true
  error.value = ''
  try {
    if (startAtBoot.value) {
      const settings = await api.settings()
      settings.app.startAtBoot = true
      await api.saveSettings(settings as Settings)
    }
    await install()
    toast('ok', 'Ledger POS is now in your applications menu')
  } catch (e) {
    error.value = e instanceof Error ? e.message : 'Install failed'
  } finally {
    busy.value = false
  }
}

async function notNow() {
  await dismiss()
}
</script>

<style scoped>
.wizard-enter-active,
.wizard-leave-active {
  transition: opacity 0.2s ease;
}
.wizard-enter-from,
.wizard-leave-to {
  opacity: 0;
}
</style>
