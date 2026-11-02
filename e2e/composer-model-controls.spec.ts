import { test, expect, type Page } from '@playwright/test'

async function connectDesktopFixture(page: Page) {
  await page.addInitScript(() => {
    const browser = window as any
    browser.__composerRequests = []
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'plugin:dialog|open') return '/workspace/atlas'
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
        if (args.method === 'brr/worktree/inspect') return { commands: [] }
        if (args.method === 'thread/start') return { thread: { id: 'new', cwd: '/workspace/atlas.session', preview: '', updatedAt: 1 } }
        if (args.method === 'turn/start') return { turn: { id: 'turn-1' } }
        if (args.method === 'turn/interrupt') return {}
        throw new Error(`Unexpected request ${args.method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  })
  await page.goto('/')
}

async function expectComposerControlsFit(page: Page, includeStop = false) {
  const composer = page.locator('.composer-area')
  const send = page.getByRole('button', { name: 'Send message' })
  const sendBox = (await send.boundingBox())!
  const composerBox = (await composer.boundingBox())!
  for (const control of [page.getByRole('combobox', { name: 'Model', exact: true }), page.getByRole('combobox', { name: 'Reasoning effort' }), send,
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
  await expect(effort).toHaveValue('medium')
  await page.getByRole('button', { name: 'Choose project folder' }).click()
  const message = page.getByRole('textbox', { name: 'Message' })
  await message.fill('Keep this draft while changing models')
  await page.screenshot({ path: 'projects/desktop-agent/evidence/composer-model-controls.png' })
  await page.locator('.composer-area').screenshot({ path: 'projects/desktop-agent/evidence/composer-model-controls-detail.png' })
  await page.setViewportSize({ width: 800, height: 600 })
  await expectComposerControlsFit(page)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/composer-model-controls-minimum.png' })

  await model.selectOption('sol')
  await expect(message).toHaveValue('Keep this draft while changing models')
  await expect(effort).toHaveValue('high')
  await effort.selectOption('low')
  await expectComposerControlsFit(page)
  await page.getByRole('button', { name: 'Send message' }).click()
  await expect(page.getByRole('button', { name: 'Stop', exact: true })).toBeVisible()
  expect(await page.evaluate(() => (window as any).__composerRequests.find((request: any) => request.method === 'turn/start')?.params)).toMatchObject({
    model: 'gpt-6.1-sol', effort: 'low', input: [{ type: 'text', text: 'Keep this draft while changing models' }],
  })
  await expectComposerControlsFit(page, true)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/composer-model-controls-running.png' })
  await expect(page.getByRole('alert')).toHaveCount(0)
})
