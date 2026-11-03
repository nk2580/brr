import { test, expect } from '@playwright/test'

test('Extensions panel installs plugins and preserves MCP configuration, OAuth, and prompt drafts', async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem('brr.appearance.sidebarColor', 'lime')
    const thread = {
      id: 'extensions-thread', projectId: null, preview: 'Review the extension setup', cwd: '/workspace/extensions', workspace: {cwd: '/workspace/extensions', roots: ['/workspace/extensions'], folders: [{source: '/workspace/extensions', path: '/workspace/extensions'}], worktrees: [], scopeProjectId: null, projectRevision: null}, updatedAt: 1,
      turns: [{ id: 'turn-1', status: 'completed', items: [
        { id: 'user-1', type: 'userMessage', content: [{ type: 'text', text: 'Set up the extension.' }] },
        { id: 'assistant-1', type: 'agentMessage', text: 'Ready to review the extension setup.' },
      ] }],
    }
    const plugin = {
      id: 'calendar@fixture', name: 'calendar', installed: false, enabled: false,
      source: { type: 'local', path: '/marketplaces/fixture/plugins/calendar' },
      installPolicy: 'AVAILABLE', availability: 'AVAILABLE', description: 'Calendar integration',
    }
    const requests: any[] = []
    const browser = window as any
    browser.__extensionsRequests = requests
    browser.__extensionPluginInstalled = false
    browser.__extensionConfig = {
      command: 'node', args: ['server.js'], env: { API_TOKEN: 'fixture-secret' },
      http_headers: { 'X-Custom-Auth': 'header-secret' },
      customRuntimeField: { preserve: true },
    }
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'open_external_url') {
          requests.push({ command, args })
          return null
        }
        if (command === 'list_projects') return { data: [] }
        if (command === 'create_project') return { id: 'project-extension', revision: 1, ...args.project, hidden: false }
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
        const { method, params } = args
        requests.push(args)
        if (method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (method === 'model/list') return { data: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'Sol', isDefault: true, supportedReasoningEfforts: [{ reasoningEffort: 'medium' }], defaultReasoningEffort: 'medium' }] }
        if (method === 'thread/list') return { data: [thread], nextCursor: null }
        if (method === 'thread/resume') return { thread }
        if (method === 'plugin/list') return {
          marketplaces: [{ name: 'fixture-marketplace', path: '/marketplaces/fixture', plugins: [{ ...plugin, installed: browser.__extensionPluginInstalled, enabled: browser.__extensionPluginInstalled }] }],
          marketplaceLoadErrors: [], featuredPluginIds: [],
        }
        if (method === 'plugin/read') return { plugin: { summary: { ...plugin }, description: 'Calendar integration', skills: [], hooks: [], apps: [], mcpServers: [] } }
        if (method === 'plugin/install') { browser.__extensionPluginInstalled = true; return { authPolicy: 'none', appsNeedingAuth: [] } }
        if (method === 'config/read') return { config: { mcpServers: { 'qa-server': browser.__extensionConfig } } }
        if (method === 'config/value/write') { browser.__extensionConfig = params.value; return {} }
        if (method === 'config/mcpServer/reload') return {}
        if (method === 'mcpServerStatus/list') return { data: [{
          name: 'qa-server', runtimeStatus: 'ready', tools: { lookup: { description: 'Look up events' } },
          resources: [{ uri: 'calendar://events', name: 'Events' }], resourceTemplates: [], authStatus: 'authenticated',
          serverInfo: { name: 'Fixture calendar', version: '1.0' },
        }], nextCursor: null }
        if (method === 'mcpServer/oauth/login') return { authorizationUrl: 'https://auth.example.test/authorize?client_id=fixture' }
        if (method === 'mcpServer/prompts/list') return { prompts: [{ name: 'daily-brief', title: 'Daily brief', description: 'Prepare a daily brief' }], nextCursor: null }
        if (method === 'mcpServer/prompt/get') return { description: 'A user supplied prompt', messages: [{ role: 'user', content: [{ type: 'text', text: 'Summarize today’s calendar.' }] }] }
        throw new Error(`Unexpected request ${method} ${JSON.stringify(params)}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  })

  await page.goto('/')
  await page.getByRole('button', { name: 'Review the extension setup' }).click()
  await page.getByRole('textbox', { name: 'Message' }).fill('Keep this draft')
  const neutralMenu = await page.getByRole('complementary', { name: 'Main sidebar' }).evaluate(element => getComputedStyle(element).backgroundColor)
  await page.getByRole('button', { name: 'Plugins', exact: true }).click()
  await expect(page.getByRole('complementary', { name: 'Plugins and MCP connections' })).toBeVisible()
  const pluginMenu = page.getByRole('tablist', { name: 'Extension type' })
  await expect(pluginMenu).toBeVisible()
  expect(await pluginMenu.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(neutralMenu)
  expect(await page.getByRole('navigation', { name: 'Main rail' }).evaluate(element => getComputedStyle(element).backgroundColor)).not.toBe(neutralMenu)
  await expect(page.getByRole('complementary', { name: 'Main sidebar' })).toBeHidden()
  await expect(page.getByRole('button', { name: 'Collapse sidebar', exact: true })).toHaveAttribute('aria-controls', 'plugins-sidebar')
  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  await expect(pluginMenu).toBeHidden()
  await expect(page.getByRole('navigation', { name: 'Main rail' })).toBeVisible()
  await page.getByRole('button', { name: 'Plugins', exact: true }).click()
  await expect(pluginMenu).toBeHidden()
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click()
  await expect(pluginMenu).toBeVisible()

  await page.getByRole('button', { name: 'Install calendar' }).click()
  await expect(page.getByText('Plugin installation completed.')).toBeVisible()
  await expect(page.locator('.plugin-row').filter({ hasText: 'calendar' }).getByText('Enabled')).toBeVisible()
  const install = await page.evaluate(() => (window as any).__extensionsRequests.find((request: any) => request.method === 'plugin/install'))
  expect(install.params).toEqual({ marketplacePath: '/marketplaces/fixture', pluginName: 'calendar' })

  await page.getByRole('tab', { name: 'MCP connections' }).click()
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.getByText(/Authorization started for qa-server/)).toBeVisible()
  const oauth = await page.evaluate(() => (window as any).__extensionsRequests.find((request: any) => request.method === 'mcpServer/oauth/login'))
  expect(oauth.params).toEqual({ name: 'qa-server', threadId: 'extensions-thread' })
  await page.getByRole('button', { name: 'Open authorization page' }).click()
  const openedAuth = await page.evaluate(() => (window as any).__extensionsRequests.find((request: any) => request.command === 'open_external_url'))
  expect(openedAuth.args.url).toBe('https://auth.example.test/authorize?client_id=fixture')

  await page.getByRole('combobox', { name: 'Select existing MCP server' }).selectOption('qa-server')
  const editor = page.getByRole('textbox', { name: 'MCP server configuration JSON' })
  await expect(editor).toContainText('[[stored secret hidden]]')
  await expect(editor).not.toContainText('fixture-secret')
  await expect(editor).not.toContainText('header-secret')
  const edited = JSON.parse(await editor.inputValue())
  edited.customRuntimeField.preserve = true
  edited.timeout = 45000
  await editor.fill(JSON.stringify(edited, null, 2))
  await page.getByRole('button', { name: 'Save MCP server' }).click()
  await expect(page.getByText('Saved and reloaded MCP server “qa-server”.')).toBeVisible()
  const write = await page.evaluate(() => (window as any).__extensionsRequests.find((request: any) => request.method === 'config/value/write'))
  expect(write.params).toEqual({ keyPath: 'mcp_servers."qa-server"', mergeStrategy: 'replace', value: {
    command: 'node', args: ['server.js'], env: { API_TOKEN: 'fixture-secret' },
    http_headers: { 'X-Custom-Auth': 'header-secret' }, customRuntimeField: { preserve: true }, timeout: 45000,
  } })
  expect(await page.evaluate(() => (window as any).__extensionsRequests.filter((request: any) => request.method === 'config/mcpServer/reload').length)).toBe(1)

  await page.getByRole('button', { name: 'Load MCP prompts' }).click()
  await page.getByText('Daily brief', { exact: true }).click()
  await page.getByRole('button', { name: 'Get prompt daily-brief' }).click()
  await expect(page.getByLabel('Retrieved MCP prompt payload')).toContainText('Summarize today’s calendar.')
  await page.screenshot({ path: 'projects/desktop-agent/evidence/extensions-desktop.png', fullPage: true })
  await page.setViewportSize({ width: 800, height: 600 })
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await expect(page.getByRole('button', { name: 'Use prompt as message' })).toBeVisible()
  await page.getByRole('button', { name: 'Use prompt as message' }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: 'projects/desktop-agent/evidence/extensions-minimum.png', fullPage: true })
  await page.getByRole('button', { name: 'Use prompt as message' }).click()
  await expect(page.getByRole('complementary', { name: 'Plugins and MCP connections' })).toHaveCount(0)
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveText('Keep this draft\n\nuser: Summarize today’s calendar.')
  const promptRequest = await page.evaluate(() => (window as any).__extensionsRequests.find((request: any) => request.method === 'mcpServer/prompt/get'))
  expect(promptRequest.params).toEqual({ server: 'qa-server', name: 'daily-brief', threadId: 'extensions-thread' })
})
