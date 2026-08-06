/**
 * Reduced-effects ("fx-lite") mode for weak GPUs and Raspberry Pi class boards.
 *
 * When on, a `fx-lite` class on <html> swaps translucent blurred panels for
 * solid ones and stops decorative animation (see main.css). The choice is
 * per-device: stored in localStorage, auto-detected on first run.
 */

const STORAGE_KEY = 'ledger-pos:reduced-effects'

/** Heuristic: is this machine likely to struggle with blur/glow compositing? */
export function detectWeakGpu(): boolean {
  try {
    const gl =
      document.createElement('canvas').getContext('webgl') ??
      document.createElement('canvas').getContext('experimental-webgl')
    // No GL at all → rendering is already falling back to software.
    if (!gl || !('getParameter' in gl)) return true
    const dbg = gl.getExtension('WEBGL_debug_renderer_info')
    const renderer = dbg ? String(gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL)) : ''
    // Software rasterizers and Pi-class GPUs.
    if (/llvmpipe|softpipe|swiftshader|software/i.test(renderer)) return true
    if (/videocore|v3d|broadcom/i.test(renderer)) return true
  } catch {
    return true
  }
  if (/aarch64|armv7|raspberry/i.test(navigator.userAgent)) return true
  return (navigator.hardwareConcurrency || 8) <= 4
}

/** Reactive state; initialized by the effects plugin before the app mounts. */
export const useReducedEffects = () => useState<boolean>('fx-reduced', () => false)

function applyClass(reduced: boolean) {
  document.documentElement.classList.toggle('fx-lite', reduced)
}

/** Called once from the client plugin: stored choice wins, else auto-detect. */
export function initReducedEffects() {
  const stored = localStorage.getItem(STORAGE_KEY)
  const reduced = stored !== null ? stored === '1' : detectWeakGpu()
  useReducedEffects().value = reduced
  applyClass(reduced)
}

/** Explicit user choice: apply immediately and remember it on this device. */
export function setReducedEffects(reduced: boolean) {
  useReducedEffects().value = reduced
  localStorage.setItem(STORAGE_KEY, reduced ? '1' : '0')
  applyClass(reduced)
}

/**
 * Non-reactive check usable outside a Nuxt context (GSAP helpers, event
 * handlers): reflects the class the plugin/setter maintain on <html>.
 */
export function reducedEffectsActive(): boolean {
  if (typeof document === 'undefined') return true
  return document.documentElement.classList.contains('fx-lite')
}
