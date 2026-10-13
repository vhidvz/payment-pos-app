// Stands in for `cargo update --workspace --offline --manifest-path <toml>` in the tests:
// copies the package version from Cargo.toml into Cargo.lock, or fails when asked to.
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'

if (process.env.FAKE_CARGO_FAIL) {
  console.error('fake cargo: failing on purpose')
  process.exit(101)
}
const manifest = process.argv[process.argv.indexOf('--manifest-path') + 1]
const version = readFileSync(manifest, 'utf8').match(/\[package\][\s\S]*?\nversion = "([^"]+)"/)[1]
const lockPath = join(dirname(manifest), 'Cargo.lock')
const lock = readFileSync(lockPath, 'utf8')
writeFileSync(lockPath, lock.replace(/(name = "payment-pos-app"\nversion = ")[^"]+"/, `$1${version}"`))
