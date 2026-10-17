<template>
  <div ref="root">
    <PageHeader
      kicker="Guide · 06"
      title="Everything it does, in one place."
      lede="How to connect a terminal, take payments from your till, keep the bridge running, protect it, update it and fix it when something is off. Nothing here needs the README."
    />

    <div class="grid items-start gap-8 lg:grid-cols-[200px_1fr]">
      <!-- contents rail -->
      <nav class="space-y-3 lg:sticky lg:top-8" aria-label="Guide contents" data-reveal>
        <input v-model="query" type="search" class="field !py-2 text-xs" placeholder="Search the guide…" aria-label="Search the guide" />
        <ol class="space-y-0.5">
          <li v-for="s in SECTIONS" :key="s.id">
            <a
              :href="`#${s.id}`"
              class="block rounded-lg px-2.5 py-1.5 text-xs transition-colors"
              :class="[visible[s.id] === false ? 'pointer-events-none text-paper-mute/40' : 'text-paper-dim hover:bg-white/4 hover:text-paper', current === s.id && 'toc-active']"
            >
              {{ s.title }}
            </a>
          </li>
        </ol>
      </nav>

      <div ref="body" class="min-w-0 space-y-6">
        <p v-if="query && !anyVisible" class="glass p-6 text-sm text-paper-mute">Nothing in the guide mentions “{{ query }}”.</p>

        <!-- ------------------------------------------------------------ start -->
        <GlassCard id="start" v-show="visible.start !== false" class="guide scroll-mt-8" data-section>
          <h2>Getting started</h2>
          <p>
            Ledger POS turns a payment terminal into a <strong>local REST API</strong>. Your till, web shop or script asks for a
            payment, the customer taps their card, and the receipt comes back as JSON. This window is for setting it up and
            watching it; the API keeps serving from the system tray when the window is closed.
          </p>
          <ol>
            <li><strong>Try it on the sandbox.</strong> It starts on a built-in simulated terminal, so <NuxtLink to="/operations?provider=sandbox&fn=purchase">a test sale</NuxtLink> works before any hardware is involved.</li>
            <li><strong>Connect your terminal</strong> on <NuxtLink to="/providers">Providers</NuxtLink> and make it active (<a href="#terminal">how</a>).</li>
            <li><strong>Point your till at the API</strong> — <code>{{ base }}</code> (<a href="#api">examples</a>).</li>
            <li><strong>Optionally protect it</strong> with a console password, webhooks and signed receipts (<a href="#security">what each does</a>).</li>
          </ol>
          <p>The <NuxtLink to="/">Overview</NuxtLink> keeps a checklist of these steps that ticks itself as you go.</p>
        </GlassCard>

        <!-- --------------------------------------------------------- terminal -->
        <GlassCard id="terminal" v-show="visible.terminal !== false" class="guide scroll-mt-8" data-section>
          <h2>Connecting a terminal</h2>
          <p>Ledger POS drives the <strong>Saman SSP1126</strong>; the built-in <strong>sandbox</strong> simulates the same operations.</p>
          <ol>
            <li>Open <NuxtLink to="/providers/saman-ssp1126">Providers → Saman SSP1126</NuxtLink>.</li>
            <li>Set <code>host</code> to the terminal's IP address and <code>port</code> to <code>1197</code>. For a serial cable, set <code>transport</code> to <code>serial</code> and give the device <code>path</code> (e.g. <code>/dev/ttyUSB0</code> or <code>COM3</code>).</li>
            <li><em>Save configuration</em>, then <em>Make active</em>.</li>
          </ol>
          <div class="callout">
            <strong>After every power-on, insert a card in the terminal once — you can cancel it.</strong> The SSP1126 does not open
            its link to the computer when it boots; it opens it the first time a card wakes its payment application, and keeps it
            open afterwards. Until then it refuses every connection and the app reports <code>terminal_unreachable</code>. Once per
            power-on, not once per sale.
          </div>
          <p>
            The terminal light in the sidebar and on the terminal's page is <span class="text-ok">green</span> while it answers.
            <em>Run connection test</em> on the Overview asks it directly.
          </p>
          <p>
            The link to the terminal is checked for corruption but <strong>not encrypted</strong> — keep it on a network you trust.
          </p>
        </GlassCard>

        <!-- ------------------------------------------------------- operations -->
        <GlassCard id="operations" v-show="visible.operations !== false" class="guide scroll-mt-8" data-section>
          <h2>Running operations by hand</h2>
          <p>
            <NuxtLink to="/operations">Operations</NuxtLink> lists everything the selected terminal can do — purchases, balance
            inquiries, bill payments, reports — with a form built from the same description the API publishes. Use it to try an
            operation before your till does it, or to run the occasional report.
          </p>
          <ul>
            <li>Operations marked <span class="text-warn">slow</span> wait for the cardholder — up to about two minutes.</li>
            <li>Only one transaction runs per terminal at a time; a second one answers <code>409 busy</code>.</li>
            <li>Every call — from here or over the API — appears under <em>Recent activity</em> on the Overview.</li>
            <li>With a console password set, running operations by hand needs the console unlocked; your till's calls over the API do not.</li>
          </ul>
        </GlassCard>

        <!-- -------------------------------------------------------------- api -->
        <GlassCard id="api" v-show="visible.api !== false" class="guide scroll-mt-8" data-section>
          <h2>Using the API</h2>
          <p>
            Base URL <code>{{ base }}</code>. Every endpoint is described, with a <em>Try it out</em> button, in the
            <button type="button" class="link" @click="openApiDocs()">interactive API docs</button>.
          </p>
          <p>Take a payment on whichever terminal is active:</p>
          <CommandBox :command="purchaseExample" />
          <p>Configure a terminal without opening this window:</p>
          <CommandBox :command="configExample" />
          <div class="callout">
            <strong>Use generous timeouts</strong> — card operations block until the cardholder acts, up to about two minutes.
            Only one transaction runs per terminal at a time.
          </div>

          <h3>Endpoints</h3>
          <table>
            <tbody>
              <tr v-for="e in ENDPOINTS" :key="e[0] + e[1]">
                <td class="whitespace-nowrap font-mono text-paper-mute">{{ e[0] }}</td>
                <td class="font-mono text-paper">{{ e[1] }}</td>
                <td>{{ e[2] }}</td>
              </tr>
            </tbody>
          </table>

          <h3>Errors</h3>
          <p>Failures answer <code>{ "code": "…", "error": "…" }</code>:</p>
          <table>
            <tbody>
              <tr v-for="e in ERRORS" :key="e[1]">
                <td class="whitespace-nowrap font-mono text-paper-mute">{{ e[0] }}</td>
                <td class="font-mono text-paper">{{ e[1] }}</td>
                <td>{{ e[2] }}</td>
              </tr>
            </tbody>
          </table>
          <p>
            A successful reply on the active terminal may also carry <code>attestation</code> — a
            <a href="#security">signed receipt</a>. It is added only when an administrator turned receipts on, and changes
            nothing an existing integration reads.
          </p>
        </GlassCard>

        <!-- ------------------------------------------------------- background -->
        <GlassCard id="background" v-show="visible.background !== false" class="guide scroll-mt-8" data-section>
          <h2>Running in the background</h2>
          <p>
            The API is served by <code>posd</code>, a background process with no window, and by nothing else. This app is only
            its console: it installs <code>posd</code> as a service, starts it, restarts it if it stops and moves it onto each new
            version — there is nothing to set up, and nothing to choose.
          </p>
          <ul>
            <li><strong>From login on</strong> the API answers, whether this window is open or not; on Linux it is the systemd user service <code>posd.service</code>, and it also starts at boot where the system allows it.</li>
            <li><strong>Closing or quitting this window</strong> never stops the API. <em>Minimize to tray on close</em> and <em>Open in the tray at login</em> in <NuxtLink to="/settings">Settings</NuxtLink> are about the console only.</li>
            <li><strong>Updates</strong> let the payment in progress finish, then restart the API on the new version by themselves.</li>
            <li>On Linux the tray icon needs <code>libappindicator</code>; without it, closing the window leaves no way back to the console (the API keeps serving).</li>
          </ul>
          <h3>A machine without a desktop</h3>
          <p>One command installs the same service, for a server or a till with no screen:</p>
          <CommandBox command="./Ledger*.AppImage --install" />
        </GlassCard>

        <!-- --------------------------------------------------------- settings -->
        <GlassCard id="settings" v-show="visible.settings !== false" class="guide scroll-mt-8" data-section>
          <h2>Settings and data files</h2>
          <p>
            Everything on <NuxtLink to="/settings">Settings</NuxtLink> and each terminal's configuration is kept in
            <code>{{ settingsFile || 'settings.json' }}</code> — editable here, over the API (<code>PUT /api/v1/settings</code>) or by hand.
            Beside it:
          </p>
          <ul>
            <li><code>events-outbox.json</code> — webhook messages not delivered yet.</li>
            <li><code>state.json</code> — the last version that ran.</li>
            <li><code>admin.json</code> — the console password, auto-lock, webhooks and receipt key; kept elsewhere (<code>/etc/ledger-pos</code>, <code>%ProgramData%\ledger-pos</code> on Windows) and written only by the administrator.</li>
          </ul>
          <p>
            Changing the API host or port rebinds at once. If the new address cannot be used, it falls back to the last working
            one and says why on the Overview — you cannot lock yourself out. Keep <code>127.0.0.1</code> unless other machines
            must reach the API.
          </p>
        </GlassCard>

        <!-- --------------------------------------------------------- security -->
        <GlassCard id="security" v-show="visible.security !== false" class="guide scroll-mt-8" data-section>
          <h2>Locking, webhooks and signed receipts</h2>
          <p>
            All three are optional, off until the computer's administrator turns them on, and managed on
            <NuxtLink to="/security">Security</NuxtLink>. They live in an administrator-only file, so the buttons there make your
            system ask for the <strong>administrator's password</strong> (the one for <code>sudo</code>) — someone who only knows the
            console password cannot switch them off. The console only hands the administrator a <code>posd</code> that root
            alone can change — after an AppImage install, make that copy once:
          </p>
          <CommandBox command="sudo install -o root -g root -m 755 ~/.local/bin/posd /usr/local/bin/posd" />
          <dl>
            <dt>Console password</dt>
            <dd>Anyone can look; changing settings, terminals or the active terminal, and running operations by hand, needs it. Payments over the API and every read keep working, so the till keeps trading while nobody at the counter can repoint the terminal. It re-locks after the auto-lock idle time and on restart; wrong passwords are answered more slowly after the third try. Unlock with the padlock beside the logo.</dd>
            <dt>Webhook events</dt>
            <dd>Tell a server elsewhere when this machine changes — the lock opened, the password removed, settings changed, the terminal repointed. Messages follow Standard Webhooks (<code>webhook-id</code>, <code>webhook-timestamp</code>, <code>webhook-signature</code>), so any server verifies them with an existing library. Undelivered messages are retried for 72 hours.</dd>
            <dt>Signed receipts</dt>
            <dd>Every successful operation on the active terminal gains <code>attestation</code>: a JWS (HS256) holding the request, the reply and the machine's posture from before the operation, plus <code>postureChanged</code>, <code>iat</code> and a unique <code>jti</code>. A receiver should verify it, trust its claims rather than the JSON around it, want <code>postureChanged: false</code> and <code>provider</code> equal to <code>posture.activeProvider</code>, and store <code>jti</code> so one receipt pays for one sale.</dd>
          </dl>

          <h3>Events a webhook can receive</h3>
          <table>
            <tbody>
              <tr v-for="(info, type) in EVENT_INFO" :key="type">
                <td class="whitespace-nowrap font-mono text-paper">{{ type }}</td>
                <td>{{ info.when }}</td>
              </tr>
            </tbody>
          </table>

          <h3>From a terminal instead</h3>
          <p>The same changes, for scripts or a machine without a desktop:</p>
          <CommandBox :command="ADMIN_COMMANDS" />
          <p v-if="installInfo.installed">
            After an AppImage install, give the full path — <code>sudo</code> does not look in <code>~/.local/bin</code>:
            <code>sudo ~/.local/bin/posd admin status</code>. On Windows, run them from an Administrator command prompt, without <code>sudo</code>.
          </p>
          <p v-else>
            After an AppImage install, <code>sudo</code> needs the full path (<code>sudo ~/.local/bin/posd …</code>). On Windows, run
            them from an Administrator command prompt, without <code>sudo</code>.
          </p>

          <div class="callout">
            <strong>What this proves, and what it cannot.</strong> The console lock is an operational control, not a security
            boundary: payments stay open on purpose, and the settings file belongs to the account the app runs as. The webhook
            secrets and receipt key are as private as the administrator file, which by default every account can read
            (<NuxtLink to="/security#device">Security</NuxtLink> warns when it is). Run the background service as its own user
            (<code>User=ledgerpos</code>), then <code>chgrp ledgerpos</code> and <code>chmod 640</code> the file. A stopped app or a
            cut network sends nothing: the absence of an event is not evidence that nothing changed.
          </div>
        </GlassCard>

        <!-- --------------------------------------------------------- updating -->
        <GlassCard id="updating" v-show="visible.updating !== false" class="guide scroll-mt-8" data-section>
          <h2>Installing and updating</h2>
          <h3>AppImage (any Linux)</h3>
          <p>
            Make the file executable and open it. The first time, it installs itself — it copies itself to
            <code>~/.local/bin/ledger-pos.AppImage</code>, adds the menu entry and icons, puts <code>posd</code> beside it and starts
            the background API, all in your home folder, no password. <NuxtLink to="/settings">Settings → Desktop integration</NuxtLink>
            removes it; removing takes back exactly those files and the service, and leaves your settings alone.
          </p>
          <p>
            <strong>To update</strong>, open the newer AppImage: it offers <em>Update and restart</em>, lets the running copy
            finish any sale in progress, and the new version starts in its place. The previous version is kept for one rollback
            (<em>Roll back</em> in Settings). Opening an older file never replaces a newer install.
          </p>
          <p>The same from a terminal, for setting up several machines:</p>
          <CommandBox :command="APPIMAGE_FLAGS" />
          <h3>Packages</h3>
          <p>
            <code>.deb</code>, <code>.rpm</code> and the Windows <code>.msi</code> put the app and <code>posd</code> in place and on the
            menu; the first start of the app sets up the background API. Install a newer package over the old one to update — the
            next start of the app moves the API onto it.
          </p>
          <h3>Your data across versions</h3>
          <p>
            Settings carry over. On the first start after an update each data file is brought up to date and the original kept as
            <code>&lt;file&gt;.v&lt;N&gt;.bak</code>; the app says once what changed. A file written by a <em>newer</em> version is never
            overwritten — settings turn read-only — so going back a version loses nothing. One exception: a version from before 1.0
            cannot read the 1.0 webhook outbox and sets it aside, so deliveries still queued at that moment are not sent.
          </p>
        </GlassCard>

        <!-- -------------------------------------------------- troubleshooting -->
        <GlassCard id="troubleshooting" v-show="visible.troubleshooting !== false" class="guide scroll-mt-8" data-section>
          <h2>Troubleshooting</h2>
          <table>
            <tbody>
              <tr v-for="t in TROUBLE" :key="t[0]">
                <td class="w-2/5 text-paper">{{ t[0] }}</td>
                <td>{{ t[1] }}</td>
              </tr>
            </tbody>
          </table>
        </GlassCard>

        <!-- ------------------------------------------------------- safety -->
        <GlassCard id="safety" v-show="visible.safety !== false" class="guide scroll-mt-8" data-section>
          <h2>Security notes</h2>
          <ul>
            <li>The API listens on <code>127.0.0.1</code> and does not authenticate payments, even with the console locked — anything on this computer can drive the terminal. Change the host only if you accept that other machines can too.</li>
            <li><em>Allow every origin</em> (CORS) would let any web page you visit call the API from your browser. Leave it off unless you understand that.</li>
            <li>The link to the terminal is checked for corruption but not encrypted. Keep it on a network you trust.</li>
          </ul>
          <p class="text-paper-mute">Ledger POS {{ version }} · MIT License</p>
        </GlassCard>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
