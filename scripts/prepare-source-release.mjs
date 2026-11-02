import { spawnSync } from 'node:child_process'
import { readdir, mkdir, stat } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const rootFiles = new Set([
  '.gitignore', '.gitmodules', '.env.example', 'components.json', 'index.html', 'package.json', 'pnpm-lock.yaml', 'rust-toolchain.toml',
  'playwright.config.ts', 'tsconfig.json', 'vite.config.ts', 'README.md', 'PRODUCT.md',
  'DESIGN.md', 'LICENSE', 'NOTICE', 'CONTRIBUTING.md', 'GOVERNANCE.md',
])
const includedTrees = new Set(['src', 'src-tauri', 'runtime', 'vendor', 'scripts', 'docs', 'website', 'e2e'])
const excludedDirectoryNames = new Set([
  '.git', '.cache', '.openai', '.sites-runtime', '.tools', 'node_modules', 'target', 'dist', 'gen', 'bazel-out',
  'bazel-bin', 'bazel-testlogs', 'bazel-src', 'test-results', 'playwright-report',
  'raw-previews', 'generated-previews',
])

function isExcluded(relativePath, entry) {
  const segments = relativePath.split('/')
  if (segments.some(segment => excludedDirectoryNames.has(segment) || /^bazel-[^/]+$/.test(segment))) return true
  if (relativePath.startsWith('projects/')) return true
  if (relativePath === 'website/assets/raw' || relativePath.startsWith('website/assets/raw/')) return true
  if (relativePath === 'website/public/generated' || relativePath.startsWith('website/public/generated/')) return true
  if (relativePath === 'website/public/downloads' || relativePath.startsWith('website/public/downloads/')) return true
  if (relativePath.startsWith('src-tauri/binaries/')) return true
  if (relativePath.startsWith('docs/') && relativePath !== 'docs/open-source-release.md' && relativePath !== 'docs/adr' && !relativePath.startsWith('docs/adr/')) return true
  if (relativePath.startsWith('e2e/')) {
    if (segments.slice(1).some(segment => ['screenshots', 'results', 'videos', 'traces'].includes(segment))) return true
    if (/\.(?:png|jpe?g|webp|gif|avif|mp4|webm|zip)$/i.test(segments.at(-1) ?? '')) return true
  }
  if (entry.isSymbolicLink()) return true
  const name = segments.at(-1) ?? ''
  if (/^\.env(?:\..*)?$/.test(name) && name !== '.env.example') return true
  if (/^account\.env$/i.test(name)) return true
  if (!relativePath.startsWith('runtime/codex/') && /^(?:account(?:\..*)?\.json|auth\.json|credentials\.json)$/i.test(name)) return true
  if ((!relativePath.includes('/') || relativePath.startsWith('src-tauri/')) && /\.(?:pem|p12|key|mobileprovision)$/i.test(name)) return true
  if (/\.(?:app|dmg|exe|dll|so|dylib|a|o|rlib)$/i.test(name)) return true
  if (/\.tsbuildinfo$/i.test(name)) return true
  if (/^(?:preview|raw-preview|generated-preview)\.(?:png|jpe?g|webp|gif|avif|svg)$/i.test(name)) return true
  if (!relativePath.startsWith('runtime/codex/') && /\.(?:tar(?:\.(?:gz|bz2|xz))?|tgz|zip)$/i.test(name)) return true
  return false
}

export async function selectReleaseFiles(sourceRoot = rootDir) {
  const selected = []
  async function visit(relativeDirectory = '') {
    const absoluteDirectory = path.join(sourceRoot, relativeDirectory)
    const entries = await readdir(absoluteDirectory, { withFileTypes: true })
    entries.sort((a, b) => a.name.localeCompare(b.name))
    for (const entry of entries) {
      const relativePath = relativeDirectory ? `${relativeDirectory}/${entry.name}` : entry.name
      if (isExcluded(relativePath, entry)) continue
      if (entry.isDirectory()) {
        if (!relativeDirectory && !includedTrees.has(entry.name)) continue
        await visit(relativePath)
      } else if (entry.isFile() && (!relativeDirectory ? rootFiles.has(entry.name) : true)) {
        selected.push(relativePath)
      }
    }
  }
  await visit()
  return selected.sort()
}

export async function createSourceArchive({ sourceRoot = rootDir, outputPath = path.join(sourceRoot, '.cache/releases/brr-source.tar.gz') } = {}) {
  const files = await selectReleaseFiles(sourceRoot)
  if (!files.includes('LICENSE') || !files.includes('NOTICE') || !files.includes('runtime/codex/LICENSE') || !files.includes('runtime/codex/NOTICE')) {
    throw new Error('Release source is missing root or upstream license notices')
  }
  const absoluteOutput = path.resolve(outputPath)
  const absoluteSourceRoot = path.resolve(sourceRoot)
  const relativeOutput = path.relative(absoluteSourceRoot, absoluteOutput)
  const outputIsInsideSource = relativeOutput !== '' && !relativeOutput.startsWith(`..${path.sep}`) && relativeOutput !== '..' && !path.isAbsolute(relativeOutput)
  if (absoluteOutput === absoluteSourceRoot || (outputIsInsideSource && !relativeOutput.startsWith(`.cache${path.sep}releases${path.sep}`))) {
    throw new Error('Archive output inside the source tree must be under .cache/releases/')
  }
  await mkdir(path.dirname(absoluteOutput), { recursive: true })
  const tarArgs = ['-czf', absoluteOutput, '-C', path.resolve(sourceRoot), '--no-xattrs']
  if (process.platform === 'darwin') tarArgs.push('--disable-copyfile')
  tarArgs.push('--', ...files)
  const result = spawnSync('tar', tarArgs, {
    env: { ...process.env, COPYFILE_DISABLE: '1' },
    encoding: 'utf8',
    maxBuffer: 1024 * 1024 * 8,
  })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`tar failed: ${result.stderr || result.stdout}`)
  return { outputPath: absoluteOutput, files, bytes: (await stat(absoluteOutput)).size }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const outputPath = process.argv[2] ? path.resolve(process.argv[2]) : undefined
  const result = await createSourceArchive({ outputPath })
  console.log(`Created ${result.outputPath} (${result.bytes} bytes, ${result.files.length} files)`)
}
