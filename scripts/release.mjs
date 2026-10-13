#!/usr/bin/env node
/**
 * Version a Ledger POS release (spec §1).
 *
 *   node scripts/release.mjs 1.0.0               bump every mirror, cut the CHANGELOG section
 *   node scripts/release.mjs --check             the mirrors agree; tauri.conf.json has no version
 *   node scripts/release.mjs --check-tag v1.0.0  … and the tag and a non-empty section match
 *   node scripts/release.mjs --notes 1.0.0       print that CHANGELOG section (the release text)
 *
 * `src-tauri/Cargo.toml` is canonical — the binary and Tauri both read it. `package.json`
 * and the `payment-pos-app` entry of `Cargo.lock` are mirrors this script keeps equal.
 * A bump is all-or-nothing: every new file is computed first, and if any step fails the
 * four files get their original bytes back. It never commits or tags.
 */
import { execFileSync } from 'node:child_process'
import { readFileSync, renameSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?$/

const FILES = {
  pkg: 'package.json',
  cargo: 'src-tauri/Cargo.toml',
  lock: 'src-tauri/Cargo.lock',
  tauri: 'src-tauri/tauri.conf.json',
  changelog: 'CHANGELOG.md',
}

export function parseVersion(v) {
  const m = SEMVER.exec(String(v))
  if (!m) return null
  return { major: +m[1], minor: +m[2], patch: +m[3], pre: m[4] ? m[4].split('.') : [] }
}

/** Semver precedence: a pre-release sorts below its release. */
export function compareVersions(a, b) {
  const x = parseVersion(a)
  const y = parseVersion(b)
  if (!x || !y) throw new Error(`not a version: ${!x ? a : b}`)
  for (const k of ['major', 'minor', 'patch']) if (x[k] !== y[k]) return x[k] - y[k]
  if (!x.pre.length || !y.pre.length) return y.pre.length - x.pre.length
  for (let i = 0; i < Math.max(x.pre.length, y.pre.length); i++) {
    const p = x.pre[i]
    const q = y.pre[i]
    if (p === undefined) return -1
    if (q === undefined) return 1
    const pn = /^\d+$/.test(p)
    const qn = /^\d+$/.test(q)
    if (pn && qn && +p !== +q) return +p - +q
    if (pn !== qn) return pn ? -1 : 1
    if (p !== q) return p < q ? -1 : 1
  }
  return 0
}

const PACKAGE_VERSION = /(\[package\][^[]*?\nversion = ")([^"]+)(")/

export function cargoVersion(toml) {
  const m = PACKAGE_VERSION.exec(toml)
  if (!m) throw new Error('src-tauri/Cargo.toml has no [package] version')
  return m[2]
}

export function setCargoVersion(toml, v) {
  return toml.replace(PACKAGE_VERSION, `$1${v}$3`)
}

export function lockVersion(lock) {
  return /name = "payment-pos-app"\nversion = "([^"]+)"/.exec(lock)?.[1] ?? null
}

/** The body of `## [<version>] …` up to the next `## [`, or null when there is none. */
function section(text, version) {
  const head = `\n## [${version}]`
  const start = text.indexOf(head)
  if (start < 0) return null
  const bodyStart = text.indexOf('\n', start + 1) + 1
  const next = text.indexOf('\n## [', bodyStart - 1)
  return text.slice(bodyStart, next < 0 ? undefined : next + 1)
}

export function cutChangelog(text, version, date, { allowEmpty = false } = {}) {
  if (section(text, version) !== null) return text // already cut: a re-run is a no-op
  const marker = '## [Unreleased]'
  const at = text.indexOf(marker)
  if (at < 0) throw new Error('CHANGELOG.md has no ## [Unreleased] section')
  const body = section(text, 'Unreleased') ?? ''
  if (!body.trim() && !allowEmpty) throw new Error('the [Unreleased] section is empty — nothing to release')
  const after = text.slice(at + marker.length + 1 + body.length)
  return `${text.slice(0, at)}${marker}\n\n## [${version}] - ${date}\n${body}${after}`
}

export function notes(text, version) {
  const body = section(text, version)
  if (body === null) throw new Error(`CHANGELOG.md has no section for ${version}`)
  return body.trim()
}

function readAll(root) {
  return Object.fromEntries(Object.entries(FILES).map(([k, f]) => [k, readFileSync(join(root, f), 'utf8')]))
}

