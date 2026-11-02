import { test, expect } from '@playwright/test'

test('saved worktrees display their location and new chats use the original project', async ({ page }) => {
  await page.addInitScript(() => {
    const project = '/workspace/atlas'
    const saved = {
      id: 'saved', preview: 'Isolate the search redesign', cwd: '/workspace/atlas.search', updatedAt: 1,
      worktree: { projectPath: project, path: '/workspace/atlas.search', cwd: '/workspace/atlas.search', branch: 'codex/brr-search' },
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
        if (command !== 'engine_request') throw new Error(`Unexpected command ${command}`)
        const { method, params } = args
        requests.push(args)
        if (method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (method === 'model/list') return { data: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'Sol', isDefault: true, supportedReasoningEfforts: [{ reasoningEffort: 'medium' }], defaultReasoningEffort: 'medium' }] }
        if (method === 'thread/list') return { data: [saved], nextCursor: null }
        if (method === 'thread/resume') return { thread: saved }
        if (method === 'brr/worktree/inspect') return { commands: [] }
        if (method === 'thread/start') return { thread: { id: 'new', preview: '', cwd: '/workspace/atlas.new', updatedAt: 2,
          worktree: { projectPath: params.cwd, path: '/workspace/atlas.new', cwd: '/workspace/atlas.new', branch: 'codex/brr-new' } } }
        if (method === 'turn/start') return { turn: { id: 'new-turn' } }
        throw new Error(`Unexpected request ${method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  })
  await page.goto('/')
  await page.getByRole('button', { name: 'Isolate the search redesign' }).click()
  await expect(page.getByLabel('Session worktree')).toHaveText('Worktree · codex/brr-search')
  await expect(page.getByLabel('Session worktree')).toHaveAttribute('title', '/workspace/atlas.search')
  await expect(page.getByTitle('/workspace/atlas', { exact: true })).toBeVisible()
  await page.screenshot({ path: 'projects/desktop-agent/evidence/worktrees-desktop.png' })
  await page.setViewportSize({ width: 800, height: 600 })
  await expect(page.getByLabel('Session worktree')).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/worktrees-minimum.png' })
  await page.getByRole('button', { name: 'New conversation' }).click()
  await expect(page.getByText('New chats use a separate worktree.')).toBeVisible()
  await page.getByRole('textbox', { name: 'Message' }).fill('A separate task')
  await page.getByRole('button', { name: 'Send message' }).click()
  await expect(page.getByLabel('Session worktree')).toHaveText('Worktree · codex/brr-new')
  const start = await page.evaluate(() => (window as any).__worktreeRequests.find((request: any) => request.method === 'thread/start'))
  expect(start.params.cwd).toBe('/workspace/atlas')
  expect(start.params.sandbox).toBe('workspace-write')
  await expect(page.getByRole('alert')).toHaveCount(0)
})
