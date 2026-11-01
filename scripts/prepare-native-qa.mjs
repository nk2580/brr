import { randomBytes } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import { mkdirSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const fixturesRoot = path.join(repoRoot, '.cache', 'native-qa')
mkdirSync(fixturesRoot, { recursive: true })

let fixtureId
let fixtureRoot
for (let attempt = 0; attempt < 20; attempt += 1) {
  const timestamp = new Date().toISOString().replace(/[-:.]/g, '')
  fixtureId = `${timestamp}-${randomBytes(3).toString('hex')}`
  fixtureRoot = path.join(fixturesRoot, fixtureId)
  try {
    mkdirSync(fixtureRoot)
    break
  } catch (error) {
    if (error.code !== 'EEXIST' || attempt === 19) throw error
  }
}

const workspace = path.join(fixtureRoot, 'workspace')
const approvalTarget = path.join(fixtureRoot, 'approval-target')
mkdirSync(workspace)
mkdirSync(approvalTarget)

writeFileSync(path.join(workspace, 'go.mod'), 'module native-qa.local/mathx\n\ngo 1.23\n')
writeFileSync(path.join(workspace, 'adder.go'), `package mathx

// Add is intentionally incorrect for the native QA baseline.
func Add(a, b int) int {
	return a - b
}
`)
writeFileSync(path.join(workspace, 'adder_test.go'), `package mathx

import "testing"

func TestAdd(t *testing.T) {
	tests := []struct {
		name string
		a, b int
		want int
	}{
		{name: "positive values", a: 2, b: 3, want: 5},
		{name: "mixed signs", a: -2, b: 1, want: -1},
		{name: "zero", a: 0, b: 4, want: 4},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if got := Add(test.a, test.b); got != test.want {
				t.Fatalf("Add(%d, %d) = %d, want %d", test.a, test.b, got, test.want)
			}
		})
	}
}
`)
writeFileSync(path.join(workspace, '.gitignore'), `/.cache/\n`)
writeFileSync(path.join(workspace, 'test.sh'), `#!/bin/sh
set -eu
mkdir -p .cache/go-build
export GOCACHE="$PWD/.cache/go-build"
export GOTOOLCHAIN=local
export GOPROXY=off
export GOSUMDB=off
export GOWORK=off
export GOENV=off
export GOFLAGS=
exec go test ./...
`, { mode: 0o755 })
writeFileSync(path.join(workspace, 'TASK.md'), `# Native QA task

Fix the intentionally incorrect Add implementation in adder.go so the table tests pass. Run ./test.sh and inspect git diff after the edit.

Controlled approval target (sibling directory, outside this workspace):

${approvalTarget}/approval-marker.txt

Use this path only for the separate write-approval exercise in docs/native-qa.md. Start by requesting the write and denying it; verify the marker remains absent. Then repeat and allow the write.
`)
writeFileSync(path.join(approvalTarget, 'README.md'), `# Controlled approval target

This directory is outside the Go workspace. The approval exercise uses approval-marker.txt. It is intentionally absent at fixture creation. Do not create it before the native app presents a real approval request and the tester explicitly allows that request.
`)

const gitEnv = { ...process.env }
for (const name of Object.keys(gitEnv)) {
  if (name.startsWith('GIT_')) delete gitEnv[name]
}
Object.assign(gitEnv, {
  GIT_CONFIG_NOSYSTEM: '1',
  GIT_CONFIG_GLOBAL: '/dev/null',
  GIT_TERMINAL_PROMPT: '0',
})
function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? workspace,
    env: options.env ?? process.env,
    encoding: 'utf8',
    stdio: 'pipe',
  })
  if (result.error) throw result.error
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} failed (${result.status}):\n${result.stdout}${result.stderr}`)
  }
  return `${result.stdout}${result.stderr}`
}
function runGit(args) {
  return run('git', args, { env: gitEnv })
}

runGit(['init', '--quiet', '--template='])
runGit(['config', '--local', 'user.name', 'Native QA Fixture'])
runGit(['config', '--local', 'user.email', 'native-qa-fixture@example.invalid'])
runGit(['config', '--local', 'commit.gpgsign', 'false'])
runGit(['config', '--local', 'core.hooksPath', '/dev/null'])
runGit(['branch', '-M', 'main'])
runGit(['add', '--', '.gitignore', 'go.mod', 'adder.go', 'adder_test.go', 'TASK.md', 'test.sh'])
runGit(['-c', 'commit.gpgsign=false', 'commit', '--no-gpg-sign', '-m', 'Initial intentionally failing QA fixture'])
if (runGit(['remote']).trim() !== '') throw new Error('Fixture unexpectedly has a git remote.')

const baseline = spawnSync('./test.sh', [], { cwd: workspace, env: process.env, encoding: 'utf8', stdio: 'pipe' })
if (baseline.error) throw baseline.error
const baselineOutput = `${baseline.stdout}${baseline.stderr}`
if (baseline.status !== 1 || !baselineOutput.includes('--- FAIL: TestAdd')) {
  throw new Error(`Expected ./test.sh to exit 1 with a failing TestAdd baseline; it exited ${baseline.status ?? 'without a status'}:\n${baselineOutput}`)
}

console.log(`Created a fresh native QA fixture: ${fixtureId}`)
console.log(`Workspace: ${workspace}`)
console.log(`Approval target: ${approvalTarget}`)
console.log(`Initial baseline: expected red (./test.sh exits ${baseline.status})`)
console.log(baselineOutput.trim())
console.log('\nNext: open the workspace in the native app and follow docs/native-qa.md. Use a real ChatGPT sign-in; do not inject credentials or fake auth state.')
