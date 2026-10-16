<template>
  <div ref="root">
    <PageHeader
      kicker="Security · 04"
      title="Who can change what."
      lede="Lock the console, tell another server when this machine changes, and sign every sale so its receiver can trust it. All optional, all off until the computer's administrator turns them on."
    >
      <template #actions>
        <NuxtLink to="/guide#security" class="btn-ghost">How it works</NuxtLink>
      </template>
    </PageHeader>

    <!-- at a glance -->
    <div class="mb-8 grid gap-4 md:grid-cols-3" data-reveal>
      <a v-for="t in tiles" :key="t.href" :href="t.href" class="glass hover-glow block p-5">
        <div class="mb-2 flex items-center justify-between">
          <span class="data-label">{{ t.label }}</span>
          <StatusPill :tone="t.tone" :label="t.state" />
        </div>
        <p class="text-xs leading-relaxed text-paper-mute">{{ t.detail }}</p>
      </a>
    </div>

    <div class="mb-8 flex gap-3 rounded-xl border border-line-bright bg-white/2 px-4 py-3 text-xs leading-relaxed text-paper-dim" data-reveal>
      <svg class="mt-0.5 size-4 shrink-0 text-brass-400" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" /></svg>
      <p>
        Buttons with a shield change a file only the computer's
        administrator can write. Your system asks for that password — the one used for <code class="font-mono">sudo</code> —
        so someone who only knows the Ledger POS password cannot switch any of this off.
      </p>
    </div>

    <div class="space-y-6">
      <!-- ============================================================ lock -->
      <GlassCard id="lock" data-reveal class="scroll-mt-8">
        <SectionHead title="Application lock" :tone="auth.status.value.passwordSet ? 'ok' : 'muted'" :state="auth.status.value.passwordSet ? 'on' : 'off'">
          With a password set, anyone can look, but changing settings or terminals and running operations by hand
          needs the password. Payments sent by your till over the API keep working while it is locked.
        </SectionHead>

        <dl class="mb-5 grid grid-cols-2 gap-4 sm:grid-cols-3">
          <div>
            <dt class="data-label mb-1">Password</dt>
            <dd class="text-sm text-paper-dim">{{ auth.status.value.passwordSet ? 'Set' : 'Not set' }}</dd>
          </div>
          <div>
            <dt class="data-label mb-1">Auto-lock</dt>
            <dd class="text-sm text-paper-dim">
              {{ auth.status.value.autoLockMinutes > 0 ? `after ${auth.status.value.autoLockMinutes} idle minutes` : 'never' }}
            </dd>
          </div>
          <div v-if="auth.status.value.passwordSet">
            <dt class="data-label mb-1">This window</dt>
            <dd class="flex items-center gap-2 text-sm text-paper-dim">
              {{ locked ? 'Locked' : 'Unlocked' }}
              <button type="button" class="text-xs text-brass-400 hover:text-brass-300" @click="locked ? requestUnlock() : auth.lock()">
                {{ locked ? 'Unlock' : 'Lock now' }}
              </button>
            </dd>
          </div>
        </dl>

        <div class="mb-5 flex flex-wrap gap-2">
          <AdminButton v-if="!auth.status.value.passwordSet" primary @click="setPassword(false)">Set a password</AdminButton>
          <template v-else>
            <AdminButton @click="setPassword(true)">Change password</AdminButton>
            <AdminButton danger @click="clearPassword">Remove password</AdminButton>
          </template>
        </div>

        <form class="mb-5 flex flex-wrap items-end gap-3" @submit.prevent="applyAutoLock">
          <div class="space-y-1">
            <label for="auto-lock" class="font-mono text-[0.8125rem] text-paper">auto-lock minutes</label>
            <input id="auto-lock" v-model.number="autoLock" type="number" min="0" max="1440" class="field !w-36 font-mono tabular" />
          </div>
          <AdminButton type="submit" :disabled="!autoLockValid || autoLock === auth.status.value.autoLockMinutes">Apply</AdminButton>
          <p class="w-full text-xs" :class="autoLockValid ? 'text-paper-mute' : 'text-warn'">Idle minutes before an unlocked console locks itself again, 0–1440. 0 never re-locks.</p>
        </form>

        <details class="group">
          <summary class="disclosure">What the lock blocks — and what it does not</summary>
          <div class="mt-3 space-y-3 text-xs leading-relaxed text-paper-dim">
            <table class="w-full text-left">
              <tbody>
                <tr class="border-b hairline"><td class="py-1.5 pr-4">Changing settings, a terminal's configuration or the active terminal — here or over the API</td><td class="py-1.5 text-warn">blocked</td></tr>
                <tr class="border-b hairline"><td class="py-1.5 pr-4">Running operations by hand on the Operations screen</td><td class="py-1.5 text-warn">blocked</td></tr>
                <tr><td class="py-1.5 pr-4">Payments over the API (<code class="font-mono">POST …/invoke</code>) and every read</td><td class="py-1.5 text-ok">still works</td></tr>
              </tbody>
            </table>
            <p>
              So the till keeps trading while nobody at the counter can repoint the terminal. It re-locks when idle and on
              restart, and wrong passwords are answered more slowly after the third try.
            </p>
            <p>
              <strong class="text-paper">An operational control, not a security boundary.</strong> Payments stay open on
              purpose, so anything on this computer can still drive the terminal, and the settings file belongs to the
              account the app runs as. Running the background service under its own account closes those doors —
              <NuxtLink to="/guide#security" class="text-brass-400 hover:text-brass-300">see the guide</NuxtLink>.
            </p>
          </div>
        </details>
      </GlassCard>

      <!-- ========================================================== events -->
      <GlassCard id="events" data-reveal class="scroll-mt-8">
        <SectionHead title="Webhook events" :tone="activeSubs ? 'ok' : 'muted'" :state="activeSubs ? `${activeSubs} active` : 'off'">
          Tell a server somewhere else the moment this machine changes in a way that matters to it — the lock opened,
          the password removed, the terminal repointed. Each message is signed
          (<a href="https://www.standardwebhooks.com" target="_blank" rel="noopener" class="text-brass-400 hover:text-brass-300">Standard Webhooks</a>)
          with a secret only the receiver gets.
        </SectionHead>

        <ul v-if="ev?.subscriptions.length" class="mb-5 divide-y divide-white/5 rounded-xl border hairline">
          <li v-for="s in ev.subscriptions" :key="s.id" class="flex flex-wrap items-center gap-3 px-4 py-3">
            <span class="size-2 shrink-0 rounded-full" :class="s.enabled ? 'bg-ok' : 'bg-paper-mute/50'" :title="s.enabled ? 'active' : 'paused'" />
            <div class="min-w-0 flex-1">
              <div class="truncate font-mono text-[0.8125rem] text-paper">{{ s.url }}</div>
              <div class="text-[0.6875rem] text-paper-mute">
                <span class="font-mono">{{ s.id }}</span> · {{ describePatterns(s.events) }}<span v-if="!s.enabled"> · paused</span>
              </div>
            </div>
            <div class="flex shrink-0 gap-1.5">
              <AdminButton small @click="testSub(s.id)">Test</AdminButton>
              <AdminButton small @click="toggleSub(s.id, !s.enabled)">{{ s.enabled ? 'Pause' : 'Resume' }}</AdminButton>
              <AdminButton small danger @click="removeSub(s.id, s.url)">Remove</AdminButton>
            </div>
          </li>
        </ul>
        <p v-else class="mb-5 text-sm text-paper-mute">No webhooks yet.</p>

        <!-- add -->
        <div v-if="adding" class="glass-inset mb-5 space-y-4 p-4">
          <div class="space-y-1">
            <label for="wh-url" class="font-mono text-[0.8125rem] text-paper">receiver URL</label>
            <input id="wh-url" v-model.trim="newUrl" class="field font-mono" placeholder="https://example.com/hooks/pos" spellcheck="false" autocomplete="off" />
            <p v-if="urlProblem" class="text-xs text-warn">{{ urlProblem }}</p>
            <p v-else-if="newUrl.startsWith('http://')" class="text-xs text-warn">Plain http: the message stays signed but travels unencrypted.</p>
          </div>

          <fieldset class="space-y-2">
            <legend class="mb-1 font-mono text-[0.8125rem] text-paper">send</legend>
            <label class="flex cursor-pointer items-center gap-2 text-sm text-paper-dim">
              <input v-model="allEvents" type="radio" :value="true" class="accent-brass-500" /> Every event <span class="text-xs text-paper-mute">(recommended)</span>
            </label>
            <label class="flex cursor-pointer items-center gap-2 text-sm text-paper-dim">
              <input v-model="allEvents" type="radio" :value="false" class="accent-brass-500" /> Only these:
            </label>
            <div v-if="!allEvents" class="grid gap-x-6 gap-y-1.5 pl-6 sm:grid-cols-2">
              <label v-for="t in ev?.eventTypes ?? []" :key="t" class="flex cursor-pointer items-start gap-2 text-xs text-paper-dim" :title="eventInfo(t).when">
                <input v-model="picked" type="checkbox" :value="t" class="mt-0.5 accent-brass-500" />
                <span><span class="font-mono text-paper">{{ t }}</span><br /><span class="text-paper-mute">{{ eventInfo(t).when }}</span></span>
              </label>
            </div>
          </fieldset>

          <div class="flex gap-2">
            <AdminButton primary :disabled="!!urlProblem || !newUrl || (!allEvents && !picked.length)" @click="addSub">Add webhook</AdminButton>
            <button type="button" class="btn-ghost" @click="adding = false">Cancel</button>
          </div>
        </div>
        <button v-else type="button" class="btn-ghost mb-5" @click="startAdding">+ Add a webhook</button>

        <!-- deliveries -->
        <div class="mb-2 flex items-baseline justify-between">
          <h3 class="data-label">Recent deliveries</h3>
          <span class="text-xs text-paper-mute">{{ ev?.pending ?? 0 }} waiting to be delivered</span>
        </div>
        <div v-if="ev?.recent.length" class="glass-inset mb-5 max-h-72 overflow-auto">
          <table class="w-full text-left text-[0.75rem]">
            <thead>
              <tr class="border-b hairline text-[0.625rem] tracking-[0.14em] text-paper-mute uppercase">
                <th class="px-3 py-2 font-medium">When</th>
                <th class="px-3 py-2 font-medium">Event</th>
                <th class="px-3 py-2 font-medium">Webhook</th>
                <th class="px-3 py-2 font-medium">Result</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="d in ev.recent" :key="d.msgId + d.at" class="border-b hairline align-top last:border-0">
                <td class="px-3 py-1.5 whitespace-nowrap text-paper-mute tabular">{{ timeOf(d.at) }}</td>
                <td class="px-3 py-1.5 font-mono text-paper-dim">{{ d.eventType }}</td>
                <td class="px-3 py-1.5 font-mono text-paper-mute">{{ d.subscription }}</td>
                <td class="px-3 py-1.5" :title="OUTCOME_TEXT[d.outcome]">
                  <StatusPill :tone="OUTCOME_TONE[d.outcome] ?? 'muted'" :label="d.outcome" />
                  <span v-if="d.attempts > 1" class="ml-1 text-paper-mute">×{{ d.attempts }}</span>
                  <div v-if="d.detail" class="mt-0.5 text-paper-mute">{{ d.detail }}</div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-else class="mb-5 text-xs text-paper-mute">Nothing sent yet.</p>

        <details>
          <summary class="disclosure">Which events exist, and how delivery works</summary>
          <div class="mt-3 space-y-3 text-xs leading-relaxed text-paper-dim">
            <dl class="grid gap-x-6 gap-y-2 sm:grid-cols-[auto_1fr]">
              <template v-for="t in ev?.eventTypes ?? []" :key="t">
                <dt class="font-mono text-paper">{{ t }}</dt>
                <dd>{{ eventInfo(t).when }}</dd>
              </template>
            </dl>
            <p>
              Every message carries this device's id and its <a href="#device" class="text-brass-400 hover:text-brass-300">posture</a>,
              including a fingerprint of the settings a payment depends on — a receiver that recorded it can tell from any
              later message that the machine is no longer set up the way it accepted. Undelivered messages wait in
              <code class="font-mono">events-outbox.json</code> and are retried (5 s, doubling to 15 min) for 72 hours;
              redirects are not followed.
            </p>
            <p class="text-warn">A stopped app or a cut network sends nothing: the absence of an event is not evidence that nothing changed.</p>
          </div>
        </details>
      </GlassCard>

      <!-- ======================================================== receipts -->
      <GlassCard id="receipts" data-reveal class="scroll-mt-8">
        <SectionHead title="Signed receipts" :tone="ev?.receiptKeyId ? 'ok' : 'muted'" :state="ev?.receiptKeyId ? 'on' : 'off'">
          Let whoever receives a sale verify that this app produced it. Every successful operation on the
          <strong class="text-paper">active terminal</strong> then carries an <code class="font-mono">attestation</code> — a
          signed token holding the request, the reply and this machine's posture. Other providers, the built-in sandbox
          included, are never signed.
        </SectionHead>

        <div v-if="ev?.receiptKeyId" class="mb-5 flex flex-wrap items-center gap-x-8 gap-y-3">
          <div>
            <div class="data-label mb-1">Key id</div>
            <div class="font-mono text-sm text-paper">{{ ev.receiptKeyId }}</div>
          </div>
          <div class="flex gap-2">
            <AdminButton @click="setReceiptKey(null, true)">Rotate key</AdminButton>
            <AdminButton danger @click="clearReceiptKey">Turn off</AdminButton>
          </div>
        </div>
        <form v-else class="mb-5 flex flex-wrap items-end gap-3" @submit.prevent="setReceiptKey(kid || null, false)">
          <div class="space-y-1">
            <label for="kid" class="font-mono text-[0.8125rem] text-paper">key id <span class="text-paper-mute">(optional)</span></label>
            <input id="kid" v-model.trim="kid" class="field !w-56 font-mono" placeholder="shop-7" spellcheck="false" autocomplete="off" />
          </div>
          <AdminButton type="submit" primary>Turn on</AdminButton>
          <p class="w-full text-xs text-paper-mute">The key id is put in each token's header so a receiver can tell keys apart after a rotation.</p>
        </form>

        <details>
          <summary class="disclosure">How a receiver checks a receipt</summary>
          <ol class="mt-3 list-decimal space-y-1.5 pl-5 text-xs leading-relaxed text-paper-dim">
            <li>Verify the <code class="font-mono">attestation</code> with any JWT library (HS256, the key you were given; match <code class="font-mono">kid</code>).</li>
            <li>Trust the token's claims, not the JSON around it.</li>
            <li>Want <code class="font-mono">postureChanged: false</code> — the machine's setup did not move during the sale.</li>
            <li>Want <code class="font-mono">provider</code> equal to <code class="font-mono">posture.activeProvider</code>.</li>
            <li>Store <code class="font-mono">jti</code> against your own order, so one receipt pays for one sale.</li>
          </ol>
        </details>
      </GlassCard>

      <!-- ========================================================== device -->
      <GlassCard id="device" data-reveal class="scroll-mt-8">
        <SectionHead title="This device" tone="muted" state="posture">
          What a receiver learns about this machine with every event and signed receipt.
        </SectionHead>

        <dl v-if="ev" class="mb-5 grid grid-cols-2 gap-x-6 gap-y-4 sm:grid-cols-3">
          <div>
            <dt class="data-label mb-1">Device id</dt>
            <dd class="font-mono text-xs text-paper-dim">{{ ev.deviceId ?? 'created with the first webhook or key' }}</dd>
          </div>
          <div>
            <dt class="data-label mb-1">Active terminal</dt>
            <dd class="font-mono text-xs text-paper-dim">{{ ev.posture.activeProvider }}</dd>
          </div>
          <div>
            <dt class="data-label mb-1">Console</dt>
            <dd class="text-xs text-paper-dim">
              {{ ev.posture.passwordSet ? (ev.posture.unlocked ? 'password set · unlocked now' : 'password set · locked') : 'no password' }}
            </dd>
          </div>
          <div class="col-span-2 sm:col-span-3">
            <dt class="data-label mb-1">Settings fingerprint</dt>
            <dd class="flex items-center gap-2">
              <code class="min-w-0 truncate font-mono text-xs text-paper-dim">{{ ev.posture.securityDigest }}</code>
              <button type="button" class="shrink-0 text-xs text-brass-400 hover:text-brass-300" @click="copyText(ev.posture.securityDigest)">Copy</button>
            </dd>
            <p class="mt-1 text-[0.6875rem] leading-relaxed text-paper-mute">
              Changes whenever anything a payment depends on changes — the API address, browser access (CORS), the active
              terminal or its configuration — even through a hand edit while the app was off.
            </p>
          </div>
        </dl>

        <div v-if="ev?.adminFile?.worldReadable" class="rounded-xl border border-warn/25 bg-warn/6 px-4 py-3 text-xs leading-relaxed text-warn">
          <p class="mb-2">
            <strong>Every account on this computer can read the administrator file</strong>
            (<code class="font-mono">{{ ev.adminFile.path }}</code>){{ holdsSecrets ? ', so anyone here could sign a message or a receipt as this device.' : ', which allows offline guessing of the password.' }}
            If the background service runs under its own account, narrow the file to it:
          </p>
          <CommandBox :command="`sudo chgrp <service-user> ${ev.adminFile.path} && sudo chmod 640 ${ev.adminFile.path}`" />
        </div>
        <p v-else-if="ev?.adminFile" class="text-xs text-paper-mute">
          Administrator file: <code class="font-mono">{{ ev.adminFile.path }}</code>
        </p>

        <details class="mt-4">
          <summary class="disclosure">What this proves, and what it cannot</summary>
          <p class="mt-3 text-xs leading-relaxed text-paper-dim">
            Webhooks and the receipt key live in the administrator file, so an operator who cannot change the password cannot
            remove them. But the secrets are only as private as that file, and an administrator of the computer can always
            remove them. Signed receipts are the stronger of the two, because the posture travels with every sale.
          </p>
        </details>
      </GlassCard>
    </div>
  </div>
