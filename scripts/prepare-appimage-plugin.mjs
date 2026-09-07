/**
 * Make the AppImage bundler work on distributions that no longer ship a
 * gdk-pixbuf module directory.
 *
 * gdk-pixbuf 2.43+ compiles its loaders into the library and stopped installing
 * `/usr/lib/gdk-pixbuf-2.0/2.10.0`, but its `.pc` file still advertises the path
 * and Tauri's linuxdeploy GTK plugin copies it unconditionally — so bundling
 * dies with `cp: cannot stat`. Nothing is lost by skipping that copy: the
 * loaders are inside `libgdk_pixbuf-2.0.so`, which the bundler already ships.
 *
 * This runs as `beforeBundleCommand`. It is a no-op unless it is needed, and it
 * never fails the build: a bundle that would have worked anyway still works.
 */
import { existsSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { homedir, platform } from 'node:os'
import { join } from 'node:path'

const PLUGIN_URL =
  'https://raw.githubusercontent.com/tauri-apps/linuxdeploy-plugin-gtk/master/linuxdeploy-plugin-gtk.sh'
const CACHE = join(homedir(), '.cache', 'tauri')
const PLUGIN = join(CACHE, 'linuxdeploy-plugin-gtk.sh')
const MARKER = '# patched: tolerate a missing gdk-pixbuf module directory'

const skip = (why) => {
  console.log(`prepare-appimage-plugin: nothing to do (${why})`)
  process.exit(0)
}

if (platform() !== 'linux') skip('not Linux')

let binaryDir
try {
  binaryDir = execFileSync('pkg-config', ['--variable=gdk_pixbuf_binarydir', 'gdk-pixbuf-2.0'], {
    encoding: 'utf8',
  }).trim()
} catch {
  skip('pkg-config could not describe gdk-pixbuf')
}
if (!binaryDir || existsSync(binaryDir)) skip(`${binaryDir || 'path'} exists`)

if (!existsSync(PLUGIN)) {
  mkdirSync(CACHE, { recursive: true })
  try {
    const res = await fetch(PLUGIN_URL)
    if (!res.ok) throw new Error(`HTTP ${res.status}`)
    writeFileSync(PLUGIN, await res.text(), { mode: 0o755 })
    console.log('prepare-appimage-plugin: fetched the GTK plugin')
  } catch (e) {
    skip(`could not fetch the plugin (${e.message}); the bundler will try itself`)
  }
}

const script = readFileSync(PLUGIN, 'utf8')
if (script.includes(MARKER)) skip('already patched')

const COPY = 'copy_tree "$gdk_pixbuf_binarydir" "$APPDIR/"'
const TAIL = 'sed -i "s|$gdk_pixbuf_moduledir/||g" "$APPDIR/$gdk_pixbuf_cache_file"'
if (!script.includes(COPY) || !script.includes(TAIL)) {
  skip('the plugin no longer has the block this patches — leaving it untouched')
}

const start = script.indexOf(COPY)
const end = script.indexOf(TAIL) + TAIL.length
const guarded =
  `${MARKER}\nif [ -d "$gdk_pixbuf_binarydir" ]; then\n` +
  script.slice(start, end) +
  '\nelse\n    echo "Skipping GDK PixBuf modules: $gdk_pixbuf_binarydir does not exist"\nfi'

writeFileSync(PLUGIN, script.slice(0, start) + guarded + script.slice(end), { mode: 0o755 })
console.log(`prepare-appimage-plugin: patched ${PLUGIN} (${binaryDir} is gone on this system)`)
