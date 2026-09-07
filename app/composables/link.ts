import type { LinkStatus } from '~/composables/api'

/**
 * Poll a provider's terminal link for a status light.
 *
 * The default interval is deliberately slow: each poll opens a real socket to a
 * payment terminal. The server declines to probe while a transaction owns the
 * provider (reporting `unknown`), so polling can never interrupt a sale — but
 * there is still no reason to knock more often than an operator would notice.
 */
export function useProviderLink(providerId: Ref<string> | string, intervalMs = 15000) {
  const api = useApi()
  const id = computed(() => (typeof providerId === 'string' ? providerId : providerId.value))
  const status = ref<LinkStatus | null>(null)
  let timer: ReturnType<typeof setInterval> | undefined

  async function refresh() {
    if (!id.value) {
      status.value = null
      return
    }
    try {
      status.value = await api.link(id.value)
    } catch {
      // The API itself being unreachable is already reported elsewhere; don't
      // let it masquerade as a terminal that is down.
      status.value = null
    }
  }

  onMounted(() => {
    refresh()
    timer = setInterval(refresh, intervalMs)
  })
  onBeforeUnmount(() => clearInterval(timer))
  watch(id, refresh)

  return { status, refresh }
}

/** Pill tone + label for a link state, or null when there is nothing to show. */
export function linkPill(status: LinkStatus | null): { tone: 'ok' | 'danger' | 'muted'; label: string } | null {
  if (!status || status.state === 'notApplicable') return null
  return {
    up: { tone: 'ok' as const, label: 'terminal up' },
    down: { tone: 'danger' as const, label: 'terminal down' },
    unknown: { tone: 'muted' as const, label: 'terminal unknown' },
  }[status.state]
}
