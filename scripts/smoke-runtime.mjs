import { spawn } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import readline from 'node:readline'
import assert from 'node:assert/strict'
import { once } from 'node:events'

const engineHome = mkdtempSync(path.join(os.tmpdir(), 'brr-runtime-smoke-'))
const startupTimeoutMs = 60000
const executable = path.resolve(process.argv[2] || `src-tauri/binaries/brr-runtime${process.platform === 'win32' ? '.exe' : ''}`)
const env = {...process.env, CODEX_HOME:engineHome}
for (const name of ['OPENAI_API_KEY','OPENAI_BASE_URL','CODEX_API_KEY','CODEX_ACCESS_TOKEN','CODEX_AUTH_JSON']) delete env[name]
const child = spawn(executable, ['-c', 'forced_login_method="api"'], {cwd:engineHome, env, stdio:['pipe','pipe','pipe']})
const requests = new Map()
let id = 0
let stderr = ''
let childClosed = false
child.stderr.on('data', data => { stderr += data.toString().slice(0,2000) })
child.on('error', error => { for (const pending of requests.values()) pending.reject(error) })
child.on('exit', code => { for (const pending of requests.values()) pending.reject(new Error(`Runtime exited (${code}): ${stderr.slice(-1000)}`)) })
child.on('close', () => { childClosed = true })
readline.createInterface({input:child.stdout}).on('line', line => {
  let message
  try { message = JSON.parse(line) } catch { return }
  if (message.method) return
  const pending = requests.get(message.id)
  if (pending) { clearTimeout(pending.timer); requests.delete(message.id); pending.resolve(message) }
})
function rpc(method, params, timeoutMs = 15000) {
  return new Promise((resolve, reject) => {
    const current = ++id
    const timer = setTimeout(() => { requests.delete(current); reject(new Error(`Timed out: ${method} after ${timeoutMs} ms`)) }, timeoutMs)
    requests.set(current, {resolve,reject,timer})
    child.stdin.write(`${JSON.stringify({id:current,method,params})}\n`)
  })
}
function isTrustedLoginUrl(value) {
  try {
    const url = new URL(value)
    return url.protocol === 'https:' && ['auth.openai.com','chatgpt.com'].includes(url.hostname)
      && !url.username && !url.password && !url.port
  } catch {
    return false
  }
}
async function waitForExit(timeoutMs) {
  if (childClosed || child.exitCode !== null || child.signalCode !== null || child.pid === undefined) return
  let timer
  try {
    await Promise.race([
      once(child, 'close'),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Runtime did not exit')), timeoutMs) }),
    ])
  } finally {
    clearTimeout(timer)
  }
}
try {
  const initialized = await rpc('initialize', {clientInfo:{name:'brr_smoke',version:'0.1.0'},capabilities:null}, startupTimeoutMs)
  assert.ok(initialized.result, JSON.stringify(initialized.error))
  child.stdin.write('{"method":"initialized"}\n')
  const account = await rpc('account/read', {refreshToken:false})
  assert.equal(account.result?.account, null)
  const models = await rpc('model/list', {includeHidden:false})
  assert.ok(Array.isArray(models.result?.data), JSON.stringify(models.error))
  const threads = await rpc('thread/list', {limit:100,sortKey:'updated_at'})
  assert.ok(Array.isArray(threads.result?.data), JSON.stringify(threads.error))
  const keyLogin = await rpc('account/login/start', {type:'apiKey',apiKey:'brr-test-invalid-not-a-secret'})
  assert.match(keyLogin.error?.message ?? '', /ChatGPT/i)
  const login = await rpc('account/login/start', {type:'chatgpt'})
  assert.equal(login.result?.type, 'chatgpt', JSON.stringify(login.error))
  assert.match(login.result?.loginId ?? '', /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i)
  assert.ok(isTrustedLoginUrl(login.result?.authUrl), `Untrusted OAuth URL returned: ${login.result?.authUrl}`)
  const cancelled = await rpc('account/login/cancel', {loginId:login.result.loginId})
  assert.equal(cancelled.result?.status, 'canceled', JSON.stringify(cancelled.error))
  const turn = await rpc('turn/start', {threadId:'00000000-0000-0000-0000-000000000000',input:[{type:'text',text:'Do not run, authentication rejection test.',text_elements:[]}]})
  assert.match(turn.error?.message ?? '', /ChatGPT/i)
  console.log('Owned runtime smoke passed: handshake, signed-out model/history loading, API-key login rejection, ChatGPT OAuth start/trusted URL/cancel, inference rejection.')
} finally {
  for (const pending of requests.values()) clearTimeout(pending.timer)
  if (child.pid !== undefined && child.exitCode === null && child.signalCode === null && !childClosed) {
    child.stdin.end()
    try {
      await waitForExit(5000)
    } catch {
      child.kill('SIGTERM')
      try {
        await waitForExit(2000)
      } catch {
        child.kill('SIGKILL')
        await waitForExit(2000)
      }
    }
  }
  rmSync(engineHome, {recursive:true,force:true})
}
