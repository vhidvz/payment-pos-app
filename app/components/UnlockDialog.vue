<template>
  <Teleport to="body">
    <Transition name="unlock">
      <div
        v-if="open"
        class="fixed inset-0 z-[60] flex items-center justify-center bg-ink-950/70 p-6 backdrop-blur-sm"
        role="dialog"
        aria-modal="true"
        aria-labelledby="unlock-title"
        @keydown.esc="close"
      >
        <GlassCard class="w-full max-w-sm">
          <h2 id="unlock-title" class="display mb-1 text-xl text-paper">Unlock to make changes</h2>
          <p class="mb-5 text-xs leading-relaxed text-paper-mute">
            Settings, provider configuration and manual terminal operations are locked.
          </p>

          <form class="space-y-3" @submit.prevent="submit">
            <div class="space-y-1">
              <label for="unlock-password" class="font-mono text-[0.8125rem] text-paper">password</label>
              <input
                id="unlock-password"
                ref="input"
                v-model="password"
                type="password"
                class="field"
                autocomplete="current-password"
                :disabled="busy"
              />
            </div>

            <p v-if="error" class="text-xs text-danger" role="alert">{{ error }}</p>

            <div class="flex gap-2 pt-1">
              <button type="submit" class="btn-primary flex-1" :disabled="busy || !password">
                {{ busy ? 'Checking…' : 'Unlock' }}
              </button>
              <button type="button" class="btn-ghost" :disabled="busy" @click="close">Cancel</button>
            </div>
          </form>

          <p class="mt-5 border-t hairline pt-4 text-[0.6875rem] leading-relaxed text-paper-mute">
            Forgotten it? Only the machine administrator can reset it:
            <code class="font-mono text-paper-dim">sudo posd admin clear-password</code>
          </p>
        </GlassCard>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
const open = useUnlockPrompt()
const { unlock } = useAuth()

const password = ref('')
const error = ref('')
const busy = ref(false)
const input = ref<HTMLInputElement>()

watch(open, async (isOpen) => {
  if (!isOpen) return
  password.value = ''
  error.value = ''
  await nextTick()
  input.value?.focus()
})

function close() {
  open.value = false
}

async function submit() {
  busy.value = true
  error.value = ''
  try {
    await unlock(password.value)
    password.value = ''
    open.value = false
    toast('ok', 'Unlocked')
  } catch (e) {
    // Repeated failures are deliberately slowed down by the server.
    error.value = e instanceof Error ? e.message : 'Could not unlock'
  } finally {
    busy.value = false
  }
}
</script>

<style scoped>
.unlock-enter-active,
.unlock-leave-active {
  transition: opacity 0.2s ease;
}
.unlock-enter-from,
.unlock-leave-to {
  opacity: 0;
}
</style>
