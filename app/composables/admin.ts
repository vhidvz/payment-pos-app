/**
 * Administrator actions — the password, auto-lock, webhooks and the receipt key.
 *
 * They live in a root-owned file so that knowing the console password is not enough to
 * switch them off. The console therefore never writes them itself: the desktop shell runs
 * `posd admin` through the operating system's own password dialog (pkexec), exactly as
 * `sudo posd admin …` would. Where it cannot ask, the dialog shows that terminal command.
 *
 * One dialog serves every action (AdminDialog.vue); pages call `useAdmin().request(…)`.
 */

export type AdminOp =
  | { op: 'setPassword'; password: string }
  | { op: 'clearPassword' }
  | { op: 'autoLock'; minutes: number }
  | { op: 'eventsAdd'; url: string; events: string[] }
  | { op: 'eventsRemove'; id: string }
  | { op: 'eventsEnable'; id: string; enabled: boolean }
  | { op: 'eventsTest'; id: string | null }
  | { op: 'receiptKeySet'; kid: string | null }
  | { op: 'receiptKeyClear' }

export type AdminOutput = Record<string, unknown>

export type AdminRunResult =
  | { kind: 'done'; output: AdminOutput }
  | { kind: 'cancelled' }
  | { kind: 'manual'; command: string; reason: string }
  | { kind: 'failed'; message: string; command: string }

export interface AdminPreview {
  inApp: boolean
  command: string
  reason?: string
}

/** A secret posd answered with — shown once, never stored. */
export interface Reveal {
  label: string
  value: string
  hint?: string
}

export interface AdminAction {
  title: string
  /** What will happen, in plain words. */
  explain: string
  confirmLabel: string
  danger?: boolean
  /** The operation; for `setPassword` the dialog asks for the password itself. */
  op: AdminOp | { op: 'setPassword' }
  /** Secrets to show once after success. */
  reveal?: (output: AdminOutput) => Reveal[]
  /** What to say after success. */
  doneText?: (output: AdminOutput) => string
  onDone?: (output: AdminOutput) => void
}

function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

/** The terminal form when there is no desktop shell to ask (a plain browser in dev). */
function browserCommand(op: AdminOp | { op: 'setPassword' }): string {
  const q = (w: string) => (/^[A-Za-z0-9_./:,=@%+-]+$/.test(w) ? w : `'${w.replace(/'/g, `'\\''`)}'`)
  const words: string[] = (() => {
    switch (op.op) {
      case 'setPassword': return ['set-password']
      case 'clearPassword': return ['clear-password']
      case 'autoLock': return ['auto-lock', String(op.minutes)]
      case 'eventsAdd': {
        const pick = op.events.filter((e) => e !== '*')
        return ['events', 'add', op.url, ...(pick.length ? ['--events', op.events.join(',')] : [])]
      }
      case 'eventsRemove': return ['events', 'remove', op.id]
      case 'eventsEnable': return ['events', op.enabled ? 'enable' : 'disable', op.id]
      case 'eventsTest': return ['events', 'test', ...(op.id ? [op.id] : [])]
      case 'receiptKeySet': return ['receipt-key', 'set', ...(op.kid ? ['--kid', op.kid] : [])]
      case 'receiptKeyClear': return ['receipt-key', 'clear']
    }
  })()
  return ['sudo', 'posd', 'admin', ...words.map(q)].join(' ')
}

async function shell<T>(name: string, args: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<T>(name, args)
}

export async function adminPreview(op: AdminOp | { op: 'setPassword' }): Promise<AdminPreview> {
  if (!inTauri()) {
    return {
      inApp: false,
      command: browserCommand(op),
      reason: 'Administrator changes are made from the desktop app, or in a terminal on this machine.',
    }
  }
  // The preview only needs the shape; a placeholder password never leaves this call.
  const probe = op.op === 'setPassword' ? { op: 'setPassword', password: '' } : op
  return shell<AdminPreview>('admin_preview', { op: probe })
}

export async function adminRun(op: AdminOp): Promise<AdminRunResult> {
  if (!inTauri()) {
    const p = await adminPreview(op)
    return { kind: 'manual', command: p.command, reason: p.reason ?? '' }
  }
  try {
    return await shell<AdminRunResult>('admin_run', { op })
  } catch (e) {
    return { kind: 'failed', message: e instanceof Error ? e.message : String(e), command: browserCommand(op) }
  }
}

// One dialog for the whole app; a plain module ref keeps callbacks out of Nuxt's payload.
const current = shallowRef<AdminAction | null>(null)
const changed = ref(0)
const busy = ref(false)

export function useAdmin() {
  return {
    /** The action on screen, if any. */
    current,
    /** Bumped after every successful change — pages watch it to reload. */
    changed,
    request(action: AdminAction) {
      // One administrator prompt at a time; the dialog is modal, this is the backstop.
      if (busy.value) return
      current.value = action
    },
    /** True while the OS password dialog is open. */
    busy,
    close() {
      current.value = null
    },
  }
}
