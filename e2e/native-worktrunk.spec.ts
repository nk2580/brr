import { test, expect } from '@playwright/test'

test('reviews native project setup before creating a worktree or sending a turn', async ({ page }) => {
  await page.addInitScript(() => {
    const browser = window as any
    const requests: any[] = []
    const project = {id: 'project-atlas', name: 'Atlas', folders: ['/workspace/atlas'], primaryFolder: '/workspace/atlas', revision: 1, model: null, effort: null, permission: null, hidden: false}
    let approved = false
    browser.__nativeWorktrunkRequests = requests
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'plugin:dialog|open') return '/workspace/atlas'
        if (command === 'list_projects') return { data: [project] }
        if (command === 'create_project') return {id: `created-${args.project.name}`, revision: 1, ...args.project, hidden: false}
        if (command === 'save_project') return { ...args.project, revision: args.project.revision + 1, hidden: false }
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
        if (args.method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (args.method === 'model/list') return { data: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'Sol', isDefault: true, supportedReasoningEfforts: [], defaultReasoningEffort: 'medium' }] }
        if (args.method === 'thread/list') return { data: [], nextCursor: null }
        if (args.method === 'brr/project/inspect') return { commands: [{ cwd: '/workspace/atlas', phase: 'pre-start', name: 'Install dependencies', template: 'pnpm install --frozen-lockfile', approved }] }
        if (args.method === 'brr/worktree/approve') { approved = true; return { approved: true } }
        if (args.method === 'thread/start') return { thread: { id: 'new', projectId: args.params.projectId, cwd: '/workspace/atlas.session', preview: '', updatedAt: 1, workspace: {cwd: '/workspace/atlas.session', roots: ['/workspace/atlas.session'], folders: [{source: '/workspace/atlas', path: '/workspace/atlas.session'}], worktrees: [{projectPath: '/workspace/atlas', path: '/workspace/atlas.session', cwd: '/workspace/atlas.session', branch: 'codex/brr-session'}], scopeProjectId: project.id, projectRevision: project.revision},
          worktree: { projectPath: '/workspace/atlas', path: '/workspace/atlas.session', cwd: '/workspace/atlas.session', branch: 'codex/brr-session' } } }
        if (args.method === 'turn/start') return { turn: { id: 'turn-1' } }
        throw new Error(`Unexpected request ${args.method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  })
  await page.goto('/')
  await page.getByRole('combobox', { name: 'Project' }).selectOption('project-atlas')
  await page.getByRole('textbox', { name: 'Message' }).fill('Improve search in a separate worktree')
  await page.getByRole('button', { name: 'Send message' }).click()
  const review = page.getByRole('region', { name: 'Worktree setup approval' })
  await expect(review).toContainText('pnpm install --frozen-lockfile')
  expect(await page.evaluate(() => (window as any).__nativeWorktrunkRequests.some((request: any) => ['thread/start', 'turn/start'].includes(request.method)))).toBe(false)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/native-worktrunk-setup.png' })
  await page.setViewportSize({ width: 800, height: 600 })
  await expect(page.getByRole('button', { name: 'Approve setup hooks' })).toBeVisible()
  expect(await page.evaluate(() => {
    const approval = Array.from(document.querySelectorAll('button')).find(button => button.textContent === 'Approve setup hooks')!
    return approval.getBoundingClientRect().bottom <= document.querySelector('[role="textbox"][contenteditable="true"]')!.getBoundingClientRect().top
  }), 'setup approval must be reachable above the composer at minimum size').toBe(true)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/native-worktrunk-setup-minimum.png' })
  await page.getByRole('button', { name: 'Approve setup hooks' }).click()
  await expect(review).toHaveCount(0)
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveText('Improve search in a separate worktree')
  expect(await page.evaluate(() => (window as any).__nativeWorktrunkRequests.some((request: any) => request.method === 'turn/start'))).toBe(false)
  await page.getByRole('button', { name: 'Send message' }).click()
  await page.getByRole('button', { name: 'Chat options' }).click()
  await expect(page.getByLabel('Session worktree')).toContainText('codex/brr-session')
  await expect(page.getByRole('alert')).toHaveCount(0)
})
