import { spawnSync, execFileSync } from 'node:child_process'
import { mkdirSync, copyFileSync, linkSync, rmSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import path from 'node:path'
import { prepareV8Artifacts } from './runtime-artifacts.mjs'

const root = fileURLToPath(new URL('../', import.meta.url))
const release = process.argv.includes('--release')
const target = process.env.CARGO_BUILD_TARGET || execFileSync('rustc', ['-vV'], {encoding:'utf8'}).match(/^host: (.+)$/m)?.[1]
if (!target) throw new Error('Could not determine the Rust target')
console.log(`Preparing checksum-verified V8 artifacts for ${target}`)
const artifacts = await prepareV8Artifacts({root, target})
const targetDir = path.join(root, 'runtime/target')
const args = ['+stable', 'build', '--manifest-path', path.join(root, 'runtime/codex/codex-rs/Cargo.toml'), '-p', 'codex-app-server', '--bin', 'brr-runtime', '-p', 'codex-code-mode-host', '--bin', 'codex-code-mode-host', '--features', 'codex-app-server/brr-runtime', '--target-dir', targetDir, '--target', target]
if (release) args.push('--release')
const build = spawnSync('cargo', args, {cwd:root, stdio:'inherit', env:{...process.env, ...artifacts, CARGO_INCREMENTAL:'0', CARGO_PROFILE_DEV_DEBUG:'0', CARGO_NET_GIT_FETCH_WITH_CLI:'true'}})
if (build.status !== 0) process.exit(build.status ?? 1)
const extension = target.includes('windows') ? '.exe' : ''
const binDir = path.join(root, 'src-tauri/binaries')
mkdirSync(binDir, {recursive:true})
for (const name of ['brr-runtime', 'codex-code-mode-host']) {
  const binary = path.join(targetDir, target, release ? 'release' : 'debug', `${name}${extension}`)
  const staged = path.join(binDir, `${name}-${target}${extension}`)
  copyFileSync(binary, staged)
  const development = path.join(binDir, `${name}${extension}`)
  rmSync(development, {force:true})
  linkSync(staged, development)
}
console.log(`Built Brr runtime for ${target} (${release ? 'release' : 'debug'})`)
