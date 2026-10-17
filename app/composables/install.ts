/**
 * Desktop integration for the AppImage build.
 *
 * An AppImage is one portable file; nothing puts it on the application menu or
 * gives the background API (`posd`) a home that outlives the file. The first run
 * installs it by itself, under the user's own directories — no privileges, and
 * removal is exact. These commands are Settings' install/uninstall/rollback.
 *
 * `available` is false for packaged installs (.deb/.rpm), where the packaging
 * already did this and there is nothing to offer.
 */
export interface InstallInfo {
  available: boolean
  installed: boolean
  installedPath?: string
  /** This start installed it by itself — say so once. */
  justInstalled: boolean
  /** The version kept for one rollback, when there is one. */
  previousVersion?: string
}

const EMPTY: InstallInfo = { available: false, installed: false, justInstalled: false }

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

  /** Put the previous version back on the menu; it runs from the next start. */
  async function rollback(): Promise<string | null> {
    const version = await cmd<string>('rollback_install')
    await refresh()
    return version
  }

  return { info, refresh, install, uninstall, rollback }
}