const api = useApi()
const base = useApiBase()
const { info: installInfo, refresh: refreshInstall } = useInstall()
const root = ref<HTMLElement>()
const body = ref<HTMLElement>()
useReveal(root)

const SECTIONS = [
  { id: 'start', title: 'Getting started' },
  { id: 'terminal', title: 'Connecting a terminal' },
  { id: 'operations', title: 'Operations by hand' },
  { id: 'api', title: 'Using the API' },
  { id: 'background', title: 'Running in the background' },
  { id: 'settings', title: 'Settings and data files' },
  { id: 'security', title: 'Lock, webhooks, receipts' },
  { id: 'updating', title: 'Installing and updating' },
  { id: 'troubleshooting', title: 'Troubleshooting' },
  { id: 'safety', title: 'Security notes' },
] as const

const ENDPOINTS: [string, string, string][] = [
  ['GET', '/api/v1/health', 'Is it alive'],
  ['GET', '/api/v1/system', 'Version, active terminal, server address, settings file, data-file state'],
  ['GET/PUT', '/api/v1/settings', 'Read / replace all settings'],
  ['GET', '/api/v1/activity', 'Recent operations'],
  ['GET', '/api/v1/providers', 'Installed terminal types'],
  ['GET/PUT', '/api/v1/providers/active', 'Read / switch the active terminal'],
  ['GET', '/api/v1/providers/{id}', 'Everything about one terminal type'],
  ['GET/PUT', '/api/v1/providers/{id}/config', 'Read / change its configuration'],
  ['GET', '/api/v1/providers/{id}/link', 'Is the terminal reachable right now'],
  ['GET', '/api/v1/providers/{id}/functions[/{fn}]', 'Operation catalogue / one operation'],
  ['POST', '/api/v1/providers/{id}/functions/{fn}/invoke', 'Run an operation'],
  ['POST', '/api/v1/providers/active/functions/{fn}/invoke', 'Run it on the active terminal'],
  ['GET', '/api/v1/auth/status', 'Is a password set, are you locked'],
  ['POST', '/api/v1/auth/unlock · /lock', 'Get / give up a token (send it as Authorization: Bearer …)'],
  ['GET', '/api/v1/events', 'Webhooks, receipt signing, delivery state (no secrets)'],
]

