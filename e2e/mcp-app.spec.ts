import { createServer, type Server } from 'node:http'
import { readFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import type { AddressInfo } from 'node:net'
import { expect, test } from '@playwright/test'

const token = 'c'.repeat(64)
const appUri = 'ui://fixture/app'
const initialResult = { content: [{ type: 'text', text: 'Original MCP tool result' }] }

async function startSandboxFixture(hostOrigin: string) {
  const rawProxy = await readFile(fileURLToPath(new URL('../src-tauri/src/mcp-sandbox/proxy.js', import.meta.url)), 'utf8')
  const server: Server = createServer((request, response) => {
    const address = server.address() as AddressInfo
    const sandboxOrigin = `http://127.0.0.1:${address.port}`
    const pagePath = `/widget/${token}`
    if (request.method !== 'GET') { response.writeHead(405).end(); return }
    if (request.url === pagePath) {
      const viewUrl = `${sandboxOrigin}${pagePath}/view`
      const script = rawProxy.replace('__HOST_ORIGIN__', JSON.stringify(hostOrigin)).replace('__VIEW_URL__', JSON.stringify(viewUrl))
      const nonce = 'fixture-nonce'
      response.writeHead(200, {
        'content-type': 'text/html; charset=utf-8',
        'content-security-policy': `default-src 'none'; script-src 'nonce-${nonce}'; style-src 'unsafe-inline'; frame-src 'self'; connect-src 'none'; img-src data: blob:; base-uri 'none'; form-action 'none'; object-src 'none'; frame-ancestors ${hostOrigin}`,
        'x-content-type-options': 'nosniff', 'cache-control': 'no-store',
      }).end(`<!doctype html><meta charset="utf-8"><style>html,body,iframe{width:100%;height:100%;margin:0;border:0}iframe{display:block}</style><iframe id="view" title="MCP App" sandbox="allow-scripts allow-same-origin allow-forms allow-downloads"></iframe><script nonce="${nonce}">${script}</script>`)
      return
    }
    if (request.url === `${pagePath}/view`) {
      response.writeHead(200, {
        'content-type': 'text/html; charset=utf-8',
        'content-security-policy': `default-src 'none'; script-src 'self' 'unsafe-inline' ${hostOrigin}; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; media-src 'self'; connect-src 'self' ${hostOrigin} ws://127.0.0.1:1420; frame-src 'none'; base-uri 'self'; form-action 'none'; object-src 'none'; frame-ancestors ${hostOrigin} ${sandboxOrigin}`,
        'x-content-type-options': 'nosniff', 'referrer-policy': 'no-referrer', 'cache-control': 'no-store',
      }).end(`<!doctype html><html><head><meta charset="utf-8"><title>Fixture MCP App</title></head><body>
        <main><h1>Fixture app ready</h1><output id="connect-status"></output><output id="tool-input"></output><output id="tool-result"></output>
        <button id="call-local">Call same-server tool</button><output id="local-call"></output>
        <button id="call-other">Call tool from another server</button><output id="other-call"></output><output id="app-error"></output></main>
        <button id="download-fixture">Download fixture</button>
        <script type="module" src="${hostOrigin}/e2e/fixtures/mcp-app.ts"></script></body></html>`)
      return
    }
    response.writeHead(404, { 'content-security-policy': "default-src 'none'" }).end('Not found')
  })
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
  const address = server.address() as AddressInfo
  return { server, url: `http://127.0.0.1:${address.port}/widget/${token}` }
}

test('MCP App SDK handshakes in isolated nested frames, receives results, and gates server tools', async ({ page }, testInfo) => {
  const hostOrigin = new URL(String(testInfo.project.use.baseURL ?? 'http://127.0.0.1:1420')).origin
  const sandbox = await startSandboxFixture(hostOrigin)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.addInitScript(({ sandboxUrl, appUri, initialResult }) => {
    const requests: any[] = []
    const thread = {
      id: 'mcp-app-thread', projectId: null, preview: 'Show the calendar app', cwd: '/workspace/mcp-app', workspace: {cwd: '/workspace/mcp-app', roots: ['/workspace/mcp-app'], folders: [{source: '/workspace/mcp-app', path: '/workspace/mcp-app'}], worktrees: [], scopeProjectId: null, projectRevision: null}, updatedAt: 1,
      turns: [{ id: 'turn-1', status: 'completed', items: [
        { id: 'user-1', type: 'userMessage', content: [{ type: 'text', text: 'Open the app' }] },
        { id: 'tool-1', type: 'mcpToolCall', status: 'completed', server: 'fixture-server', tool: 'launch',
          arguments: { date: '2026-11-01' }, result: initialResult,
          mcpAppUi: { resourceUri: appUri } },
      ] }],
    }
    const browser = window as any
    browser.__mcpAppRequests = requests
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'create_mcp_sandbox') return { url: sandboxUrl, token: 'c'.repeat(64) }
        if (command === 'destroy_mcp_sandbox') { requests.push({ command, args }); return null }
        if (command === 'open_external_url') return null
        if (command === 'list_projects') return { data: [] }
        if (command === 'create_project') return { id: 'project-mcp', revision: 1, ...args.project, hidden: false }
        if (command === 'save_project') return { ...args.project, revision: (args.project.revision ?? 1) + 1, hidden: false }
        if (command === 'delete_project' || command === 'move_chat_to_project') return null
        if (command === 'agents_request' && args.method === 'questions.list') return { data: [] }
        if (command === 'chat_request') {
          const {method, params = {}} = args
          ;((window as any).__chatRequests ??= []).push({method, params})
          if (method === 'drafts.list' || method === 'attachments.list') return []
          if (method === 'drafts.get') throw new Error('No saved fixture draft')
          if (method === 'drafts.save') return {...params, id: params.id, threadId: params.threadId ?? null, projectId: params.projectId ?? null, agentId: params.agentId ?? null, revision: (params.expectedRevision ?? 0) + 1, updatedAt: '2026-11-02T00:00:00Z'}
          if (method === 'drafts.promote') return {id: params.id, threadId: params.threadId, projectId: params.projectId ?? null, agentId: params.agentId ?? null, document: params.document ?? {text: '', references: [], quotes: []}, settings: params.settings ?? {model: '', effort: 'medium', mode: 'default'}, revision: 1, updatedAt: '2026-11-02T00:00:00Z'}
          if (method === 'foreground.submit') return {turn: {id: 'fixture-turn'}, turnId: 'fixture-turn', submission: {id: 'fixture-submission', threadId: params.threadId, status: 'accepted', revision: 1}, queued: false}
          if (method === 'queue.list') return {entries: [], state: {paused: false}}
          return {}
        }
        if (command !== 'engine_request') throw new Error(`Unexpected command ${command}`)
        requests.push(args)
        const { method } = args
        if (method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (method === 'model/list') return { data: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'medium' }] }] }
        if (method === 'thread/list') return { data: [thread], nextCursor: null }
        if (method === 'thread/resume') return { thread }
        if (method === 'mcpServer/resource/read') return { contents: [{
          uri: appUri, mimeType: 'text/html;profile=mcp-app',
          text: '<!doctype html><html><body>served by fixture</body></html>',
          _meta: { ui: { csp: {
            connectDomains: ['http://127.0.0.1:1420', 'ws://127.0.0.1:1420'],
            resourceDomains: ['http://127.0.0.1:1420'], frameDomains: [], baseUriDomains: [],
          } } },
        }] }
        if (method === 'mcpServerStatus/list') return { data: [
          { name: 'fixture-server', runtimeStatus: 'ready', tools: {
            lookup: { description: 'Look up items', inputSchema: { type: 'object', properties: { query: { type: 'string' } } }, _meta: { ui: { visibility: ['app'] } } },
          }, resources: [], resourceTemplates: [] },
          { name: 'other-server', runtimeStatus: 'ready', tools: { 'tool-from-other-server': { description: 'Must not cross servers' } }, resources: [], resourceTemplates: [] },
        ], nextCursor: null }
        if (method === 'mcpServer/tool/call') return { content: [{ type: 'text', text: 'Approved live tool result' }] }
        throw new Error(`Unexpected request ${method} ${JSON.stringify(args.params)}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  }, { sandboxUrl: sandbox.url, appUri, initialResult })

  try {
    await page.goto('/')
    await page.getByRole('button', { name: 'Show the calendar app' }).click()
    const outer = page.frameLocator('iframe[title="MCP App: launch"]')
    const app = outer.frameLocator('#view')
    await expect(app.getByRole('heading', { name: 'Fixture app ready' })).toBeVisible({ timeout: 20_000 })
    await expect(app.locator('#connect-status')).toContainText('connected')
    await expect(app.locator('#tool-input')).toContainText('2026-11-01')
    await expect(app.locator('#tool-result')).toContainText('Original MCP tool result')

    await app.getByRole('button', { name: 'Call tool from another server' }).click()
    await expect(app.locator('#other-call')).toContainText('not available to this MCP App')
    expect(await page.evaluate(() => (window as any).__mcpAppRequests.filter((request: any) => request.method === 'mcpServer/tool/call'))).toHaveLength(0)

    await app.getByRole('button', { name: 'Call same-server tool' }).click()
    await expect(page.getByRole('dialog', { name: 'Approve MCP App tool call' })).toBeVisible()
    await page.getByRole('button', { name: 'Deny' }).click()
    await expect(app.locator('#local-call')).toContainText('not approved')

    await app.getByRole('button', { name: 'Call same-server tool' }).click()
    await expect(page.getByRole('dialog', { name: 'Approve MCP App tool call' })).toBeVisible()
    await page.getByRole('button', { name: 'Allow once' }).click()
    await expect(app.locator('#local-call')).toContainText('Approved live tool result')
    const actualCall = await page.evaluate(() => (window as any).__mcpAppRequests.find((request: any) => request.method === 'mcpServer/tool/call'))
    expect(actualCall.params).toEqual({ threadId: 'mcp-app-thread', server: 'fixture-server', tool: 'lookup', arguments: { query: 'same server' } })

    const downloadPromise = page.waitForEvent('download')
    await app.getByRole('button', { name: 'Download fixture' }).click()
    const download = await downloadPromise
    expect(download.suggestedFilename()).toBe('fixture.txt')

    await page.setViewportSize({ width: 800, height: 600 })
    await expect(page.locator('iframe[title="MCP App: launch"]')).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
    expect(errors).toEqual([])
    await page.screenshot({ path: 'projects/desktop-agent/evidence/mcp-app-minimum.png', fullPage: true })
  } finally {
    await new Promise<void>((resolve, reject) => sandbox.server.close(error => error ? reject(error) : resolve()))
  }
})
