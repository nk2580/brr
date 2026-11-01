import assert from 'node:assert/strict'
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { spawnSync } from 'node:child_process'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { createSourceArchive, selectReleaseFiles } from './prepare-source-release.mjs'

test('selects releasable source and attribution while excluding local and generated material', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'brr-release-fixture-'))
  try {
    const files = [
      'LICENSE', 'NOTICE', 'README.md', 'package.json', 'src/App.tsx',
      'src-tauri/src/lib.rs', 'src-tauri/Cargo.toml', 'website/src/main.tsx', 'runtime/UPSTREAM.md',
      'runtime/codex/LICENSE', 'runtime/codex/NOTICE', 'runtime/codex/codex-rs/core/src/lib.rs',
      'runtime/codex/AGENTS.md', 'runtime/codex/CONTEXT.md',
      'scripts/prepare-source-release.mjs', 'docs/open-source-release.md',
      'e2e/preview.spec.ts', 'e2e/test-results/screenshot.png', 'e2e/screenshots/manual.png',
      'docs/adr/001-owned-runtime.md', 'docs/native-qa.md',
      'website/.openai/hosting.json', 'website/.sites-runtime/runtime.json',
      'website/src/app.tsbuildinfo', 'website/assets/raw-previews/hero.png',
      'website/public/assets/hero.webp', 'website/public/downloads/brr-source.tar.gz',
      'scripts/deployment.tar.gz', 'runtime/codex/tests/fixtures/upstream.tar.gz',
      '.env.example', '.env', 'account.env', 'projects/desktop-agent/evidence/session.json',
      'projects/desktop-agent/state.json', 'AGENTS.md', 'CONTEXT.md',
      '.cache/transcript.jsonl', '.tools/pnpm/bin/pnpm', 'node_modules/pkg/index.js',
      'src-tauri/target/debug/brr-runtime', 'src-tauri/binaries/brr-runtime',
      'runtime/codex/codex-rs/core/target/debug/a.out',
      'runtime/codex/bazel-out/bin/generated', 'runtime/codex/.git/config',
      'runtime/codex/tmp/account.env', 'src-tauri/signing.pem',
    ]
    for (const file of files) {
      const target = path.join(root, file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, 'fixture')
    }
    await mkdir(path.join(root, 'runtime/codex/.git/worktrees'), { recursive: true })
    await writeFile(path.join(root, 'runtime/codex/.git/worktrees/private'), 'fixture')
    await symlink(path.join(root, '.cache/transcript.jsonl'), path.join(root, 'src/private-link'))

    const selected = await selectReleaseFiles(root)
    assert.ok(selected.includes('runtime/codex/LICENSE'))
    assert.ok(selected.includes('runtime/codex/NOTICE'))
    assert.ok(selected.includes('runtime/codex/AGENTS.md'))
    assert.ok(selected.includes('runtime/codex/CONTEXT.md'))
    assert.ok(selected.includes('runtime/codex/codex-rs/core/src/lib.rs'))
    assert.ok(selected.includes('src/App.tsx'))
    assert.ok(selected.includes('e2e/preview.spec.ts'))
    assert.ok(selected.includes('.env.example'))
    assert.ok(selected.includes('docs/open-source-release.md'))
    assert.ok(selected.includes('docs/adr/001-owned-runtime.md'))
    assert.ok(selected.includes('website/src/main.tsx'))
    assert.ok(selected.includes('website/public/assets/hero.webp'))
    assert.ok(selected.includes('runtime/codex/tests/fixtures/upstream.tar.gz'))
    for (const excluded of ['docs/native-qa.md', 'website/.openai/hosting.json', 'website/.sites-runtime/runtime.json', 'website/src/app.tsbuildinfo', 'website/assets/raw-previews/hero.png', 'website/public/downloads/brr-source.tar.gz', 'scripts/deployment.tar.gz', 'e2e/test-results/screenshot.png', 'e2e/screenshots/manual.png']) {
      assert.ok(!selected.includes(excluded), `${excluded} must be excluded`)
    }
    for (const excluded of files.filter(file => /\.env$|account\.env|evidence|state\.json|^AGENTS|^CONTEXT|docs\/native-qa|\.openai|\.sites-runtime|tsbuildinfo|raw-previews|\.cache|\.tools|node_modules|target|bazel-out|\.git|\.pem$|binaries/.test(file))) {
      assert.ok(!selected.includes(excluded), `${excluded} must be excluded`)
    }
    assert.ok(!selected.includes('src/private-link'), 'symlinks must not be packaged')
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

test('creates a source archive with notices and without local evidence or symlink targets', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'brr-release-archive-'))
  try {
    const contents = {
      LICENSE: 'Apache License',
      NOTICE: 'Brr notice',
      'src/App.tsx': 'export const app = true',
      'runtime/UPSTREAM.md': 'Pinned source information',
      'runtime/codex/LICENSE': 'Upstream license',
      'runtime/codex/NOTICE': 'Upstream notices',
      'runtime/codex/src/lib.rs': 'pub fn runtime() {}',
      'projects/desktop-agent/evidence/private.json': 'local transcript data',
      '.cache/local.json': 'private cache',
      'src-tauri/binaries/brr-runtime': 'generated binary',
    }
    for (const [file, content] of Object.entries(contents)) {
      const target = path.join(root, file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, content)
    }
    if (process.platform === 'darwin') {
      const attribute = spawnSync('xattr', ['-w', 'com.brr.source-release-test', 'fixture', path.join(root, 'src/App.tsx')], { encoding: 'utf8' })
      assert.equal(attribute.status, 0, attribute.stderr)
    }
    const output = path.join(root, '.cache/releases/test.tar.gz')
    const result = await createSourceArchive({ sourceRoot: root, outputPath: output })
    assert.ok(result.bytes > 0)
    const listing = spawnSync('tar', ['-tzf', output], { encoding: 'utf8' })
    assert.equal(listing.status, 0, listing.stderr)
    const names = listing.stdout.trim().split('\n')
    assert.equal(names.length, result.files.length, 'archive should contain exactly the selected files without AppleDouble companions')
    assert.ok(!names.some(name => name.split('/').some(segment => segment.startsWith('._'))), 'AppleDouble companions must not be packaged')
    assert.ok(names.includes('LICENSE'))
    assert.ok(names.includes('runtime/codex/LICENSE'))
    assert.ok(names.includes('src/App.tsx'))
    assert.ok(!names.some(name => /evidence|\.cache|brr-runtime/.test(name)))
    assert.equal(await readFile(output).then(buffer => buffer.length), result.bytes)
    if (process.platform === 'darwin') {
      const inspect = spawnSync('python3', ['-c', [
        'import json, sys, tarfile',
        'with tarfile.open(sys.argv[1], "r:gz") as archive:',
        '    members = archive.getmembers()',
        '    names = [member.name for member in members]',
        '    metadata = [key for member in members for key in member.pax_headers if "xattr" in key.lower() or "copyfile" in key.lower() or "com.apple" in key.lower()]',
        'print(json.dumps({"members": len(members), "appledouble": sum(any(part.startswith("._") for part in name.split("/")) for name in names), "metadata": len(metadata)}))',
      ].join('\n'), output], { encoding: 'utf8' })
      assert.equal(inspect.status, 0, inspect.stderr)
      const metadata = JSON.parse(inspect.stdout)
      assert.equal(metadata.members, result.files.length)
      assert.equal(metadata.appledouble, 0)
      assert.equal(metadata.metadata, 0)
    }
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

test('rejects archive output inside a selected source tree', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'brr-release-output-'))
  try {
    for (const file of ['LICENSE', 'NOTICE', 'runtime/codex/LICENSE', 'runtime/codex/NOTICE', 'src/App.tsx']) {
      const target = path.join(root, file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, 'fixture')
    }
    await assert.rejects(
      createSourceArchive({ sourceRoot: root, outputPath: path.join(root, 'src/archive.tar.gz') }),
      /inside the source tree/,
    )
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

