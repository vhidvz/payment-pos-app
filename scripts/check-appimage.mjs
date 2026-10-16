/**
 * Fail when a built AppImage carries a library that must come from the host.
 *
 *   node scripts/check-appimage.mjs [<file.AppImage>…]   # default: every AppImage in the bundle dir
 *
 * Today that is libwayland: a bundled old copy stops the host's Mesa EGL from loading and
 * the window stays blank (see scripts/prepare-appimage-plugin.mjs). The AppImage lists its
 * own contents with `--appimage-extract`, run in a scratch directory and removed afterwards.
 */
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readdirSync, rmSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/** Paths (relative to the AppImage root) of libraries that must not be bundled. */
export function conflictingLibraries(paths) {
  return paths.filter((p) => /^usr\/lib[^/]*\/(?:.*\/)?libwayland-[^/]*\.so(?:\.|$)/.test(p))
}

function walk(dir, root = dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = join(dir, e.name)
    return e.isDirectory() ? walk(p, root) : [relative(root, p)]
  })
}

function contents(appimage) {
  const scratch = mkdtempSync(join(tmpdir(), 'check-appimage-'))
  try {
    execFileSync(resolve(appimage), ['--appimage-extract'], { cwd: scratch, stdio: 'ignore' })
    return walk(join(scratch, 'squashfs-root'))
  } finally {
    rmSync(scratch, { recursive: true, force: true })
  }
}

function main(args) {
  const here = dirname(fileURLToPath(import.meta.url))
  const bundleDir = join(here, '..', 'src-tauri/target/release/bundle/appimage')
  const files = args.length
    ? args
    : existsSync(bundleDir)
      ? readdirSync(bundleDir).filter((f) => f.endsWith('.AppImage')).map((f) => join(bundleDir, f))
      : []
  if (!files.length) {
    console.error('check-appimage: no AppImage to check')
    return 1
  }
  let failed = 0
  for (const file of files) {
    const bad = conflictingLibraries(contents(file))
    if (bad.length) {
      failed++
      console.error(`check-appimage: ${file} bundles libraries that must come from the host:\n  ${bad.join('\n  ')}`)
    } else {
      console.log(`check-appimage: ${file} — ok`)
    }
  }
  return failed ? 1 : 0
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = main(process.argv.slice(2))
}
