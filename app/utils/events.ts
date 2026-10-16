/**
 * What each webhook event means, for people — the Security page's picker and the Guide.
 * The list of types itself comes from the API (`eventTypes`); a type this file does not
 * know yet still shows, under its raw name.
 */
export interface EventInfo {
  group: 'App' | 'Lock' | 'Settings' | 'Monitoring' | 'Test'
  when: string
}

export const EVENT_INFO: Record<string, EventInfo> = {
  'app.started': { group: 'App', when: 'Ledger POS starts. Carries the settings fingerprint, so a hand-edited settings file shows up here.' },
  'app.stopping': { group: 'App', when: 'Ledger POS quits normally. A crash or a cut network sends nothing.' },
  'security.unlocked': { group: 'Lock', when: 'Someone unlocked the console with the password.' },
  'security.locked': { group: 'Lock', when: 'A session was locked on purpose.' },
  'security.password_set': { group: 'Lock', when: 'The administrator set or changed the console password.' },
  'security.password_cleared': { group: 'Lock', when: 'The administrator removed the console password.' },
  'security.auto_lock_changed': { group: 'Lock', when: 'The idle time before re-locking changed.' },
  'settings.changed': { group: 'Settings', when: 'Any setting changed — it names which ones, never their values. Several quick changes may arrive as one.' },
  'events.config_changed': { group: 'Monitoring', when: 'Webhooks were added, removed, paused or resumed — also sent to the old addresses, so one cannot be swapped quietly.' },
  'receipts.key_changed': { group: 'Monitoring', when: 'The receipt-signing key was set, rotated or cleared.' },
  'webhook.test': { group: 'Test', when: 'Sent by the Test button, to check the receiver.' },
}

export const EVENT_GROUPS: EventInfo['group'][] = ['App', 'Lock', 'Settings', 'Monitoring', 'Test']

export function eventInfo(type: string): EventInfo {
  return EVENT_INFO[type] ?? { group: 'App', when: '' }
}

/** A subscription's filter, said plainly: `["*"]` → "every event". */
export function describePatterns(patterns: string[]): string {
  if (!patterns.length || patterns.includes('*')) return 'every event'
  return patterns.join(', ')
}

export const OUTCOME_TONE: Record<string, 'ok' | 'warn' | 'danger' | 'muted'> = {
  delivered: 'ok',
  retrying: 'warn',
  gone: 'danger',
  expired: 'danger',
  orphaned: 'muted',
}

export const OUTCOME_TEXT: Record<string, string> = {
  delivered: 'The receiver accepted it.',
  retrying: 'Not accepted yet — retried after 5 s, doubling up to 15 min, for 72 hours.',
  gone: 'The receiver answered 410 Gone; nothing more is sent for this message.',
  expired: 'Gave up after 72 hours without a successful delivery.',
  orphaned: 'The webhook was removed before the message could be sent.',
}
