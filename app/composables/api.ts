/**
 * Typed client for the local REST API.
 *
 * Base URL resolution: inside Tauri we ask the shell (`server_info`) so the UI
 * follows whatever port the user configured; in a plain browser (nuxt dev
 * without Tauri) we fall back to the default port 4373.
 */

// ----------------------------------------------------------------- types

export interface ParamSpec {
  name: string
  type: string
  required: boolean
  description: string
  default?: unknown
  enum?: unknown[]
  items?: Record<string, unknown>
  example?: unknown
}

export interface FunctionSpec {
  id: string
  title: string
  category: string
  summary: string
  description: string
  params: ParamSpec[]
  returns: string
  requiresTerminal: boolean
  longRunning: boolean
  example?: Record<string, unknown>
}

export interface ProviderStatus {
  configured: boolean
  configurationHint?: string
  sessionOpen: boolean
  busy: boolean
}

export interface ProviderMeta {
  id: string
  name: string
  vendor: string
  model?: string
  description: string
  protocol?: string
  transports: string[]
  capabilities: string[]
  docsUrl?: string
  version: string
}

export interface ProviderSummary extends ProviderMeta {
  active: boolean
  status: ProviderStatus
  functionCount: number
}

/** Reachability of the physical link to a terminal. */
export interface LinkStatus {
  state: 'up' | 'down' | 'unknown' | 'notApplicable'
  detail: string
  hint?: string
  latencyMs?: number
}

export interface ProviderDetail {
  metadata: ProviderMeta
  active: boolean
  status: ProviderStatus
  configSchema: Record<string, unknown>
  config: Record<string, unknown>
  functions: FunctionSpec[]
}

export interface ServerSettings {
  host: string
  port: number
  corsAllowedOrigins: string[]
  corsAllowAll: boolean
}

export interface AppBehaviorSettings {
  minimizeToTrayOnClose: boolean
  startAtBoot: boolean
  startMinimized: boolean
}

export interface Settings {
  server: ServerSettings
  app: AppBehaviorSettings
  providers: { active: string; configs: Record<string, unknown> }
}

export interface ActivityEntry {
  id: string
  timestamp: string
  provider: string
  function: string
  ok: boolean
  responseCode?: string
  summary?: string
  durationMs: number
  error?: string
}

export interface AuthStatus {
  passwordSet: boolean
  locked: boolean
  autoLockMinutes: number
}

export interface SystemInfo {
  name: string
  version: string
  platform: string
  arch: string
  activeProvider: string
  server: { running: boolean; host: string; port: number; error?: string }
  docsPath: string
  openapiPath: string
  settingsFile: string
}

export class ApiError extends Error {
  code: string
  status: number
  constructor(status: number, code: string, message: string) {
    super(message)
    this.code = code
    this.status = status
  }
}

// ------------------------------------------------------------- base URL

const FALLBACK_BASE = 'http://127.0.0.1:4373'

function inTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

async function fetchServerInfoFromShell(): Promise<{ baseUrl: string; docsUrl: string; running: boolean; error?: string } | null> {
  if (!inTauri()) return null
  try {
    const { invoke } = await import('@tauri-apps/api/core')
    return await invoke('server_info')
  } catch {
    return null
  }
}

export const useApiBase = () => useState<string>('api-base', () => FALLBACK_BASE)

/**
 * Session token from `POST /api/v1/auth/unlock`.
 *
 * Deliberately in memory only: reloading the window re-locks the UI, and the
 * token never survives on disk where another user of this machine could read it.
 */
export const useAuthToken = () => useState<string | null>('auth-token', () => null)

export async function resolveApiBase(): Promise<string> {
  const base = useApiBase()
  const info = await fetchServerInfoFromShell()
  if (info?.baseUrl) base.value = info.baseUrl
  return base.value
}

// ---------------------------------------------------------------- client

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const base = useApiBase().value
  const token = useAuthToken()
  const headers: Record<string, string> = {}
  if (body !== undefined) headers['content-type'] = 'application/json'
  if (token.value) headers.authorization = `Bearer ${token.value}`
  let res: Response
  try {
    res = await fetch(`${base}${path}`, {
      method,
      headers: Object.keys(headers).length ? headers : undefined,
      body: body !== undefined ? JSON.stringify(body) : undefined,
    })
  } catch {
    throw new ApiError(0, 'unreachable', 'API server is not reachable')
  }
  const text = await res.text()
  let data: unknown = null
  try {
    data = text ? JSON.parse(text) : null
  } catch {
    data = text
  }
  if (!res.ok) {
    const err = data as { code?: string; error?: string } | null
    // The session expired or was revoked elsewhere — stop presenting a dead token.
    if (err?.code === 'locked') token.value = null
    throw new ApiError(res.status, err?.code ?? 'http_error', err?.error ?? `HTTP ${res.status}`)
  }
  return data as T
}

export function useApi() {
  return {
    get: <T>(path: string) => request<T>('GET', path),
    post: <T>(path: string, body?: unknown) => request<T>('POST', path, body),
    put: <T>(path: string, body?: unknown) => request<T>('PUT', path, body),

    health: () => request<{ status: string; version: string; uptimeSecs: number }>('GET', '/api/v1/health'),
    system: () => request<SystemInfo>('GET', '/api/v1/system'),
    settings: () => request<Settings>('GET', '/api/v1/settings'),
    saveSettings: (s: Settings) =>
      request<{ settings: Settings; serverRestarting: boolean }>('PUT', '/api/v1/settings', s),
    providers: () => request<ProviderSummary[]>('GET', '/api/v1/providers'),
    provider: (id: string) => request<ProviderDetail>('GET', `/api/v1/providers/${encodeURIComponent(id)}`),
    link: (id: string) => request<LinkStatus>('GET', `/api/v1/providers/${encodeURIComponent(id)}/link`),
    setActive: (id: string) => request<ProviderDetail>('PUT', '/api/v1/providers/active', { id }),
    saveProviderConfig: (id: string, config: Record<string, unknown>) =>
      request<Record<string, unknown>>('PUT', `/api/v1/providers/${encodeURIComponent(id)}/config`, config),
    invoke: (id: string, fn: string, params: Record<string, unknown>) =>
      request<Record<string, unknown>>(
        'POST',
        `/api/v1/providers/${encodeURIComponent(id)}/functions/${encodeURIComponent(fn)}/invoke`,
        params,
      ),
    activity: (limit = 50) => request<ActivityEntry[]>('GET', `/api/v1/activity?limit=${limit}`),

    authStatus: () => request<AuthStatus>('GET', '/api/v1/auth/status'),
    unlock: (password: string) =>
      request<{ token: string; autoLockMinutes: number }>('POST', '/api/v1/auth/unlock', { password }),
    lockApp: () => request<{ ok: boolean }>('POST', '/api/v1/auth/lock'),
  }
}

// ------------------------------------------------------- shared app state

export interface ShellInfo {
  baseUrl: string
  docsUrl: string
  running: boolean
  version?: string
  error?: string
}

export const useShellInfo = () => useState<ShellInfo | null>('shell-info', () => null)

/** Open the Swagger docs — via the shell inside Tauri, new tab otherwise. */
export async function openApiDocs() {
  if (inTauri()) {
    try {
      const { invoke } = await import('@tauri-apps/api/core')
      await invoke('open_docs')
      return
    } catch {
      /* fall through */
    }
  }
  const base = useApiBase().value
  window.open(`${base}/docs`, '_blank', 'noopener')
}
