import type { AuthStatus } from '~/composables/api'

/**
 * Application lock.
 *
 * An administrator sets the password with `sudo posd admin set-password`; it is
 * never settable from here, which is why there is no `setPassword` below. This
 * composable only unlocks, locks, and reports state.
 *
 * The gate on the Operations runner is a UI gate: the REST invoke endpoint stays
 * open by design so tills and integrations keep working while the app is locked.
 */
const EMPTY: AuthStatus = { passwordSet: false, locked: false, autoLockMinutes: 15 }

export const useAuthStatus = () => useState<AuthStatus>('auth-status', () => ({ ...EMPTY }))

/** True when a password exists AND this session has not unlocked it. */
export const useLocked = () => {
  const status = useAuthStatus()
  return computed(() => status.value.passwordSet && status.value.locked)
}

/** Whether the unlock dialog is on screen. */
export const useUnlockPrompt = () => useState<boolean>('unlock-prompt', () => false)

/** Ask the user for the password. Call from any action a lock should block. */
export function requestUnlock() {
  useUnlockPrompt().value = true
}

export function useAuth() {
  const api = useApi()
  const token = useAuthToken()
  const status = useAuthStatus()

  async function refresh() {
    try {
      status.value = await api.authStatus()
    } catch {
      // A server we cannot reach is reported by the sidebar; don't also claim
      // the app is locked or open on no evidence.
    }
  }

  async function unlock(password: string) {
    const res = await api.unlock(password)
    token.value = res.token
    await refresh()
  }

  async function lock() {
    try {
      await api.lockApp()
    } finally {
      token.value = null
      await refresh()
    }
  }

  return { status, token, refresh, unlock, lock }
}
