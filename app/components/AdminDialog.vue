<template>
  <Teleport to="body">
    <Transition name="admin">
      <div
        v-if="action"
        class="fixed inset-0 z-[60] flex items-center justify-center bg-ink-950/70 p-6 backdrop-blur-sm"
        role="dialog"
        aria-modal="true"
        aria-labelledby="admin-title"
        @keydown.esc="phase !== 'waiting' && !mustAcknowledge && close()"
      >
        <GlassCard class="w-full max-w-lg">
          <div class="mb-1 flex items-center gap-2">
            <span class="admin-badge" aria-hidden="true">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" /></svg>
            </span>
            <span class="data-label">Administrator</span>
          </div>
          <h2 id="admin-title" class="display mb-2 text-xl text-paper">{{ action.title }}</h2>

          <!-- confirm ------------------------------------------------------------>
          <template v-if="phase === 'confirm'">
            <p class="mb-5 text-sm leading-relaxed text-paper-dim">{{ action.explain }}</p>

            <form v-if="asksPassword" class="mb-5 space-y-3" @submit.prevent="go">
              <div class="space-y-1">
                <label for="adm-pw" class="font-mono text-[0.8125rem] text-paper">new password</label>
                <input id="adm-pw" ref="first" v-model="pw" type="password" class="field" autocomplete="new-password" />
              </div>
              <div class="space-y-1">
                <label for="adm-pw2" class="font-mono text-[0.8125rem] text-paper">repeat it</label>
                <input id="adm-pw2" v-model="pw2" type="password" class="field" autocomplete="new-password" />
              </div>
              <p v-if="pwProblem" class="text-xs text-warn">{{ pwProblem }}</p>
            </form>

            <div v-if="preview?.inApp" class="mb-5 flex gap-3 rounded-xl border border-info/25 bg-info/6 px-4 py-3 text-xs leading-relaxed text-paper-dim">
              <svg class="mt-0.5 size-4 shrink-0 text-info" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="11" width="18" height="11" rx="2" /><path d="M7 11V7a5 5 0 0 1 10 0v4" /></svg>
              <div>
                Your computer will ask for the <strong class="text-paper">administrator's password</strong> (the one
                for <code class="font-mono">sudo</code>) — not the Ledger POS password.
                <button type="button" class="ml-1 cursor-pointer text-brass-400 hover:text-brass-300" @click="showCommand = !showCommand">
                  {{ showCommand ? 'Hide the terminal command' : 'Prefer a terminal?' }}
                </button>
              </div>
            </div>
            <div v-else-if="preview" class="mb-5 rounded-xl border border-warn/25 bg-warn/6 px-4 py-3 text-xs leading-relaxed text-warn">
              {{ preview.reason }} Run this in a terminal on this machine instead:
            </div>

            <CommandBox v-if="preview && (!preview.inApp || showCommand)" :command="preview.command" class="mb-5" />

            <div class="flex gap-2">
              <button
                v-if="!preview || preview.inApp"
                type="button"
                class="flex-1"
                :class="action.danger ? 'btn-danger' : 'btn-primary'"
                :disabled="!preview || (asksPassword && !!pwProblem)"
                @click="go"
              >
                {{ action.confirmLabel }}
              </button>
              <button v-else type="button" class="btn-primary flex-1" @click="copyText(preview.command)">Copy command</button>
              <button type="button" class="btn-ghost" @click="close">{{ preview && !preview.inApp ? 'Done' : 'Cancel' }}</button>
            </div>
            <p v-if="preview && !preview.inApp" class="mt-4 text-[0.6875rem] leading-relaxed text-paper-mute">
              This screen updates by itself a few seconds after the command has run.
            </p>
          </template>

          <!-- waiting ------------------------------------------------------------>
          <div v-else-if="phase === 'waiting'" class="flex items-center gap-3 py-4 text-sm text-paper-dim" role="status">
            <span class="spinner" aria-hidden="true" />
            Waiting for the administrator's password in the system dialog…
          </div>

          <!-- done --------------------------------------------------------------->
          <template v-else-if="phase === 'done'">
            <p class="mb-4 flex items-start gap-2 text-sm leading-relaxed text-ok">
              <svg class="mt-0.5 size-4 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5" /></svg>
              <span>{{ doneText }}</span>
            </p>
            <div v-if="reveals.length" class="mb-4 space-y-3">
              <p class="rounded-lg border border-warn/25 bg-warn/6 px-3 py-2 text-xs leading-relaxed text-warn">
                Shown <strong>once</strong>. Copy it now and give it to whoever needs it — Ledger POS cannot show it again.
              </p>
              <div v-for="r in reveals" :key="r.label">
                <div class="data-label mb-1">{{ r.label }}</div>
                <div class="flex items-center gap-2">
                  <code class="glass-inset min-w-0 flex-1 break-all px-3 py-2 font-mono text-xs text-paper">{{ r.value }}</code>
                  <button type="button" class="btn-ghost shrink-0 text-xs" @click="copyText(r.value)">Copy</button>
                </div>
                <p v-if="r.hint" class="mt-1 text-[0.6875rem] leading-relaxed text-paper-mute">{{ r.hint }}</p>
              </div>
              <label class="flex cursor-pointer items-center gap-2 text-xs text-paper-dim">
                <input v-model="saved" type="checkbox" class="accent-brass-500" /> I have saved it
              </label>
            </div>
            <button type="button" class="btn-primary w-full" :disabled="mustAcknowledge" @click="close">Close</button>
          </template>

          <!-- cancelled ---------------------------------------------------------->
          <template v-else-if="phase === 'cancelled'">
            <p class="mb-5 text-sm text-paper-dim">Cancelled — nothing was changed.</p>
            <div class="flex gap-2">
              <button type="button" class="btn-primary flex-1" @click="retry">Try again</button>
              <button type="button" class="btn-ghost" @click="close">Close</button>
            </div>
          </template>

          <!-- manual / failed ---------------------------------------------------->
          <template v-else>
            <p class="mb-3 text-sm leading-relaxed" :class="phase === 'failed' ? 'text-danger' : 'text-warn'" role="alert">{{ problem }}</p>
            <p class="mb-2 text-xs text-paper-mute">You can run it in a terminal on this machine instead:</p>
            <CommandBox v-if="command" :command="command" class="mb-5" />
            <button type="button" class="btn-ghost w-full" @click="close">Close</button>
          </template>
        </GlassCard>
      </div>
    </Transition>
  </Teleport>
