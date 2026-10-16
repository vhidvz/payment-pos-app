/**
 * Read a signed receipt's header and claims for display. This does **not** verify it —
 * the console never holds the receipt key; the receiver verifies with its copy.
 */
export interface DecodedJws {
  header: Record<string, unknown>
  claims: Record<string, unknown>
}

function part(b64url: string): Record<string, unknown> {
  const b64 = b64url.replace(/-/g, '+').replace(/_/g, '/').padEnd(Math.ceil(b64url.length / 4) * 4, '=')
  const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0))
  const v = JSON.parse(new TextDecoder().decode(bytes))
  if (!v || typeof v !== 'object' || Array.isArray(v)) throw new Error('not an object')
  return v as Record<string, unknown>
}

export function decodeJws(token: unknown): DecodedJws | null {
  if (typeof token !== 'string') return null
  const pieces = token.split('.')
  if (pieces.length !== 3) return null
  try {
    return { header: part(pieces[0]!), claims: part(pieces[1]!) }
  } catch {
    return null
  }
}
