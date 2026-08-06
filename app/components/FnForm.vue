<template>
  <form class="space-y-5" @submit.prevent="submit">
    <p v-if="!fn.params.length" class="text-sm text-paper-mute">
      This function takes no parameters.
    </p>

    <div v-for="p in fn.params" :key="p.name" class="space-y-1.5">
      <label :for="`fld-${fn.id}-${p.name}`" class="flex items-baseline gap-2">
        <span class="font-mono text-[0.8125rem] text-paper">{{ p.name }}</span>
        <span v-if="p.required" class="text-[0.625rem] font-semibold tracking-[0.14em] text-brass-500 uppercase">required</span>
        <span class="text-[0.6875rem] text-paper-mute">{{ p.type }}</span>
      </label>

      <!-- boolean -->
      <TSwitch
        v-if="p.type === 'boolean'"
        :model-value="Boolean(model[p.name])"
        :label="p.name"
        @update:model-value="model[p.name] = $event"
      />

      <!-- enum -->
      <select
        v-else-if="p.enum?.length"
        :id="`fld-${fn.id}-${p.name}`"
        v-model="model[p.name]"
        class="field"
      >
        <option v-if="!p.required" value="">— none —</option>
        <option v-for="v in p.enum" :key="String(v)" :value="String(v)">{{ v }}</option>
      </select>

      <!-- object arrays (e.g. additionalData) -->
      <textarea
        v-else-if="p.type === 'array' && itemType(p) === 'object'"
        :id="`fld-${fn.id}-${p.name}`"
        v-model.trim="model[p.name] as string"
        rows="3"
        class="field font-mono text-[0.8125rem]"
        :placeholder="p.example ? JSON.stringify(p.example) : '[]'"
        spellcheck="false"
      />

      <!-- string arrays (e.g. filterValues) -->
      <input
        v-else-if="p.type === 'array'"
        :id="`fld-${fn.id}-${p.name}`"
        v-model.trim="model[p.name] as string"
        class="field font-mono"
        :placeholder="stringArrayPlaceholder(p)"
        autocomplete="off"
        spellcheck="false"
      />

      <!-- numbers -->
      <input
        v-else-if="p.type === 'integer' || p.type === 'number'"
        :id="`fld-${fn.id}-${p.name}`"
        v-model.trim="model[p.name] as string"
        class="field font-mono tabular"
        inputmode="numeric"
        :placeholder="p.example !== undefined ? String(p.example) : ''"
        autocomplete="off"
      />

      <!-- strings -->
      <input
        v-else
        :id="`fld-${fn.id}-${p.name}`"
        v-model.trim="model[p.name] as string"
        class="field"
        :placeholder="p.example !== undefined ? String(p.example) : ''"
        autocomplete="off"
        spellcheck="false"
      />

      <p class="text-xs leading-relaxed text-paper-mute">{{ p.description }}</p>
    </div>

    <div class="flex items-center gap-3 pt-1">
      <button type="submit" class="btn-primary" :disabled="busy">
        <svg v-if="busy" class="size-4 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <circle cx="12" cy="12" r="9" stroke="currentColor" stroke-opacity="0.25" stroke-width="3" />
          <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" stroke-width="3" stroke-linecap="round" />
        </svg>
        {{ busy ? busyLabel : 'Invoke' }}
      </button>
      <span v-if="fn.longRunning && !busy" class="text-xs text-paper-mute">
        May wait for the cardholder — up to ~2 minutes.
      </span>
      <span v-if="busy && fn.longRunning" class="text-xs text-warn" role="status">
        Waiting on the terminal…
      </span>
    </div>
  </form>
</template>

<script setup lang="ts">
import type { FunctionSpec, ParamSpec } from '~/composables/api'

const props = defineProps<{ fn: FunctionSpec; busy: boolean }>()
const emit = defineEmits<{ invoke: [params: Record<string, unknown>] }>()

const model = reactive<Record<string, unknown>>({})

watch(
  () => props.fn.id,
  () => {
    for (const k of Object.keys(model)) delete model[k]
    for (const p of props.fn.params) {
      if (p.type === 'boolean') model[p.name] = Boolean(p.default ?? false)
      else if (p.default !== undefined && p.default !== null) model[p.name] = String(p.default)
      else model[p.name] = ''
    }
  },
  { immediate: true },
)

const busyLabel = computed(() => (props.fn.longRunning ? 'Running…' : 'Invoking…'))

function itemType(p: ParamSpec): string {
  return String((p.items as { type?: string } | undefined)?.type ?? 'string')
}

function stringArrayPlaceholder(p: ParamSpec): string {
  if (Array.isArray(p.example)) return (p.example as unknown[]).join(', ')
  return 'comma, separated, values'
}

function submit() {
  const params: Record<string, unknown> = {}
  for (const p of props.fn.params) {
    const raw = model[p.name]
    if (p.type === 'boolean') {
      // only send true — false is the wire default everywhere in this catalog
      if (raw === true) params[p.name] = true
      continue
    }
    const s = typeof raw === 'string' ? raw.trim() : ''
    if (!s) {
      if (p.required) {
        toast('error', `“${p.name}” is required`)
        return
      }
      continue
    }
    if (p.type === 'integer' || p.type === 'number') {
      const n = Number(s)
      if (!Number.isFinite(n)) {
        toast('error', `“${p.name}” must be a number`)
        return
      }
      params[p.name] = n
    } else if (p.type === 'array' && itemType(p) === 'object') {
      try {
        const parsed = JSON.parse(s)
        if (!Array.isArray(parsed)) throw new Error('not an array')
        params[p.name] = parsed
      } catch {
        toast('error', `“${p.name}” must be a JSON array`)
        return
      }
    } else if (p.type === 'array') {
      params[p.name] = s.split(',').map((v) => v.trim()).filter(Boolean)
    } else if (p.enum?.length && typeof p.enum[0] === 'number') {
      params[p.name] = Number(s)
    } else {
      params[p.name] = s
    }
  }
  emit('invoke', params)
}
</script>
