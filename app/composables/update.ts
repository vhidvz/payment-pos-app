/**
 * Update mode (spec §3.1/§5): the shell decides before Tauri starts whether this run is
 * the app or an update screen; `boot_mode` tells the frontend which.
 */
export type BootMode = 'normal' | 'updateOffer' | 'replaceUnknown' | 'newerInstalled'

export interface BootInfo {
  mode: BootMode
  current: string
  installed?: string
  reason?: 'badVersionKey' | 'unreconciledJournal' | 'unreadableEntry'
  detail?: string
  running: boolean
}

export type StepId = 'install' | 'stopRunning' | 'service' | 'launch'
export type StepState = 'running' | 'waiting' | 'done' | 'failed'

export interface Progress {
  step: StepId
  state: StepState
  message: string
}

export type EngineOutcome =
  | { kind: 'started'; pid: number }
  | { kind: 'runningNotStopped' }
  | { kind: 'launchFailed'; reason: string }
  | { kind: 'installFailed'; reason: string }

function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

async function call<T>(name: string): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<T>(name)
}

export const useBootInfo = () => useState<BootInfo | null>('boot-info', () => null)

/** The version the API on the configured port reports — set by the layout's health poll. */
export const useApiVersion = () => useState<string | null>('api-version', () => null)

export async function loadBootInfo(): Promise<BootInfo> {
  const normal: BootInfo = { mode: 'normal', current: '', running: false }
  if (!inTauri()) return normal
  try {
    return await call<BootInfo>('boot_mode')
  } catch {
    return normal
  }
}

export function useUpdater() {
  const steps = ref<Partial<Record<StepId, Progress>>>({})
  const outcome = ref<EngineOutcome | null>(null)
  const busy = ref(false)

  async function run(command: 'start_update' | 'relaunch_update' | 'rollback_update') {
    busy.value = true
    outcome.value = null
    steps.value = {}
    const { listen } = await import('@tauri-apps/api/event')
    const stop = await listen<Progress>('update://progress', (e) => {
      steps.value = { ...steps.value, [e.payload.step]: e.payload }
    })
    try {
      outcome.value = await call<EngineOutcome>(command)
    } catch (e) {
      outcome.value = { kind: 'installFailed', reason: e instanceof Error ? e.message : String(e) }
    } finally {
      stop()
      busy.value = false
    }
  }

  return {
    steps,
    outcome,
    busy,
    start: () => run('start_update'),
    relaunch: () => run('relaunch_update'),
    rollback: () => run('rollback_update'),
    openInstalled: () => call<void>('open_installed'),
    quit: () => call<void>('quit_app'),
  }
}
