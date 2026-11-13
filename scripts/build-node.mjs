import { spawnSync, execFileSync } from 'node:child_process'
import { mkdirSync, copyFileSync, linkSync, rmSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

const root = fileURLToPath(new URL('../', import.meta.url))
const release = process.argv.includes('--release')
const target = process.env.CARGO_BUILD_TARGET || execFileSync('rustc', ['-vV'], { encoding:'utf8' }).match(/^host: (.+)$/m)?.[1]
const browserPlatform = { 'aarch64-apple-darwin':'mac-arm64', 'x86_64-unknown-linux-gnu':'linux64' }[target]
if (!browserPlatform) throw new Error(`Unsupported fleet node target: ${target}`)
const targetDir = process.env.CARGO_TARGET_DIR || path.join(root, 'crates/target')
const args = [`+${process.env.BRR_RUST_TOOLCHAIN || 'stable'}`, 'build', '--locked', '--manifest-path', path.join(root,'crates/Cargo.toml'), '-p','brr-node','--features','host,network,fleet','--bins','--target-dir',targetDir]
if(process.env.CARGO_BUILD_TARGET)args.push('--target',target)
if (release) args.push('--release')
const built = spawnSync('cargo', args, {cwd:root,stdio:'inherit',env:{...process.env,CARGO_INCREMENTAL:'0',CARGO_PROFILE_DEV_DEBUG:'0'}})
if (built.status !== 0) process.exit(built.status ?? 1)
const binaries = path.join(root,'src-tauri/binaries')
mkdirSync(binaries,{recursive:true})
for(const name of ['brrd','brrctl']) {
  const staged=path.join(binaries,`${name}-${target}`)
  copyFileSync(path.join(targetDir,...(process.env.CARGO_BUILD_TARGET?[target]:[]),release?'release':'debug',name),staged)
  const development=path.join(binaries,name)
  rmSync(development,{force:true});linkSync(staged,development)
}
const browser = spawnSync('python3',[path.join(root,'packaging/prepare-browser.py'),'--platform',browserPlatform,'--output',path.join(binaries,'browser')],{cwd:root,stdio:'inherit'})
if(browser.status !== 0)process.exit(browser.status ?? 1)
console.log(`Built Brr node for ${target} (${release?'release':'debug'})`)