const ERRORS: [string, string, string][] = [
  ['400', 'invalid_params', 'A parameter is missing or wrong — the function spec says which'],
  ['401', 'locked · invalid_password', 'A protected change without an unlocked session / a wrong password'],
  ['404', 'unknown_provider · unknown_function', 'No such terminal type or operation'],
  ['409', 'busy', 'Another transaction is running on that terminal — wait for it'],
  ['409', 'settings_read_only', 'Settings were written by a newer version'],
  ['409', 'admin_file_newer', 'The administrator file is from a newer version; the console stays locked'],
  ['412', 'not_configured · no_password_set', 'Configure the terminal first / there is no password to unlock'],
  ['502', 'execution_failed', 'The terminal was reached but the operation failed'],
  ['503', 'terminal_unreachable', 'The terminal is not accepting connections (insert a card once after power-on)'],
  ['503', 'updating', 'Closing for an update — retry in a moment'],
]

const TROUBLE: [string, string][] = [
  ['Terminal light red, terminal_unreachable', 'Insert a card in the terminal once and cancel. Then check its IP address and port.'],
  ['Connection test fails but payments work', 'Some terminals ignore the test message; the app checks a second way, so a failure here is real.'],
  ['409 busy', 'One transaction at a time — wait for the previous one.'],
  ['A transaction occasionally takes ~20 s', 'The terminal sometimes ignores the opening message and the app re-sends it. Raise firstAckAttempts on the terminal\'s page if it still gives up.'],
  ['API offline', 'Another program holds the port, or the background API is still starting. The sidebar says which; change the port in Settings if it is taken.'],
  ['Blank window right after start (Linux)', 'Older AppImages carried a graphics library that clashes with a new Mesa (“Could not create default EGL display”). Use a newer AppImage.'],
  ['Visuals stutter or look broken', 'Turn on Reduced visual effects in Settings → Appearance.'],
  ['Forgot the console password', 'Remove it on Security (needs the computer\'s administrator), or run sudo posd admin clear-password.'],
  ['The Security buttons cannot ask for a password', 'No polkit agent or pkexec on this system: the dialog shows the terminal command to run instead.'],
  ['“…can be changed without the administrator\'s password”', 'After an AppImage install, make a copy of posd only root can change, once (and after each update): sudo install -o root -g root -m 755 ~/.local/bin/posd /usr/local/bin/posd'],
]

