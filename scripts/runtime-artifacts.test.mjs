import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { prepareV8Artifacts } from './runtime-artifacts.mjs'

const checksum = (bytes) => createHash('sha256').update(bytes).digest('hex')

async function fixtureRoot() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'brr-runtime-artifacts-'))
  const checksumPath = path.join(root, 'runtime/codex/third_party/v8/rusty_v8_152_2_0.sha256')
  await (await import('node:fs/promises')).mkdir(path.dirname(checksumPath), { recursive: true })
  return { root, checksumPath }
}

test('downloads pinned artifacts once, verifies cache on every call, and selects target filenames', async (t) => {
  const { root, checksumPath } = await fixtureRoot()
  t.after(() => rm(root, { recursive: true, force: true }))
  const target = 'x86_64-unknown-linux-gnu'
  const archiveName = `librusty_v8_ptrcomp_sandbox_release_${target}.a.gz`
  const bindingName = `src_binding_ptrcomp_sandbox_release_${target}.rs`
  const archiveBytes = Buffer.from('fake gzip archive')
  const bindingBytes = Buffer.from('fake source binding')
  await writeFile(checksumPath, `${checksum(archiveBytes)}  ${archiveName}\n${checksum(bindingBytes)}  ${bindingName}\n`)
  const fetched = []
  const fetchImpl = async (url) => {
    fetched.push(url)
    return new Response(String(url).endsWith(archiveName) ? archiveBytes : bindingBytes)
  }

  const first = await prepareV8Artifacts({ root, target, fetchImpl })
  assert.equal(path.basename(first.RUSTY_V8_ARCHIVE), archiveName)
  assert.equal(path.basename(first.RUSTY_V8_SRC_BINDING_PATH), bindingName)
  assert.match(first.RUSTY_V8_ARCHIVE, /\.cache\/runtime-artifacts\/v152\.2\.0/)
  assert.deepEqual(await readFile(first.RUSTY_V8_ARCHIVE), archiveBytes)
  assert.deepEqual(await readFile(first.RUSTY_V8_SRC_BINDING_PATH), bindingBytes)
  assert.equal(fetched.length, 2)

  await writeFile(first.RUSTY_V8_ARCHIVE, 'corrupt cache')
  const second = await prepareV8Artifacts({ root, target, fetchImpl })
  assert.deepEqual(await readFile(second.RUSTY_V8_ARCHIVE), archiveBytes)
  assert.deepEqual(await readFile(second.RUSTY_V8_SRC_BINDING_PATH), bindingBytes)
  assert.equal(fetched.length, 3, 'only the corrupt cached artifact should be downloaded again')
})

test('rejects a downloaded artifact with the wrong pinned checksum and removes its temp file', async (t) => {
  const { root, checksumPath } = await fixtureRoot()
  t.after(() => rm(root, { recursive: true, force: true }))
  const target = 'aarch64-pc-windows-msvc'
  const archiveName = `rusty_v8_ptrcomp_sandbox_release_${target}.lib.gz`
  const bindingName = `src_binding_ptrcomp_sandbox_release_${target}.rs`
  await writeFile(checksumPath, `${'0'.repeat(64)}  ${archiveName}\n${'1'.repeat(64)}  ${bindingName}\n`)
  const fetched = []

  await assert.rejects(
    prepareV8Artifacts({
      root,
      target,
      fetchImpl: async (url) => {
        fetched.push(url)
        assert.ok(String(url).includes('rusty-v8-v152.2.0/'))
        return new Response('wrong bytes')
      },
    }),
    new RegExp(`SHA-256 mismatch.*${archiveName}`),
  )
  const cacheDir = path.join(root, '.cache/runtime-artifacts/v152.2.0')
  assert.ok(fetched.some((url) => String(url).endsWith(archiveName)))
  assert.ok(fetched.some((url) => String(url).endsWith(bindingName)))
  assert.deepEqual(await readdir(cacheDir), [])
})

test('reports unsupported targets without contacting the release host', async (t) => {
  const { root, checksumPath } = await fixtureRoot()
  t.after(() => rm(root, { recursive: true, force: true }))
  await writeFile(checksumPath, '')
  let fetched = false

  await assert.rejects(
    prepareV8Artifacts({ root, target: 'x86_64-unknown-freebsd', fetchImpl: async () => { fetched = true } }),
    /Unsupported Rust target.*x86_64-unknown-freebsd/,
  )
  assert.equal(fetched, false)
})
