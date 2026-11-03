import { test, expect } from '@playwright/test'

test('saved worktrees display their location and new chats use the original project', async ({ page }) => {
  await page.addInitScript(() => {
    const project = {id: 'project-atlas', name: 'Atlas', folders: ['/workspace/atlas'], primaryFolder: '/workspace/atlas', revision: 1, model: null, effort: null, permission: null, hidden: false}
    const saved = {
      id: 'saved', projectId: project.id, preview: 'Isolate the search redesign', cwd: '/workspace/atlas.search', updatedAt: 1,
      worktree: { projectPath: '/workspace/atlas', path: '/workspace/atlas.search', cwd: '/workspace/atlas.search', branch: 'codex/brr-search' },
      workspace: {cwd: '/workspace/atlas.search', roots: ['/workspace/atlas.search'], folders: [{source: '/workspace/atlas', path: '/workspace/atlas.search'}], worktrees: [{projectPath: '/workspace/atlas', path: '/workspace/atlas.search', cwd: '/workspace/atlas.search', branch: 'codex/brr-search'}], scopeProjectId: project.id, projectRevision: 1},
      turns: [{ id: 'saved-turn', status: 'completed', items: [
        { id: 'user', type: 'userMessage', content: [{ type: 'text', text: 'Improve search without disturbing the other agent’s work.' }] },
        { id: 'assistant', type: 'agentMessage', text: 'The search changes are isolated in this conversation’s worktree. The original project stays available for other sessions.' },
      ] }],
    }
    const requests: unknown[] = []
    const browser = window as any
    browser.__worktreeRequests = requests
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'agents_request' && args.method === 'questions.list') return { data: [] }
        if (command === 'list_projects') return { data: [project] }
        if (command === 'create_project') return {id: `created-${args.project.name}`, revision: 1, ...args.project, hidden: false}
        if (command === 'save_project') return { ...args.project, revision: args.project.revision + 1, hidden: false }
        if (command === 'delete_project' || command === 'move_chat_to_project') return null
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
        if (method === 'thread/list') return { data: [saved], nextCursor: null }
        if (method === 'thread/resume') return { thread: saved }
        if (method === 'brr/project/inspect') return { commands: [] }
        if (method === 'thread/start') return { thread: { id: 'new', projectId: params.projectId, preview: '', cwd: '/workspace/atlas.new', updatedAt: 2, workspace: {cwd: '/workspace/atlas.new', roots: ['/workspace/atlas.new'], folders: [{source: '/workspace/atlas', path: '/workspace/atlas.new'}], worktrees: [{projectPath: '/workspace/atlas', path: '/workspace/atlas.new', cwd: '/workspace/atlas.new', branch: 'codex/brr-new'}], scopeProjectId: project.id, projectRevision: 1},
          worktree: { projectPath: params.cwd, path: '/workspace/atlas.new', cwd: '/workspace/atlas.new', branch: 'codex/brr-new' } } }
        if (method === 'turn/start') return { turn: { id: 'new-turn' } }
        throw new Error(`Unexpected request ${method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  })
  await page.goto('/')
  await page.getByRole('button', { name: 'Isolate the search redesign' }).click()
  await page.getByRole('button', { name: 'Chat options', exact: true }).click()
  await expect(page.getByLabel('Session worktree')).toHaveText('Worktree · codex/brr-search')
  await expect(page.getByLabel('Session worktree')).toHaveAttribute('title', '/workspace/atlas.search')
  await expect(page.getByTitle('/workspace/atlas.search', { exact: true })).toBeVisible()
  await page.screenshot({ path: 'projects/desktop-agent/evidence/worktrees-desktop.png' })
  await page.setViewportSize({ width: 800, height: 600 })
  await page.getByRole('button', { name: 'Chat options', exact: true }).click()
  await expect(page.getByLabel('Session worktree')).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/worktrees-minimum.png' })
  await page.getByRole('button', { name: 'New conversation' }).click()
  await page.getByRole('button', { name: 'Chat options', exact: true }).click()
  await expect(page.getByText('New chats use a separate worktree.')).toBeVisible()
  await page.getByRole('textbox', { name: 'Message' }).fill('A separate task')
  await page.getByRole('button', { name: 'Send message' }).click()
  await page.getByRole('button', { name: 'Chat options', exact: true }).click()
  await expect(page.getByLabel('Session worktree')).toHaveText('Worktree · codex/brr-new')
  const start = await page.evaluate(() => (window as any).__worktreeRequests.find((request: any) => request.method === 'thread/start'))
  expect(start.params.projectId).toBe('project-atlas')
  expect(start.params.sandbox).toBe('workspace-write')
  await expect(page.getByRole('alert')).toHaveCount(0)
})