const ADMIN_COMMANDS = `sudo posd admin status
sudo posd admin set-password          # asks twice, shows nothing as you type
sudo posd admin clear-password        # forgot it? this is the way back
sudo posd admin auto-lock 15          # idle minutes before re-locking; 0 never
sudo posd admin events add https://example.com/hooks/pos [--events security.*,settings.changed]
sudo posd admin events list | remove <id> | enable <id> | disable <id> | test [<id>]
sudo posd admin receipt-key set [--kid <id>]   # again = rotate
sudo posd admin receipt-key clear`

const APPIMAGE_FLAGS = `./Ledger*.AppImage --install            # --uninstall, --install-status, --version
./Ledger*.AppImage --update             # same as "Update and restart"
~/.local/bin/ledger-pos.AppImage --rollback
./Ledger*.AppImage --install --allow-downgrade   # replace a newer install on purpose`

const purchaseExample = computed(
  () => `curl -X POST ${base.value}/api/v1/providers/active/functions/purchase/invoke \\
  -H 'content-type: application/json' \\
  -d '{"mainAmount":10000,"referenceData":"ORDER-1001"}'`,
)
const configExample = computed(
  () => `curl -X PUT ${base.value}/api/v1/providers/saman-ssp1126/config \\
  -H 'content-type: application/json' \\
  -d '{"transport":"tcp","host":"192.168.1.198","port":1197}'`,
)

