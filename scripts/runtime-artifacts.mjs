import { createHash, randomUUID } from 'node:crypto'
import { createReadStream, createWriteStream } from 'node:fs'
import { mkdir, readFile, rename, rm } from 'node:fs/promises'
import path from 'node:path'
import { Readable, Transform } from 'node:stream'
import { pipeline } from 'node:stream/promises'

const V8_VERSION = '152.2.0'
const RELEASE_ROOT = `https://github.com/openai/codex/releases/download/rusty-v8-v${V8_VERSION}/`

export async function prepareV8Artifacts({ root, target, fetchImpl = fetch }) {
  const repositoryRoot = path.resolve(root)
  const archiveName = target.includes('windows')
    ? `rusty_v8_ptrcomp_sandbox_release_${target}.lib.gz`
    : `librusty_v8_ptrcomp_sandbox_release_${target}.a.gz`
  const bindingName = `src_binding_ptrcomp_sandbox_release_${target}.rs`
  const checksumFile = path.join(
    repositoryRoot,
    'runtime/codex/third_party/v8/rusty_v8_152_2_0.sha256',
  )
  const checksums = parseChecksums(await readFile(checksumFile, 'utf8'))
  const assets = [archiveName, bindingName].map((name) => {
    const expectedHash = checksums.get(name)
    if (!expectedHash) {
      throw new Error(`Unsupported Rust target '${target}': no pinned V8 artifact '${name}'`)
    }
    return { name, expectedHash }
  })
  const cacheDir = path.join(repositoryRoot, '.cache/runtime-artifacts', `v${V8_VERSION}`)
  await mkdir(cacheDir, { recursive: true })

  const results = await Promise.allSettled(assets.map(async ({ name, expectedHash }) => {
    const filePath = path.join(cacheDir, name)
    if (await sha256File(filePath) !== expectedHash) {
      await downloadVerifiedArtifact({
        filePath,
        name,
        expectedHash,
        fetchImpl,
      })
    }
    return filePath
  }))
  const failure = results.find((result) => result.status === 'rejected')
  if (failure) throw failure.reason
  const paths = results.map((result) => result.value)

  return {
    RUSTY_V8_ARCHIVE: paths[0],
    RUSTY_V8_SRC_BINDING_PATH: paths[1],
  }
}

function parseChecksums(contents) {
  const checksums = new Map()
  for (const line of contents.split(/\r?\n/)) {
    const match = line.trim().match(/^([a-f\d]{64})\s+\*?(.+)$/i)
    if (match) checksums.set(match[2], match[1].toLowerCase())
  }
  return checksums
}

async function sha256File(filePath) {
  const hash = createHash('sha256')
  try {
    for await (const chunk of createReadStream(filePath)) hash.update(chunk)
  } catch (error) {
    if (error?.code === 'ENOENT') return null
    throw error
  }
  return hash.digest('hex')
}

async function downloadVerifiedArtifact({ filePath, name, expectedHash, fetchImpl }) {
  const response = await fetchImpl(new URL(name, RELEASE_ROOT).href)
  if (!response?.ok) {
    throw new Error(`Failed to download pinned V8 artifact '${name}': HTTP ${response?.status ?? 'unknown'}`)
  }
  if (!response.body) {
    throw new Error(`Pinned V8 artifact '${name}' returned an empty response body`)
  }

  const tempPath = `${filePath}.${process.pid}.${randomUUID()}.tmp`
  const hash = createHash('sha256')
  const hashingStream = new Transform({
    transform(chunk, _encoding, callback) {
      hash.update(chunk)
      callback(null, chunk)
    },
  })

  try {
    await pipeline(
      Readable.fromWeb(response.body),
      hashingStream,
      createWriteStream(tempPath, { flags: 'wx' }),
    )
    const actualHash = hash.digest('hex')
    if (actualHash !== expectedHash) {
      throw new Error(
        `SHA-256 mismatch for '${name}': expected ${expectedHash}, got ${actualHash}`,
      )
    }
    await rm(filePath, { force: true })
    await rename(tempPath, filePath)
  } catch (error) {
    await rm(tempPath, { force: true })
    throw error
  }
}
