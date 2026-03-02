/**
 * Sync version from tauri.conf.json5 (source of truth) to Cargo.toml.
 * Runs automatically via beforeBuildCommand / beforeDevCommand.
 */
import { readFileSync, writeFileSync } from 'node:fs'
import JSON5 from 'json5'

const tauriConf = JSON5.parse(readFileSync('./src-tauri/tauri.conf.json5', 'utf-8'))
const version: string = tauriConf.version

if (!version) {
  console.error('No version found in tauri.conf.json5')
  process.exit(1)
}

// Update Cargo.toml
const cargoPath = './src-tauri/Cargo.toml'
const cargo = readFileSync(cargoPath, 'utf-8')
const updated = cargo.replace(/^version\s*=\s*".*"/m, `version = "${version}"`)
if (cargo !== updated) {
  writeFileSync(cargoPath, updated)
  console.log(`Synced version ${version} → Cargo.toml`)
} else {
  console.log(`Version ${version} already in sync`)
}