const settingsFile = ref('')
const version = ref('')

// ------------------------------------------------------------- search + rail

const query = ref('')
const visible = reactive<Record<string, boolean>>({})
const anyVisible = computed(() => SECTIONS.some((s) => visible[s.id] !== false))
const current = ref<string>('start')

watch(query, (q) => {
  const needle = q.trim().toLowerCase()
  for (const s of SECTIONS) {
    const el = document.getElementById(s.id)
    visible[s.id] = !needle || (el?.textContent ?? '').toLowerCase().includes(needle)
  }
})

let observer: IntersectionObserver | undefined
onMounted(async () => {
  refreshInstall()
  try {
    const sys = await api.system()
    settingsFile.value = sys.settingsFile
    version.value = sys.version
  } catch {
    /* the sidebar reports an unreachable API */
  }
  observer = new IntersectionObserver(
    (entries) => {
      const top = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top)[0]
      if (top) current.value = top.target.id
    },
    { rootMargin: '-10% 0px -70% 0px' },
  )
  body.value?.querySelectorAll('[data-section]').forEach((el) => observer!.observe(el))
  // Arriving with #section: the page transition ran after the browser tried to scroll.
  const hash = location.hash.slice(1)
  if (hash) document.getElementById(hash)?.scrollIntoView()
})
onBeforeUnmount(() => observer?.disconnect())
</script>