</template>

<script setup lang="ts">
import type { EventsStatus } from '~/composables/api'
import type { AdminOutput } from '~/composables/admin'

const api = useApi()
const auth = useAuth()
const locked = useLocked()
const admin = useAdmin()
const root = ref<HTMLElement>()
useReveal(root)

const ev = ref<EventsStatus | null>(null)
const autoLock = ref<number | string>(0)
const autoLockValid = computed(() => Number.isInteger(autoLock.value) && (autoLock.value as number) >= 0 && (autoLock.value as number) <= 1440)
const kid = ref('')

const activeSubs = computed(() => ev.value?.subscriptions.filter((s) => s.enabled).length ?? 0)
const holdsSecrets = computed(() => Boolean(ev.value?.subscriptions.length || ev.value?.receiptKeyId))

const tiles = computed(() => {
  const s = auth.status.value
  return [
    {
      href: '#lock',
      label: 'Application lock',
      tone: s.passwordSet ? 'ok' : 'muted',
      state: s.passwordSet ? 'on' : 'off',
      detail: s.passwordSet ? 'Changes need the password; payments keep working.' : 'Anyone at this computer can change settings.',
    },
    {
      href: '#events',
      label: 'Webhook events',
      tone: activeSubs.value ? 'ok' : 'muted',
      state: activeSubs.value ? `${activeSubs.value} active` : 'off',
      detail: activeSubs.value ? 'Another server hears about every change that matters.' : 'Nobody else hears when this machine changes.',
    },
    {
      href: '#receipts',
      label: 'Signed receipts',
      tone: ev.value?.receiptKeyId ? 'ok' : 'muted',
      state: ev.value?.receiptKeyId ? 'on' : 'off',
      detail: ev.value?.receiptKeyId ? 'Every sale on the active terminal is signed.' : 'Receivers cannot verify where a sale came from.',
    },
  ] as const
})

