import { test } from 'node:test'
import assert from 'node:assert/strict'
import { patchGtkPlugin, WAYLAND_MARKER, PIXBUF_MARKER } from './prepare-appimage-plugin.mjs'
import { conflictingLibraries } from './check-appimage.mjs'

const COPY = 'copy_tree "$gdk_pixbuf_binarydir" "$APPDIR/"'
const TAIL = 'sed -i "s|$gdk_pixbuf_moduledir/||g" "$APPDIR/$gdk_pixbuf_cache_file"'
const PLUGIN = `#! /usr/bin/env bash\nset -e\n${COPY}\n${TAIL}\nfind "$APPDIR"/usr/lib* -name 'libwebkit*' -exec sed -i -e "s|/usr|././|g" '{}' \\;\n`

test('the plugin always drops the bundled libwayland, after everything else it deploys', () => {
  const out = patchGtkPlugin(PLUGIN, { pixbufMissing: false })
  assert.ok(out.includes(WAYLAND_MARKER))
  assert.match(out, /rm -f[^\n]*"\$APPDIR"\/usr\/lib\*\/libwayland-\*\.so\*/)
  assert.ok(out.indexOf(WAYLAND_MARKER) > out.indexOf('libwebkit*'), 'appended at the end')
  assert.ok(!out.includes(PIXBUF_MARKER), 'the pixbuf guard only where the directory is gone')
})

test('patching twice changes nothing more', () => {
  const once = patchGtkPlugin(PLUGIN, { pixbufMissing: true })
  assert.equal(patchGtkPlugin(once, { pixbufMissing: true }), once)
  assert.ok(once.includes(PIXBUF_MARKER) && once.includes(WAYLAND_MARKER))
})

test('a plugin without the pixbuf block still gets the libwayland removal', () => {
  const out = patchGtkPlugin('#! /usr/bin/env bash\necho hi\n', { pixbufMissing: true })
  assert.ok(out.includes(WAYLAND_MARKER))
  assert.ok(!out.includes(PIXBUF_MARKER))
})

test('libwayland anywhere under usr/lib is a conflict; everything else is not', () => {
  assert.deepEqual(
    conflictingLibraries([
      'usr/lib/libwayland-client.so.0',
      'usr/lib/x86_64-linux-gnu/libwayland-egl.so.1',
      'usr/lib/libwayland-server.so.0.22.0',
      'usr/lib/libgtk-3.so.0',
      'usr/lib/gtk-3.0/3.0.0/immodules/im-wayland.so',
      'usr/share/doc/libwayland-client0/copyright',
    ]),
    ['usr/lib/libwayland-client.so.0', 'usr/lib/x86_64-linux-gnu/libwayland-egl.so.1', 'usr/lib/libwayland-server.so.0.22.0'],
  )
})