</template>

<script setup lang="ts">
import type { AdminOp, AdminOutput, AdminPreview, Reveal } from '~/composables/admin'

const MIN_PASSWORD = 4 // posd's own minimum (security::MIN_PASSWORD_LEN); it re-checks

const admin = useAdmin()
const action = admin.current

type Phase = 'confirm' | 'waiting' | 'done' | 'cancelled' | 'manual' | 'failed'
const phase = ref<Phase>('confirm')
const preview = ref<AdminPreview | null>(null)
const showCommand = ref(false)
const output = ref<AdminOutput>({})
const problem = ref('')
const command = ref('')
const saved = ref(false)
const pw = ref('')
const pw2 = ref('')
const first = ref<HTMLInputElement>()

const asksPassword = computed(() => action.value?.op.op === 'setPassword')
const pwProblem = computed(() => {
  if (!asksPassword.value) return ''
  if (pw.value.length < MIN_PASSWORD) return `At least ${MIN_PASSWORD} characters.`
  if (pw.value !== pw2.value) return 'The two entries do not match yet.'
  return ''
})
const reveals = computed<Reveal[]>(() => (action.value?.reveal ? action.value.reveal(output.value) : []))
const doneText = computed(() => action.value?.doneText?.(output.value) ?? 'Done.')
const mustAcknowledge = computed(() => phase.value === 'done' && reveals.value.length > 0 && !saved.value)

watch(action, async (a) => {
  phase.value = 'confirm'
  preview.value = null
  showCommand.value = false
  output.value = {}
  saved.value = false
  pw.value = ''
  pw2.value = ''
  if (!a) return
  try {
    preview.value = await adminPreview(a.op)
  } catch (e) {
    phase.value = 'failed'
    problem.value = e instanceof Error ? e.message : String(e)
    command.value = ''
    return
  }
  await nextTick()
  first.value?.focus()
})

async function go() {
  const a = action.value
  if (!a || (asksPassword.value && pwProblem.value)) return
  const op = (a.op.op === 'setPassword' ? { op: 'setPassword', password: pw.value } : a.op) as AdminOp
  phase.value = 'waiting'
  admin.busy.value = true
  const res = await adminRun(op).finally(() => (admin.busy.value = false))
  // The password has done its job; it does not wait around in memory.
  pw.value = ''
  pw2.value = ''
  switch (res.kind) {
    case 'done':
      output.value = res.output
      phase.value = 'done'
      admin.changed.value++
      a.onDone?.(res.output)
      break
    case 'cancelled':
      phase.value = 'cancelled'
      break
    case 'manual':
      phase.value = 'manual'
      problem.value = res.reason
      command.value = res.command
      break
    case 'failed':
      phase.value = 'failed'
      problem.value = res.message
      command.value = res.command
      break
  }
}

/** After a dismissed dialog: run it again — or, when it asked for a password, ask again first. */
function retry() {
  if (asksPassword.value) phase.value = 'confirm'
  else go()
}

function close() {
  output.value = {}
  admin.close()
}
</script>

<style scoped>
.admin-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.4rem;
  height: 1.4rem;
  border-radius: 9999px;
  color: var(--color-brass-300);
  background: rgb(201 160 85 / 0.12);
}
.spinner {
  width: 1rem;
  height: 1rem;
  border-radius: 9999px;
  border: 2px solid rgb(255 255 255 / 0.15);
  border-top-color: var(--color-brass-400);
  animation: spin 0.9s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}
.admin-enter-active,
.admin-leave-active {
  transition: opacity 0.2s ease;
}
.admin-enter-from,
.admin-leave-to {
  opacity: 0;
}
</style>
