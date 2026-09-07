/**
 * Desktop integration for the AppImage build.
 *
 * An AppImage is one portable file; nothing puts it on the application menu or
 * makes `posd` reachable. These commands do that under the user's own
 * directories — no privileges, and removal is exact.
 *
 * `available` is false for packaged installs (.deb/.rpm), where the packaging
 * already did this and there is nothing to offer.
 */
export interface InstallInfo {
  available: boolean
  installed: boolean
  installedPath?: string
  promptDismissed: boolean
}

const EMPTY: InstallInfo = { available: false, installed: false, promptDismissed: true }

function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

async function cmd<T>(name: string): Promise<T | null> {
  if (!inTauri()) return null
  try {
    const { invoke } = await import('@tauri-apps/api/core')
    return await invoke<T>(name)
  } catch (e) {
    throw e instanceof Error ? e : new Error(String(e))
  }
}

export const useInstallInfo = () => useState<InstallInfo>('install-info', () => ({ ...EMPTY }))

/** The offer is made once: only from an AppImage, only while not installed. */
export const useShowInstallWizard = () => {
  const info = useInstallInfo()
  return computed(() => info.value.available && !info.value.installed && !info.value.promptDismissed)
}

export function useInstall() {
  const info = useInstallInfo()

  async function refresh() {
    const res = await cmd<InstallInfo>('install_status')
    if (res) info.value = res
  }

  async function install() {
    const res = await cmd<InstallInfo>('install_app')
    if (res) info.value = res
  }

  async function uninstall() {
    const res = await cmd<InstallInfo>('uninstall_app')
    if (res) info.value = res
  }

  async function dismiss() {
    await cmd<void>('dismiss_install_prompt')
    info.value = { ...info.value, promptDismissed: true }
  }

  return { info, refresh, install, uninstall, dismiss }
}
