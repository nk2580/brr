import assert from 'node:assert/strict'
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { selectReleaseFiles } from './prepare-source-release.mjs'

test('ships the native WorkTrunk source dependency and submodule provenance without Git metadata', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'brr-native-worktrunk-source-'))
  const required = ['.gitmodules', 'rust-toolchain.toml', 'vendor/WORKTRUNK.md', 'vendor/worktrunk/Cargo.toml', 'vendor/worktrunk/LICENSE', 'vendor/worktrunk/src/lib.rs', 'vendor/worktrunk/src/embedded.rs']
  try {
    for (const file of [...required, 'vendor/worktrunk/.git', 'vendor/worktrunk/target/debug/wt']) {
      const destination = path.join(root, file)
      await mkdir(path.dirname(destination), { recursive: true })
      await writeFile(destination, 'fixture')
    }
    const selected = await selectReleaseFiles(root)
    for (const file of required) assert.ok(selected.includes(file), `source release must include ${file}`)
    assert.ok(!selected.includes('vendor/worktrunk/.git'))
    assert.ok(!selected.includes('vendor/worktrunk/target/debug/wt'))
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})