<style scoped>
.toc-active {
  color: var(--color-brass-300);
  background: rgb(201 160 85 / 0.08);
}
.guide h2 {
  font-family: var(--font-display);
  font-variation-settings: 'SOFT' 40, 'WONK' 0, 'opsz' 72;
  font-weight: 480;
  letter-spacing: -0.015em;
  font-size: 1.35rem;
  color: var(--color-paper);
  margin-bottom: 0.75rem;
}
.guide h3 {
  margin: 1.25rem 0 0.5rem;
  font-size: 0.6875rem;
  font-weight: 500;
  letter-spacing: 0.18em;
  text-transform: uppercase;
  color: var(--color-paper-mute);
}
.guide p,
.guide li,
.guide dd,
.guide td {
  font-size: 0.8125rem;
  line-height: 1.65;
  color: var(--color-paper-dim);
}
.guide p,
.guide ol,
.guide ul,
.guide dl,
.guide table,
.guide > div {
  margin-bottom: 0.85rem;
}
.guide ol {
  list-style: decimal;
  padding-left: 1.25rem;
}
.guide ul {
  list-style: disc;
  padding-left: 1.25rem;
}
.guide li + li {
  margin-top: 0.3rem;
}
.guide dt {
  margin-top: 0.75rem;
  font-size: 0.8125rem;
  font-weight: 500;
  color: var(--color-paper);
}
.guide strong {
  color: var(--color-paper);
  font-weight: 500;
}
.guide code {
  font-family: var(--font-mono);
  font-size: 0.75rem;
  color: var(--color-paper);
}
.guide a,
.guide .link {
  color: var(--color-brass-400);
  cursor: pointer;
}
.guide a:hover,
.guide .link:hover {
  color: var(--color-brass-300);
}
.guide table {
  width: 100%;
  text-align: left;
}
.guide td {
  padding: 0.4rem 0.75rem 0.4rem 0;
  vertical-align: top;
  border-bottom: 1px solid var(--color-line);
}
.guide tr:last-child td {
  border-bottom: 0;
}
.callout {
  border-radius: 0.75rem;
  border: 1px solid rgb(217 164 65 / 0.25);
  background: rgb(217 164 65 / 0.06);
  padding: 0.75rem 1rem;
  font-size: 0.8125rem;
  line-height: 1.65;
  color: var(--color-paper-dim);
}
</style>
