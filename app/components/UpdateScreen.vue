<template>
  <div class="flex min-h-screen items-center justify-center p-8">
    <GlassCard class="w-full max-w-lg">
      <p class="data-label mb-2">Ledger POS</p>
      <h1 class="display mb-2 text-2xl text-paper">{{ title }}</h1>
      <p class="mb-6 text-sm leading-relaxed text-paper-mute">{{ lead }}</p>

      <dl class="mb-6 grid grid-cols-2 gap-4 border-y hairline py-4 font-mono text-xs">
        <div>
          <dt class="data-label mb-1">Installed</dt>
          <dd class="text-paper">
            {{ installedText }}<span v-if="boot.running" class="text-paper-mute"> · running</span>
          </dd>
        </div>
        <div>
          <dt class="data-label mb-1">This file</dt>
          <dd class="text-paper">{{ boot.current }}</dd>
        </div>
      </dl>

      <ol v-if="anyStep" class="mb-6 space-y-2.5" aria-live="polite">
        <li v-for="id in ORDER" :key="id" class="flex items-start gap-3 text-sm">
          <span class="mt-1.5 size-2 shrink-0 rounded-full" :class="dot(steps[id]?.state)" aria-hidden="true" />
          <div>
            <div :class="steps[id] ? 'text-paper' : 'text-paper-mute'">{{ LABELS[id] }}</div>
            <div v-if="steps[id]?.message" class="text-xs leading-relaxed text-paper-mute">{{ steps[id]?.message }}</div>
          </div>
        </li>
      </ol>

      <p v-if="result" class="mb-5 text-sm leading-relaxed" :class="result.tone" role="status">{{ result.text }}</p>

      <div class="flex flex-wrap gap-2">
        <template v-if="!outcome">
          <template v-if="boot.mode === 'updateOffer'">
            <button type="button" class="btn-primary flex-1" :disabled="busy" @click="start">
              {{ busy ? 'Updating…' : 'Update and restart' }}
            </button>
            <button type="button" class="btn-ghost" :disabled="busy" @click="openInstalled">Not now</button>
          </template>
          <template v-else-if="boot.mode === 'replaceUnknown'">
            <!-- The installed version is unknown: replacing is the deliberate choice, not the default. -->
            <button type="button" class="btn-primary flex-1" :disabled="busy" @click="openInstalled">Not now</button>
            <button type="button" class="btn-ghost" :disabled="busy" @click="start">
              {{ busy ? 'Replacing…' : `Replace with ${boot.current}` }}
            </button>
          </template>
          <template v-else-if="boot.mode === 'newerInstalled'">
            <button type="button" class="btn-primary flex-1" @click="openInstalled">Open installed version</button>
            <button type="button" class="btn-ghost" @click="quit">Close</button>
          </template>
        </template>
        <template v-else-if="outcome.kind === 'installFailed'">
          <button type="button" class="btn-primary flex-1" :disabled="busy" @click="start">Retry</button>
          <button type="button" class="btn-ghost" :disabled="busy" @click="quit">Close</button>
        </template>
        <template v-else-if="outcome.kind === 'launchFailed'">
          <button type="button" class="btn-primary flex-1" :disabled="busy" @click="relaunch">Try again</button>
          <button type="button" class="btn-ghost" :disabled="busy" @click="rollback">
            Roll back{{ boot.installed ? ` to ${boot.installed}` : '' }}
          </button>
          <button type="button" class="btn-ghost" :disabled="busy" @click="quit">Close</button>
        </template>
        <template v-else-if="outcome.kind === 'runningNotStopped'">
          <button type="button" class="btn-primary flex-1" @click="quit">Close</button>
        </template>
      </div>
    </GlassCard>
  </div>
</template>

<script setup lang="ts">
import type { BootInfo, StepId, StepState } from '~/composables/update'

const props = defineProps<{ boot: BootInfo }>()
const { steps, outcome, busy, start, relaunch, rollback, openInstalled, quit } = useUpdater()

const ORDER: StepId[] = ['install', 'stopRunning', 'service', 'launch']
const LABELS: Record<StepId, string> = {
  install: 'Replace the installed copy',
  stopRunning: 'Close the running copy after its sale',
  service: 'Restart the background API on the new version',
  launch: 'Start the new version',
}

const anyStep = computed(() => ORDER.some((id) => steps.value[id]))
const installedText = computed(() => props.boot.installed ?? (props.boot.mode === 'updateOffer' ? 'before 1.0' : 'unknown'))

const title = computed(() => {
  switch (props.boot.mode) {
    case 'updateOffer':
      return `Update Ledger POS to ${props.boot.current}?`
    case 'replaceUnknown':
      return props.boot.reason === 'unreconciledJournal' ? 'The installation needs a repair' : 'Replace the installed copy?'
    case 'newerInstalled':
      return 'A newer version is installed'
    default:
      return 'Ledger POS'
  }
})

const lead = computed(() => {
  switch (props.boot.mode) {
    case 'updateOffer':
      return props.boot.running
        ? 'The running copy finishes any sale in progress, closes, and the new version starts in its place. Your settings carry over; a backup of each file is kept beside it.'
        : 'The installed copy is replaced and the new version starts. Your settings carry over; a backup of each file is kept beside it.'
    case 'replaceUnknown':
      return `${props.boot.detail ?? 'The installed version cannot be determined'}. Replacing it installs ${props.boot.current} in full.`
    case 'newerInstalled':
      return `Ledger POS ${props.boot.installed} is installed — newer than this file. Nothing will be changed.`
    default:
      return ''
  }
})

const result = computed(() => {
  const o = outcome.value
  if (!o) return null
  switch (o.kind) {
    case 'started':
      return { tone: 'text-brass-300', text: 'The new version is running. This window closes now.' }
    case 'runningNotStopped':
      return { tone: 'text-warn', text: 'The new version is installed, but the running copy did not close. It keeps running the old version until it is restarted.' }
    case 'launchFailed':
      return { tone: 'text-danger', text: `The new version was installed but did not start: ${o.reason}.` }
    case 'installFailed':
      return { tone: 'text-danger', text: `Nothing was changed: ${o.reason}.` }
  }
  return null
})

function dot(state?: StepState) {
  switch (state) {
    case 'done':
      return 'bg-brass-500'
    case 'running':
      return 'bg-paper animate-pulse'
    case 'waiting':
      return 'bg-warn animate-pulse'
    case 'failed':
      return 'bg-danger'
    default:
      return 'bg-paper-mute/40'
  }
}

watch(outcome, (o) => {
  if (o?.kind === 'started') setTimeout(() => quit(), 1500)
})
</script>
