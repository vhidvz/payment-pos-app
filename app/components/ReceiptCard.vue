<template>
  <div class="glass overflow-hidden !rounded-2xl">
    <!-- header strip -->
    <div
      class="flex items-center justify-between gap-4 border-b px-6 py-4 hairline"
      :class="verdict === 'ok' ? 'bg-ok/6' : verdict === 'fail' ? 'bg-danger/6' : 'bg-white/2'"
    >
      <div class="flex items-center gap-3">
        <span
          class="flex size-8 items-center justify-center rounded-full"
          :class="verdict === 'ok' ? 'bg-ok/15 text-ok' : verdict === 'fail' ? 'bg-danger/15 text-danger' : 'bg-info/15 text-info'"
          aria-hidden="true"
        >
          <svg v-if="verdict === 'ok'" class="size-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5" /></svg>
          <svg v-else-if="verdict === 'fail'" class="size-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12" /></svg>
          <svg v-else class="size-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><circle cx="12" cy="12" r="9"/><path d="M12 8h.01M12 12v4"/></svg>
        </span>
        <div class="leading-tight">
          <div class="text-sm font-medium text-paper">
            {{ verdict === 'ok' ? 'Approved' : verdict === 'fail' ? (txn.timedOut ? 'No response' : 'Declined / failed') : 'Result' }}
          </div>
          <div v-if="txn.responseCode !== undefined" class="font-mono text-xs text-paper-mute tabular">
            code {{ txn.responseCode }}<span v-if="txn.responseMessage"> — {{ txn.responseMessage }}</span>
          </div>
        </div>
      </div>
      <div class="text-right text-xs text-paper-mute">
        <div class="font-mono">{{ meta }}</div>
        <div v-if="durationMs !== undefined" class="tabular">{{ (durationMs / 1000).toFixed(2) }}s</div>
      </div>
    </div>

    <div class="space-y-5 px-6 py-5">
      <!-- headline amount -->
      <div v-if="amount" class="flex items-baseline gap-2">
        <span class="display text-[2rem] text-paper tabular">{{ amount }}</span>
        <span class="text-sm text-paper-mute">Rials</span>
      </div>

      <!-- key facts -->
      <dl v-if="facts.length" class="grid grid-cols-2 gap-x-8 gap-y-3 sm:grid-cols-3">
        <div v-for="f in facts" :key="f.label">
          <dt class="data-label mb-0.5">{{ f.label }}</dt>
          <dd class="font-mono text-[0.8125rem] break-all text-paper-dim tabular">{{ f.value }}</dd>
        </div>
      </dl>

      <!-- card segments (POS-started init) -->
      <div v-if="segments.length">
        <div class="data-label mb-2">Routing options</div>
        <div class="flex flex-wrap gap-2">
          <span v-for="s in segments" :key="s.code" class="glass-inset px-3 py-1.5 font-mono text-xs text-paper-dim">
            {{ s.code }} · {{ s.label || '—' }}
          </span>
        </div>
      </div>

      <!-- authorized operations -->
      <div v-if="authFlags.length">
        <div class="data-label mb-2">Provisioned operations</div>
        <div class="grid grid-cols-3 gap-2">
          <div v-for="f in authFlags" :key="f.name" class="flex items-center gap-2 text-[0.8125rem]">
            <span class="size-1.5 rounded-full" :class="f.on ? 'bg-ok' : 'bg-paper-mute/40'" />
            <span :class="f.on ? 'text-paper-dim' : 'text-paper-mute line-through decoration-paper-mute/40'">{{ f.name }}</span>
          </div>
        </div>
      </div>

      <!-- report rows -->
      <div v-if="rows.length" class="glass-inset overflow-x-auto">
        <table class="w-full text-left text-[0.8125rem]">
          <thead>
            <tr class="border-b hairline text-[0.6875rem] tracking-[0.14em] text-paper-mute uppercase">
              <th class="px-3 py-2 font-medium">Date</th>
              <th class="px-3 py-2 font-medium">Trace</th>
              <th class="px-3 py-2 text-right font-medium">Amount</th>
              <th class="px-3 py-2 font-medium">RRN</th>
              <th class="px-3 py-2 font-medium">Card</th>
              <th class="px-3 py-2 font-medium">Bank</th>
            </tr>
          </thead>
          <tbody class="font-mono tabular">
            <tr v-for="(r, i) in rows" :key="i" class="border-b hairline last:border-0">
              <td class="px-3 py-2 text-paper-dim">{{ r.date }}</td>
              <td class="px-3 py-2 text-paper-dim">{{ r.traceNumber }}</td>
              <td class="px-3 py-2 text-right text-paper">{{ Number(r.amount).toLocaleString('en-US') }}</td>
              <td class="px-3 py-2 text-paper-dim">{{ r.rrn }}</td>
              <td class="px-3 py-2 text-paper-dim">{{ r.cardMask }}</td>
              <td class="px-3 py-2 text-paper-dim">{{ r.bank }}</td>
            </tr>
          </tbody>
        </table>
      </div>

      <!-- totals report -->
      <dl v-if="totals.length" class="grid grid-cols-2 gap-x-8 gap-y-3 sm:grid-cols-3">
        <div v-for="t in totals" :key="t.label">
          <dt class="data-label mb-0.5">{{ t.label }}</dt>
          <dd class="font-mono text-sm text-paper-dim tabular">{{ t.value }}</dd>
        </div>
      </dl>

      <!-- signed receipt -->
      <div v-if="signed" class="rounded-xl border px-4 py-3" :class="postureMoved ? 'border-warn/30 bg-warn/5' : 'border-ok/25 bg-ok/5'">
        <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
          <div class="flex items-center gap-2 text-sm font-medium" :class="postureMoved ? 'text-warn' : 'text-ok'">
            <svg class="size-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" /><path d="m9 12 2 2 4-4" /></svg>
            Signed receipt
          </div>
          <button type="button" class="text-xs text-brass-400 hover:text-brass-300" @click="copyText(String(result.attestation))">Copy token</button>
        </div>
        <dl class="grid grid-cols-2 gap-x-6 gap-y-2 text-xs sm:grid-cols-4">
          <div><dt class="data-label mb-0.5">Key id</dt><dd class="font-mono text-paper-dim">{{ signed.header.kid ?? '—' }}</dd></div>
          <div><dt class="data-label mb-0.5">Signed at</dt><dd class="text-paper-dim tabular">{{ signedAt }}</dd></div>
          <div class="col-span-2"><dt class="data-label mb-0.5">Receipt id (jti)</dt><dd class="font-mono break-all text-paper-dim">{{ signed.claims.jti }}</dd></div>
        </dl>
        <p class="mt-2 text-[0.6875rem] leading-relaxed" :class="postureMoved ? 'text-warn' : 'text-paper-mute'">
          {{
            postureMoved
              ? 'The machine\'s setup changed while this operation ran (postureChanged: true) — a careful receiver will not trust it.'
              : 'The receiver verifies this token with the receipt key and should store its id against the order, so one receipt pays for one sale.'
          }}
        </p>
      </div>

      <!-- raw -->
      <details class="group">
        <summary class="cursor-pointer text-xs text-paper-mute transition-colors select-none hover:text-paper-dim">
          Raw response
        </summary>
        <div class="mt-3">
          <JsonView :value="result" />
        </div>
      </details>
    </div>
  </div>
