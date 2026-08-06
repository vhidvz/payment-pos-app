import { gsap } from 'gsap'

/** True when the user asked the OS for reduced motion, or fx-lite mode is on. */
export function prefersReducedMotion(): boolean {
  if (typeof window === 'undefined') return true
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches || reducedEffectsActive()
}

/**
 * Editorial reveal: staggered rise-and-fade of `[data-reveal]` children when a
 * page mounts. No-ops (content stays fully visible) under reduced motion.
 */
export function useReveal(root: Ref<HTMLElement | null | undefined>) {
  onMounted(() => {
    const el = root.value
    if (!el || prefersReducedMotion()) return
    const targets = el.querySelectorAll('[data-reveal]')
    if (!targets.length) return
    gsap.fromTo(
      targets,
      { y: 22, opacity: 0 },
      { y: 0, opacity: 1, duration: 0.8, stagger: 0.07, ease: 'power3.out', clearProps: 'transform' },
    )
  })
}

/** Micro-interaction: pulse an element once (e.g. after a copy). */
export function pulse(el: HTMLElement | null) {
  if (!el || prefersReducedMotion()) return
  gsap.fromTo(el, { scale: 1 }, { scale: 1.05, duration: 0.12, yoyo: true, repeat: 1, ease: 'power2.out' })
}

// ------------------------------------------------------------------ toasts

export interface Toast {
  id: number
  kind: 'ok' | 'error' | 'info'
  text: string
}

let toastSeq = 0

export const useToasts = () => useState<Toast[]>('toasts', () => [])

export function toast(kind: Toast['kind'], text: string) {
  const list = useToasts()
  const id = ++toastSeq
  list.value = [...list.value, { id, kind, text }]
  setTimeout(() => {
    list.value = list.value.filter((t) => t.id !== id)
  }, 4200)
}

export async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    toast('ok', 'Copied to clipboard')
  } catch {
    toast('error', 'Could not access the clipboard')
  }
}