async function load() {
  try {
    ev.value = await api.events()
  } catch {
    /* the sidebar already says when the API is unreachable */
  }
}

watch(
  () => auth.status.value.autoLockMinutes,
  (m) => (autoLock.value = m),
  { immediate: true },
)
watch(admin.changed, async () => {
  await auth.refresh()
  await load()
})

// ------------------------------------------------------------------ lock

function setPassword(change: boolean) {
  admin.request({
    title: change ? 'Change the console password' : 'Set a console password',
    explain: change
      ? 'Every open session is signed out; the new password is needed to make changes from then on.'
      : 'From then on, changing settings, terminals or running operations by hand needs this password. Payments over the API keep working.',
    confirmLabel: change ? 'Change password' : 'Set password',
    op: { op: 'setPassword' },
    doneText: () => 'Password set. Every open session has been signed out.',
  })
}

function clearPassword() {
  admin.request({
    title: 'Remove the console password',
    explain: 'Anyone at this computer can then change settings and terminals again. Use this too when the password is forgotten.',
    confirmLabel: 'Remove password',
    danger: true,
    op: { op: 'clearPassword' },
    doneText: () => 'Password removed. The console is open again.',
  })
}

function applyAutoLock() {
  if (!autoLockValid.value) return
  const minutes = autoLock.value as number
  admin.request({
    title: 'Change auto-lock',
    explain: minutes ? `An unlocked console locks itself after ${minutes} idle minutes.` : 'An unlocked console then stays unlocked until it is locked by hand or restarted.',
    confirmLabel: 'Apply',
    op: { op: 'autoLock', minutes },
    doneText: () => (minutes ? `Auto-lock set to ${minutes} minutes.` : 'Auto-lock turned off.'),
  })
}