</template>

<script setup lang="ts">
const props = defineProps<{
  result: Record<string, unknown>
  provider: string
  fnId: string
  durationMs?: number
}>()

type Rec = Record<string, unknown>

/** Report/auth results wrap the transaction under `result`. */
const txn = computed<Rec>(() => {
  const inner = props.result?.result
  if (inner && typeof inner === 'object') return inner as Rec
  return props.result ?? {}
})

const verdict = computed<'ok' | 'fail' | 'info'>(() => {
  const ok = (props.result?.ok ?? txn.value.ok) as boolean | undefined
  if (ok === true) return 'ok'
  if (ok === false) return 'fail'
  return 'info'
})

const meta = computed(() => `${props.provider} · ${props.fnId}`)

/** The `attestation` an administrator's receipt key adds — read for display, not verified. */
const signed = computed(() => decodeJws(props.result?.attestation))
const postureMoved = computed(() => signed.value?.claims.postureChanged === true)
const signedAt = computed(() => {
  const iat = Number(signed.value?.claims.iat)
  return Number.isFinite(iat) ? new Date(iat * 1000).toLocaleTimeString() : '—'
})

const amount = computed(() => {
  const a = (txn.value.amount ?? txn.value.effectiveAmount ?? props.result?.amountRials) as
    | string
    | number
    | undefined
  if (a === undefined || a === '' || a === null) return ''
  const n = Number(a)
  return Number.isFinite(n) ? n.toLocaleString('en-US') : String(a)
})

const FACT_KEYS: Array<[string, string]> = [
  ['terminalId', 'Terminal'],
  ['rrn', 'RRN'],
  ['traceNumber', 'Trace'],
  ['serialId', 'Serial'],
  ['transactionDate', 'Date'],
  ['cardMask', 'Card'],
  ['chargePin', 'Charge PIN'],
  ['chargeSerial', 'Charge serial'],
  ['chargeEmergencyNumber', 'Emergency no.'],
  ['posVersion', 'POS version'],
  ['reason', 'Reason'],
  ['billId', 'Bill id'],
  ['paymentId', 'Payment id'],
  ['categoryEn', 'Category'],
  ['en', 'Category'],
  ['code', 'Code'],
]

const facts = computed(() => {
  const source = { ...txn.value, ...props.result }
  const out: Array<{ label: string; value: string }> = []
  for (const [key, label] of FACT_KEYS) {
    const v = source[key]
    if (v !== undefined && v !== null && v !== '' && typeof v !== 'object') {
      out.push({ label, value: String(v) })
    }
  }
  return out
})

const segments = computed(() => {
  const s = props.result?.segments
  return Array.isArray(s) ? (s as Array<{ code: string; label: string }>) : []
})

const AUTH_KEYS = ['balance', 'bill', 'report', 'mciBill', 'pinCharge', 'purchase', 'topupCharge', 'tciBill', 'paymentService']
const authFlags = computed(() => {
  if (!AUTH_KEYS.every((k) => typeof props.result?.[k] === 'boolean')) return []
  return AUTH_KEYS.map((k) => ({ name: k, on: props.result[k] as boolean }))
})

const rows = computed(() => {
  const r = props.result?.rows
  return Array.isArray(r) ? (r as Rec[]) : []
})

const TOTAL_KEYS: Array<[string, string]> = [
  ['purchaseCount', 'Purchases'],
  ['purchaseAmount', 'Purchase total'],
  ['billsCount', 'Bills'],
  ['billsTotalAmount', 'Bills total'],
  ['pinChargeCount', 'PIN charges'],
  ['pinChargeAmount', 'PIN charge total'],
  ['topupChargeCount', 'Top-ups'],
  ['topupChargeAmount', 'Top-up total'],
  ['groupChargeCount', 'Group charges'],
  ['groupChargeAmount', 'Group charge total'],
]
const totals = computed(() => {
  if (typeof props.result?.purchaseCount !== 'number') return []
  return TOTAL_KEYS.map(([k, label]) => ({
    label,
    value: Number(props.result[k] ?? 0).toLocaleString('en-US'),
  }))
})
</script>
