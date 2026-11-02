import { test, expect } from '@playwright/test'

test('reviews native project setup before creating a worktree or sending a turn', async ({ page }) => {
  await page.addInitScript(() => {
    const browser = window as any
    const requests: any[] = []
    let approved = false
    browser.__nativeWorktrunkRequests = requests
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'plugin:dialog|open') return '/workspace/atlas'
        if (command !== 'engine_request') throw new Error(`Unexpected command ${command}`)
        requests.push(args)
        if (args.method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (args.method === 'model/list') return { data: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'Sol', isDefault: true, supportedReasoningEfforts: [], defaultReasoningEffort: 'medium' }] }
        if (args.method === 'thread/list') return { data: [], nextCursor: null }
        if (args.method === 'brr/worktree/inspect') return { commands: [{ phase: 'pre-start', name: 'Install dependencies', template: 'pnpm install --frozen-lockfile', approved }] }
        if (args.method === 'brr/worktree/approve') { approved = true; return { approved: true } }
        if (args.method === 'thread/start') return { thread: { id: 'new', cwd: '/workspace/atlas.session', preview: '', updatedAt: 1,
          worktree: { projectPath: '/workspace/atlas', path: '/workspace/atlas.session', cwd: '/workspace/atlas.session', branch: 'codex/brr-session' } } }
        if (args.method === 'turn/start') return { turn: { id: 'turn-1' } }
        throw new Error(`Unexpected request ${args.method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  })
  await page.goto('/')
  await page.getByRole('button', { name: 'Choose project folder' }).click()
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
    return approval.getBoundingClientRect().bottom <= document.querySelector('textarea')!.getBoundingClientRect().top
  }), 'setup approval must be reachable above the composer at minimum size').toBe(true)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/native-worktrunk-setup-minimum.png' })
  await page.getByRole('button', { name: 'Approve setup hooks' }).click()
  await expect(review).toHaveCount(0)
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Improve search in a separate worktree')
  expect(await page.evaluate(() => (window as any).__nativeWorktrunkRequests.some((request: any) => request.method === 'turn/start'))).toBe(false)
  await page.getByRole('button', { name: 'Send message' }).click()
  await expect(page.getByLabel('Session worktree')).toContainText('codex/brr-session')
  await expect(page.getByRole('alert')).toHaveCount(0)
})