// ---------------------------------------------------------------- events

const adding = ref(false)
const newUrl = ref('')
const allEvents = ref(true)
const picked = ref<string[]>([])

const urlProblem = computed(() => {
  if (!newUrl.value) return ''
  if (!/^https?:\/\/[^\s/]+/.test(newUrl.value)) return 'Starts with https:// (or http://) and a host name.'
  return ''
})

function startAdding() {
  newUrl.value = ''
  allEvents.value = true
  picked.value = []
  adding.value = true
}

function addSub() {
  const events = allEvents.value ? ['*'] : [...picked.value]
  admin.request({
    title: 'Add a webhook',
    explain: `Ledger POS will send ${allEvents.value ? 'every event' : events.join(', ')} to ${newUrl.value}, signed with a new secret. If other webhooks exist, they are told about this one too.`,
    confirmLabel: 'Add webhook',
    op: { op: 'eventsAdd', url: newUrl.value, events },
    reveal: (o: AdminOutput) => [
      {
        label: 'Signing secret',
        value: String(o.secret ?? ''),
        hint: 'The receiver verifies each message with it (webhook-id, webhook-timestamp and webhook-signature headers — any Standard Webhooks library does this).',
      },
    ],
    doneText: (o: AdminOutput) => `Webhook ${String(o.id ?? '')} added. Use Test to check that the receiver accepts it.`,
    onDone: () => (adding.value = false),
  })
}

