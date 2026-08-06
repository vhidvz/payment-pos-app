<template>
  <div aria-hidden="true" class="pointer-events-none fixed inset-0 -z-10 overflow-hidden">
    <!-- base wash -->
    <div class="absolute inset-0 bg-ink-950" />
    <!-- decorative layers cost full-viewport blends — skip them on weak GPUs -->
    <template v-if="!reduced">
    <!-- brass aurora, top left -->
    <div
      class="absolute -top-[30%] -left-[12%] h-[70vh] w-[70vw] rounded-full opacity-[0.13]"
      style="background: radial-gradient(closest-side, #c9a055, transparent 70%)"
    />
    <!-- cool counter-glow, bottom right -->
    <div
      class="absolute -right-[18%] -bottom-[35%] h-[80vh] w-[70vw] rounded-full opacity-[0.10]"
      style="background: radial-gradient(closest-side, #35507a, transparent 70%)"
    />
    <!-- faint architectural grid -->
    <div
      class="absolute inset-0 opacity-[0.05]"
      style="
        background-image:
          linear-gradient(to right, #4a5670 1px, transparent 1px),
          linear-gradient(to bottom, #4a5670 1px, transparent 1px);
        background-size: 88px 88px;
        mask-image: radial-gradient(ellipse 90% 70% at 50% 0%, black 30%, transparent 75%);
      "
    />
    <!-- film grain -->
    <div
      class="absolute inset-0 opacity-[0.05] mix-blend-overlay"
      :style="{ backgroundImage: `url(&quot;${noise}&quot;)` }"
    />
    <!-- vignette -->
    <div
      class="absolute inset-0"
      style="background: radial-gradient(ellipse 120% 90% at 50% 40%, transparent 55%, rgb(4 5 9 / 0.55) 100%)"
    />
    </template>
  </div>
</template>

<script setup lang="ts">
const reduced = useReducedEffects()

const noise =
  'data:image/svg+xml,' +
  encodeURIComponent(
    `<svg xmlns="http://www.w3.org/2000/svg" width="180" height="180"><filter id="n"><feTurbulence type="fractalNoise" baseFrequency="0.9" numOctaves="2" stitchTiles="stitch"/><feColorMatrix type="saturate" values="0"/></filter><rect width="100%" height="100%" filter="url(%23n)" opacity="0.6"/></svg>`,
  )
</script>