/** Every reason the tree is not releasable as it stands; empty means fine. */
export function check(root, tag) {
  const f = readAll(root)
  const version = cargoVersion(f.cargo)
  const problems = []
  const pkg = JSON.parse(f.pkg).version
  if (pkg !== version) problems.push(`package.json says ${pkg}, Cargo.toml says ${version}`)
  const lock = lockVersion(f.lock)
  if (lock !== version) problems.push(`Cargo.lock says ${lock}, Cargo.toml says ${version}`)
  if ('version' in JSON.parse(f.tauri)) problems.push('tauri.conf.json carries a version; Tauri must read Cargo.toml')
  if (tag !== undefined) {
    const name = String(tag).replace(/^refs\/tags\//, '')
    if (name !== `v${version}`) problems.push(`tag ${name} does not match version ${version} (want v${version})`)
    const body = section(f.changelog, version)
    if (!body || !body.trim()) problems.push(`CHANGELOG.md has no non-empty section for ${version}`)
  }
  return problems
}

function writeAtomic(path, text) {
  const tmp = `${path}.release-tmp`
  writeFileSync(tmp, text)
  renameSync(tmp, path)
}

export function bump(root, version, { allowDirty = false, allowEmpty = false, cargo, date } = {}) {
  if (!parseVersion(version)) throw new Error(`not a semver version: ${version}`)
  const original = readAll(root)
  const current = cargoVersion(original.cargo)
  if (compareVersions(version, current) < 0) throw new Error(`${version} is lower than the current ${current}`)
  if (!allowDirty) {
    const dirty = execFileSync('git', ['-C', root, 'status', '--porcelain'], { encoding: 'utf8' }).trim()
    if (dirty) throw new Error(`the tree has uncommitted changes:\n${dirty}\n(--allow-dirty to proceed anyway)`)
  }
  const day = date ?? process.env.RELEASE_DATE ?? new Date().toISOString().slice(0, 10)
  const pkg = JSON.parse(original.pkg)
  pkg.version = version
  const conf = JSON.parse(original.tauri)
  delete conf.version
  const next = {
    pkg: JSON.stringify(pkg, null, 2) + '\n',
    cargo: setCargoVersion(original.cargo, version),
    tauri: JSON.stringify(conf, null, 2) + '\n',
    changelog: cutChangelog(original.changelog, version, day, { allowEmpty }),
  }
  const restore = () => {
    for (const [k, f] of Object.entries(FILES)) writeAtomic(join(root, f), original[k])
  }
  try {
    for (const k of Object.keys(next)) writeAtomic(join(root, FILES[k]), next[k])
    const [cmd, ...pre] = (cargo ?? process.env.RELEASE_CARGO ?? 'cargo').split(' ')
    try {
      execFileSync(cmd, [...pre, 'update', '--workspace', '--offline', '--manifest-path', join(root, FILES.cargo)], {
        stdio: 'inherit',
      })
    } catch (e) {
      throw new Error(`cargo update failed (${e.message})`)
    }
    const problems = check(root)
    if (problems.length) throw new Error(problems.join('\n'))
  } catch (e) {
    restore()
    throw e
  }
}

function main(argv) {
  const flag = (name) => argv.includes(name)
  const value = (name) => (argv.includes(name) ? argv[argv.indexOf(name) + 1] : undefined)
  const root = resolve(value('--root') ?? join(dirname(fileURLToPath(import.meta.url)), '..'))
  if (flag('--check') || flag('--check-tag')) {
    const problems = check(root, value('--check-tag'))
    for (const p of problems) console.error(`✗ ${p}`)
    if (!problems.length) console.log(`✓ version ${cargoVersion(readAll(root).cargo)} is consistent`)
    return problems.length ? 1 : 0
  }
  if (flag('--notes')) {
    console.log(notes(readAll(root).changelog, value('--notes')))
    return 0
  }
  const version = argv.find((a) => parseVersion(a))
  if (!version) {
    console.error('usage: release.mjs <x.y.z> [--allow-dirty] [--allow-empty] | --check | --check-tag <ref> | --notes <x.y.z>')
    return 2
  }
  bump(root, version, { allowDirty: flag('--allow-dirty'), allowEmpty: flag('--allow-empty') })
  console.log(`Version ${version} set in package.json, Cargo.toml and Cargo.lock; CHANGELOG cut.

Next: commit these files as "Release ${version}", merge to main, then
  git tag v${version} && git push origin main v${version}`)
  return 0
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exit(main(process.argv.slice(2)))
  } catch (e) {
    console.error(`release: ${e.message}`)
    process.exit(1)
  }
}