function testSub(id: string) {
  admin.request({
    title: 'Send a test message',
    explain: 'Sends a signed webhook.test message straight away and reports what the receiver answered. Reading the secret to sign it needs the administrator too.',
    confirmLabel: 'Send test',
    op: { op: 'eventsTest', id },
    doneText: (o: AdminOutput) => {
      const r = (o.results as { delivered?: boolean; status?: number; error?: string }[] | undefined)?.[0]
      if (!r) return 'Sent.'
      return r.delivered ? `Delivered — the receiver answered ${r.status}.` : `Not accepted: ${r.error ?? `the receiver answered ${r.status}`}.`
    },
  })
}

function toggleSub(id: string, enabled: boolean) {
  admin.request({
    title: enabled ? 'Resume this webhook' : 'Pause this webhook',
    explain: enabled
      ? 'Deliveries to it start again, with the same secret.'
      : 'Nothing is sent to it until it is resumed; its secret and filter are kept. The receiver is told it is being paused.',
    confirmLabel: enabled ? 'Resume' : 'Pause',
    op: { op: 'eventsEnable', id, enabled },
    doneText: () => (enabled ? 'Resumed.' : 'Paused.'),
  })
}

function removeSub(id: string, url: string) {
  admin.request({
    title: 'Remove this webhook',
    explain: `Nothing more is sent to ${url}, and its secret is deleted. It receives one last message saying so.`,
    confirmLabel: 'Remove',
    danger: true,
    op: { op: 'eventsRemove', id },
    doneText: () => 'Removed.',
  })
}

