import { test, expect, type Page } from '@playwright/test'

async function connectDesktopFixture(page: Page, longNames = false) {
  await page.addInitScript((longNames: boolean) => {
    const browser = window as any
    const project = {id: 'project-atlas', name: longNames ? 'Atlas workspace with an especially long project name' : 'atlas', folders: ['/workspace/atlas'], primaryFolder: '/workspace/atlas', revision: 1, model: null, effort: null, permission: null, hidden: false}
    browser.__composerRequests = []
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'plugin:dialog|open') return '/workspace/atlas'
        if (command === 'agents_request') {
          if (args.method === 'questions.list') return { data: [] }
          if (args.method === 'agents.list') return { data: longNames ? [{ id: 'release', name: 'Atlas release assistant with an especially long name', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'high', revision: 1, createdAt: 1, updatedAt: 1 }] : [] }
          if (['agents.decisions', 'agents.drafts', 'tasks.list'].includes(args.method)) return { data: [], nextCursor: null }
          if (args.method === 'agents.workspace.inspect') return { git: true, commands: [] }
          if (args.method === 'agents.open') return { thread: { id: 'named', cwd: '/workspace/atlas.session', agentId: 'release', preview: '', updatedAt: 1 } }
          throw new Error(`Unexpected agent request ${args.method}`)
        }
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
        browser.__composerRequests.push(args)
        if (args.method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (args.method === 'model/list') return { data: [
          { id: 'astra', model: 'gpt-6-astra', displayName: 'GPT-6-Astra', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [
            { reasoningEffort: 'low', description: 'Fast' }, { reasoningEffort: 'medium', description: 'Balanced' }, { reasoningEffort: 'high', description: 'Thorough' },
          ] },
          { id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT-6.1-Sol with an especially long model name', isDefault: false, defaultReasoningEffort: 'high', supportedReasoningEfforts: [
            { reasoningEffort: 'low', description: 'Fast' }, { reasoningEffort: 'high', description: 'Thorough' },
          ] },
        ] }
        if (args.method === 'thread/list') return { data: [], nextCursor: null }
        if (args.method === 'brr/project/inspect') return { commands: [] }
        if (args.method === 'thread/start') return { thread: { id: 'new', projectId: args.params.projectId, cwd: '/workspace/atlas.session', preview: '', updatedAt: 1, workspace: {cwd: '/workspace/atlas.session', roots: ['/workspace/atlas.session'], folders: [{source: '/workspace/atlas', path: '/workspace/atlas.session'}], worktrees: [], scopeProjectId: project.id, projectRevision: project.revision} } }
        if (args.method === 'turn/start') return { turn: { id: 'turn-1' } }
        if (args.method === 'turn/interrupt') return {}
        throw new Error(`Unexpected request ${args.method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  }, longNames)
  await page.goto('/')
}

async function expectComposerControlsFit(page: Page, includeStop = false) {
  const composer = page.locator('.composer-area')
  const send = page.getByRole('button', { name: includeStop ? 'Queue message' : 'Send message' })
  const sendBox = (await send.boundingBox())!
  const composerBox = (await composer.boundingBox())!
  for (const control of [page.getByRole('combobox', { name: 'Model', exact: true }), page.getByRole('button', { name: 'Chat options', exact: true }), page.getByRole('combobox', { name: 'Project' }), page.getByRole('button', { name: 'Plan mode' }), send,
    ...(includeStop ? [page.getByRole('button', { name: 'Stop', exact: true })] : [])]) {
    await expect(control).toBeVisible()
    const box = (await control.boundingBox())!
    expect(box.x).toBeGreaterThanOrEqual(composerBox.x)
    expect(box.x + box.width).toBeLessThanOrEqual(composerBox.x + composerBox.width)
    expect(Math.abs(box.y + box.height / 2 - (sendBox.y + sendBox.height / 2))).toBeLessThan(3)
    expect(box.y + box.height).toBeLessThanOrEqual(600)
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
}

test('composer model controls preserve the draft, send the selected settings, and fit at minimum size', async ({ page }) => {
  await connectDesktopFixture(page)
  const model = page.getByRole('combobox', { name: 'Model', exact: true })
  const effort = page.getByRole('combobox', { name: 'Reasoning effort' })
  await expect(model).toHaveValue('astra')
  expect((await model.boundingBox())!.width, 'short selected model must stay compact even when another option has a long name').toBeLessThan(150)
  await expect(page.locator('header').getByRole('combobox')).toHaveCount(0)
  await expect(page.locator('.composer-area').getByRole('combobox', { name: 'Model', exact: true })).toBeVisible()
  await expect(effort).not.toBeVisible()
  await expect(page.getByRole('combobox', { name: 'Chat identity' })).not.toBeVisible()
  await page.getByRole('combobox', { name: 'Project' }).selectOption('project-atlas')
  const message = page.getByRole('textbox', { name: 'Message' })
  await message.fill('Keep this draft while changing models')
  await page.screenshot({ path: 'projects/desktop-agent/evidence/composer-model-controls.png' })
  await page.locator('.composer-area').screenshot({ path: 'projects/desktop-agent/evidence/composer-model-controls-detail.png' })
  await page.setViewportSize({ width: 800, height: 600 })
  await expectComposerControlsFit(page)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/composer-model-controls-minimum.png' })

  await model.selectOption('sol')
  await expect(message).toHaveText('Keep this draft while changing models')
  await page.getByRole('button', { name: 'Chat options', exact: true }).click()
  await expect(effort).toHaveValue('high')
  await effort.selectOption('low')
  const optionsBox = (await page.getByRole('dialog', { name: 'Chat options' }).boundingBox())!
  expect(optionsBox.y).toBeGreaterThanOrEqual(0)
  expect(optionsBox.y + optionsBox.height).toBeLessThanOrEqual(600)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/composer-options-minimum.png' })
  await page.keyboard.press('Escape')
  await expect(page.getByRole('button', { name: 'Chat options', exact: true })).toBeFocused()
  await expect(message).toHaveText('Keep this draft while changing models')
  await expectComposerControlsFit(page)
  await page.getByRole('button', { name: 'Send message' }).click()
  await expect(page.getByRole('button', { name: 'Stop', exact: true })).toBeVisible()
  expect(await page.evaluate(() => (window as any).__chatRequests.find((request: any) => request.method === 'foreground.submit')?.params)).toMatchObject({
    settings: {model: 'gpt-6.1-sol', effort: 'low'}, document: {text: 'Keep this draft while changing models'},
  })
  await expectComposerControlsFit(page, true)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/composer-model-controls-running.png' })
  await page.getByRole('button', { name: 'Chat options', exact: true }).click()
  await expect(page.getByRole('combobox', { name: 'Chat identity' })).toBeDisabled()
  await expect(page.getByText("Access is set by this chat's saved policy.")).toBeVisible()
  await page.getByRole('textbox', { name: 'Message' }).click({ position: { x: 10, y: 10 } })
  await expect(page.getByRole('dialog', { name: 'Chat options' })).not.toBeVisible()
  await expect(page.getByRole('alert')).toHaveCount(0)
})


test('long project, agent and model names keep Send and Stop reachable at the minimum size', async ({ page }) => {
  await connectDesktopFixture(page, true)
  await page.setViewportSize({ width: 800, height: 600 })
  await page.getByRole('combobox', { name: 'Project' }).selectOption('project-atlas')
  await page.getByRole('button', { name: 'Chat options', exact: true }).click()
  await page.getByRole('combobox', { name: 'Chat identity' }).selectOption('release')
  await page.getByRole('checkbox', { name: 'Read only' }).check()
  await page.keyboard.press('Escape')
  await expect(page.locator('.composer-options-summary')).toContainText('Atlas release assistant')
  expect((await page.locator('.composer-options-summary').boundingBox())!.width).toBeGreaterThan(30)
  await page.getByRole('textbox', { name: 'Message' }).fill('Review the pending changes')
  await expectComposerControlsFit(page)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/composer-long-names-minimum.png' })
  await page.getByRole('button', { name: 'Send message' }).click()
  await expect(page.getByRole('button', { name: 'Stop', exact: true })).toBeVisible()
  await expectComposerControlsFit(page, true)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/composer-named-running-minimum.png' })
})
