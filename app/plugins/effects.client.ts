/**
 * Applies the reduced-effects mode before first paint so weak machines never
 * render (or composite) the expensive blurred/animated chrome even once.
 */
export default defineNuxtPlugin(() => {
  initReducedEffects()
})