// -------------------------------------------------------------- receipts

function setReceiptKey(k: string | null, rotate: boolean) {
  admin.request({
    title: rotate ? 'Rotate the receipt key' : 'Turn on signed receipts',
    explain: rotate
      ? 'A new key, with a new key id, replaces the current one at once. Receipts signed from now on verify only with the new key — give it to the receivers.'
      : 'Every successful operation on the active terminal will carry a signed attestation token.',
    confirmLabel: rotate ? 'Rotate key' : 'Turn on',
    op: { op: 'receiptKeySet', kid: k },
    reveal: (o: AdminOutput) => [
      { label: `Receipt key (key id ${String(o.kid ?? '')})`, value: String(o.secret ?? ''), hint: 'Whoever verifies the receipts needs it. Keep it as private as a password.' },
    ],
    doneText: () => (rotate ? 'New key in use.' : 'Receipts are now signed.'),
    onDone: () => (kid.value = ''),
  })
}

function clearReceiptKey() {
  admin.request({
    title: 'Turn off signed receipts',
    explain: 'Replies no longer carry an attestation, and the key is deleted. A receiver that expects one will treat sales as unverified.',
    confirmLabel: 'Turn off',
    danger: true,
    op: { op: 'receiptKeyClear' },
    doneText: () => 'Receipt signing stopped.',
  })
}

function timeOf(ts: string) {
  const d = new Date(ts)
  return Number.isNaN(d.getTime()) ? ts : d.toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', second: '2-digit' })
}

let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  load()
  // A change made in a terminal shows up here by itself.
  timer = setInterval(load, 5000)
})
onBeforeUnmount(() => clearInterval(timer))
</script>
