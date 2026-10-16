/**
 * Patch Tauri's linuxdeploy GTK plugin before the AppImage is bundled. Two fixes:
 *
 * 1. **Never ship libwayland** (always). The AppImage is built on an old distribution so it
 *    runs on more of them, and linuxdeploy copies that distribution's `libwayland-*` in. But
 *    the GPU driver is never bundled — EGL comes from the host's Mesa — and a current Mesa
 *    needs symbols an old libwayland does not have (`wl_fixes_interface`,
 *    `wl_display_dispatch_queue_timeout`, `wl_display_create_queue_with_name`, Wayland 1.23).
 *    With the bundled copy loaded first, `libEGL_mesa.so.0` fails to load, WebKit's web
 *    process aborts with `Could not create default EGL display: EGL_BAD_PARAMETER`, and the
 *    window stays blank — on Arch, Manjaro, Fedora 41+ and every other distribution with a
 *    new Mesa, on Intel/AMD graphics. Every system that can run this AppImage has its own
 *    libwayland (GTK 3 itself links it), and libwayland is backwards compatible, so the host's
 *    copy is always the right one. The plugin is the last thing that deploys libraries before
 *    the AppImage is packed, so the removal goes at its end.
 *
 * 2. **Tolerate a missing gdk-pixbuf module directory** (only where it is missing).
 *    gdk-pixbuf 2.43+ compiles its loaders into the library and stopped installing
 *    `/usr/lib/gdk-pixbuf-2.0/2.10.0`, but its `.pc` file still advertises the path and the
 *    plugin copies it unconditionally — so bundling dies with `cp: cannot stat`. Nothing is
 *    lost by skipping that copy: the loaders are inside `libgdk_pixbuf-2.0.so`.
 *
 * Runs as `beforeBundleCommand`. Tauri downloads the plugin only when it is not cached, so
 * this fetches it first and patches the cached copy. It never fails the build; when it
 * cannot patch, it says so, and `scripts/check-appimage.mjs` catches an AppImage that still
 * carries libwayland.
 */
import { existsSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { homedir, platform } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const PLUGIN_URL =
  'https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh'
// Where the bundler looks: dirs::cache_dir(), i.e. $XDG_CACHE_HOME or ~/.cache.
const CACHE = join(process.env.XDG_CACHE_HOME || join(homedir(), '.cache'), 'tauri')
const PLUGIN = join(CACHE, 'linuxdeploy-plugin-gtk.sh')

export const PIXBUF_MARKER = '# patched: tolerate a missing gdk-pixbuf module directory'
export const WAYLAND_MARKER = "# patched: use the host's libwayland (scripts/prepare-appimage-plugin.mjs)"

const COPY = 'copy_tree "$gdk_pixbuf_binarydir" "$APPDIR/"'
const TAIL = 'sed -i "s|$gdk_pixbuf_moduledir/||g" "$APPDIR/$gdk_pixbuf_cache_file"'

/** The plugin with both fixes applied; applying it again changes nothing. */
export function patchGtkPlugin(script, { pixbufMissing }) {
  let out = script
  if (pixbufMissing && !out.includes(PIXBUF_MARKER) && out.includes(COPY) && out.includes(TAIL)) {
    const start = out.indexOf(COPY)
    const end = out.indexOf(TAIL) + TAIL.length
    out =
      out.slice(0, start) +
      `${PIXBUF_MARKER}\nif [ -d "$gdk_pixbuf_binarydir" ]; then\n` +
      out.slice(start, end) +
      '\nelse\n    echo "Skipping GDK PixBuf modules: $gdk_pixbuf_binarydir does not exist"\nfi' +
      out.slice(end)
  }
  if (!out.includes(WAYLAND_MARKER)) {
    out =
      out.replace(/\n*$/, '\n') +
      `\n${WAYLAND_MARKER}\n` +
      'echo "Removing the bundled libwayland: the host\'s copy matches its Mesa EGL"\n' +
      'rm -f "$APPDIR"/usr/lib*/libwayland-*.so* "$APPDIR"/usr/lib*/*/libwayland-*.so*\n'
  }
  return out
}

function pixbufDirMissing() {
  try {
    const dir = execFileSync('pkg-config', ['--variable=gdk_pixbuf_binarydir', 'gdk-pixbuf-2.0'], {
      encoding: 'utf8',
    }).trim()
    return Boolean(dir) && !existsSync(dir)
  } catch {
    return false
  }
}

async function main() {
  if (platform() !== 'linux') {
    console.log('prepare-appimage-plugin: nothing to do (not Linux)')
    return
  }
  if (!existsSync(PLUGIN)) {
    mkdirSync(CACHE, { recursive: true })
    try {
      const res = await fetch(PLUGIN_URL)
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      writeFileSync(PLUGIN, await res.text(), { mode: 0o755 })
      console.log('prepare-appimage-plugin: fetched the GTK plugin')
    } catch (e) {
      console.warn(
        `prepare-appimage-plugin: WARNING could not fetch the plugin (${e.message}); the AppImage ` +
          'will carry libwayland and show a blank window on hosts with a new Mesa',
      )
      return
    }
  }
  const script = readFileSync(PLUGIN, 'utf8')
  const patched = patchGtkPlugin(script, { pixbufMissing: pixbufDirMissing() })
  if (patched === script) {
    console.log('prepare-appimage-plugin: already patched')
    return
  }
  writeFileSync(PLUGIN, patched, { mode: 0o755 })
  console.log(`prepare-appimage-plugin: patched ${PLUGIN}`)
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main()
