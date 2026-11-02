import { test, expect, type Page } from '@playwright/test'

async function connectSidebarFixture(page: Page, manyChats = false) {
  await page.addInitScript(({ many }) => {
    const browser = window as any
    const threads = [
      { id: 'brr-ui', name: 'Match the desktop navigation', preview: 'Match the desktop navigation', cwd: '/workspace/brr.ui', updatedAt: 3,
        worktree: { projectPath: '/workspace/brr', cwd: '/workspace/brr.ui', path: '/workspace/brr.ui', branch: 'codex/navigation' } },
      { id: 'brr-agents', name: 'Research and design agents', preview: 'Research and design agents', cwd: '/workspace/brr.agents', updatedAt: 2,
        worktree: { projectPath: '/workspace/brr', cwd: '/workspace/brr.agents', path: '/workspace/brr.agents', branch: 'codex/agents' } },
      { id: 'atlas', name: 'Review the release checklist', preview: 'Review the release checklist', cwd: '/workspace/atlas', updatedAt: 1 },
      ...(many ? Array.from({ length: 40 }, (_, index) => ({ id: `older-${index}`, name: `Earlier conversation ${index + 1}`, preview: `Earlier conversation ${index + 1}`, cwd: '', updatedAt: 0 })) : []),
    ]
    browser.__sidebarRequests = []
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'agents_request') return args.method === 'agents.settings' ? { maxBackground: 4, maxPerAgent: 2 } : { data: [], nextCursor: null, runtimeRequests: [] }
        if (command !== 'engine_request') throw new Error(`Unexpected command ${command}`)
        browser.__sidebarRequests.push(args)
        if (args.method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (args.method === 'model/list') return { data: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT-6.1-Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'medium', description: 'Balanced' }] }] }
        if (args.method === 'thread/list') return { data: threads, nextCursor: null }
        if (args.method === 'thread/resume') return { thread: { ...threads.find(thread => thread.id === args.params.threadId), turns: [{ id: 'saved-turn', status: 'completed', items: [
          { id: 'user', type: 'userMessage', content: [{ type: 'text', text: 'Match the desktop navigation.' }] },
          { id: 'assistant', type: 'agentMessage', text: 'The icon rail stays visible. Expand the sidebar to browse your projects and recent chats.' },
        ] }] } }
        throw new Error(`Unexpected request ${args.method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  }, { many: manyChats })
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'Match the desktop navigation', exact: true })).toBeVisible()
}

test('fixed rail survives collapse, moves chat beside it, and restores the saved layout', async ({ page }) => {
  await connectSidebarFixture(page)
  const rail = page.getByRole('navigation', { name: 'Main rail' })
  const sidebar = page.getByRole('complementary', { name: 'Main sidebar' })
  const message = page.getByRole('textbox', { name: 'Message' })
  await page.getByRole('button', { name: 'Match the desktop navigation', exact: true }).click()
  await message.fill('Keep this draft and conversation')
  await expect(page.getByText('The icon rail stays visible.', { exact: false })).toBeVisible()
  await page.getByRole('button', { name: 'Toggle project brr', exact: true }).click()
  const children = page.locator('.project-threads .project-thread')
  await expect(children).toHaveCount(2)
  const firstChild = (await children.nth(0).boundingBox())!
  const nextChild = (await children.nth(1).boundingBox())!
  expect(nextChild.y).toBeGreaterThanOrEqual(firstChild.y + firstChild.height)
  expect(nextChild.width).toBe(firstChild.width)
  await expect(children.nth(0)).toHaveAttribute('aria-current', 'page')
  const online = (await page.locator('.rail-status').boundingBox())!
  const home = (await page.getByRole('button', { name: 'Home', exact: true }).boundingBox())!
  expect(online.x).toBeGreaterThanOrEqual(home.x)
  expect(online.x + online.width).toBeLessThanOrEqual(home.x + home.width)

  const railBefore = (await rail.boundingBox())!
  const sidebarBefore = (await sidebar.boundingBox())!
  const mainBefore = (await page.getByRole('main').boundingBox())!
  expect(railBefore.width).toBe(52)
  expect(sidebarBefore.x).toBe(railBefore.x + railBefore.width)
  expect(mainBefore.x).toBe(sidebarBefore.x + sidebarBefore.width)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/desktop-expanded.png' })

  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  await expect(sidebar).toBeHidden()
  await expect(rail).toBeVisible()
  expect(await rail.boundingBox()).toEqual(railBefore)
  const mainAfter = (await page.getByRole('main').boundingBox())!
  expect(mainAfter.x).toBe(railBefore.x + railBefore.width)
  expect(mainAfter.width - mainBefore.width).toBe(sidebarBefore.width)
  await expect(message).toHaveValue('Keep this draft and conversation')
  await expect(page.getByText('The icon rail stays visible.', { exact: false })).toBeVisible()
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/desktop-collapsed.png' })

  await page.reload()
  await expect(sidebar).toBeHidden()
  await expect(rail).toBeVisible()
  await page.getByRole('button', { name: 'Home', exact: true }).click()
  await expect(sidebar).toBeHidden()
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click()
  await expect(sidebar).toBeVisible()
  await page.reload()
  await expect(sidebar).toBeVisible()
})

test('search stays in the sidebar and fixed rail controls remain usable while collapsed', async ({ page }) => {
  await connectSidebarFixture(page)
  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click()
  await page.getByRole('button', { name: 'Search chats', exact: true }).click()
  const search = page.getByRole('textbox', { name: 'Search chats' })
  await expect(search).toBeFocused()
  await search.fill('release')
  await expect(page.getByRole('button', { name: 'Review the release checklist', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Match the desktop navigation', exact: true })).toBeHidden()
  await page.getByRole('textbox', { name: 'Message' }).focus()
  await page.getByRole('button', { name: 'Search chats', exact: true }).click()
  await expect(search).toBeFocused()
  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  await page.getByRole('button', { name: 'Settings', exact: true }).click()
  await expect(page.getByRole('radio', { name: 'Graphite' })).toBeChecked()
  await page.getByRole('button', { name: 'Close settings' }).click()
  await expect(page.getByRole('button', { name: 'Settings', exact: true })).toBeFocused()
})

test('sidebar scroll and composer fit independently at the minimum native window', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 600 })
  await connectSidebarFixture(page, true)
  const rail = page.getByRole('navigation', { name: 'Main rail' })
  const railBefore = await rail.boundingBox()
  const composerBefore = await page.locator('.composer-area').boundingBox()
  await page.locator('.sidebar-scroll').evaluate(element => { element.scrollTop = element.scrollHeight })
  expect(await rail.boundingBox()).toEqual(railBefore)
  expect(await page.locator('.composer-area').boundingBox()).toEqual(composerBefore)
  await expect(page.getByRole('button', { name: 'Earlier conversation 40', exact: true })).toBeInViewport()
  for (const name of ['Settings', 'Home', 'Send message']) {
    await expect(page.getByRole('button', { name, exact: true })).toBeInViewport()
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  const composer = (await page.locator('.composer-area').boundingBox())!
  const send = (await page.getByRole('button', { name: 'Send message' }).boundingBox())!
  expect(send.x + send.width).toBeLessThanOrEqual(composer.x + composer.width)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/minimum-expanded.png' })
  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/minimum-collapsed.png' })
  await page.getByRole('button', { name: 'Profile', exact: true }).click()
  await expect(page.getByRole('complementary', { name: 'Main sidebar' })).toBeHidden()
  await expect(page.getByRole('dialog', { name: 'User profile' })).toBeFocused()
  await expect(page.getByRole('button', { name: 'Sign out', exact: true })).toBeInViewport()
})

test('five fixed rail buttons retain their positions and show hover and keyboard tooltips', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 600 })
  await connectSidebarFixture(page)
  const rail = page.getByRole('navigation', { name: 'Main rail' })
  const names = ['Home', 'Agents', 'Plugins', 'Profile', 'Settings']
  await expect(rail.getByRole('button')).toHaveCount(5)
  expect(await rail.getByRole('button').evaluateAll(buttons => buttons.map(button => button.getAttribute('aria-label')))).toEqual(names)
  const boxes = await Promise.all(names.map(name => rail.getByRole('button', { name, exact: true }).boundingBox()))
  expect(boxes[3]!.y).toBeGreaterThan(450)
  expect(boxes[4]!.y).toBeGreaterThan(boxes[3]!.y + boxes[3]!.height)
  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  for (let index = 0; index < names.length; index += 1) {
    const button = rail.getByRole('button', { name: names[index], exact: true })
    await expect(button).toBeInViewport()
    expect(await button.boundingBox()).toEqual(boxes[index])
    await expect(button).not.toHaveAttribute('aria-expanded')
    await button.hover()
    await expect(page.getByRole('tooltip', { name: names[index], exact: true })).toBeVisible()
    await page.mouse.move(700, 30)
    await button.focus()
    await expect(page.getByRole('tooltip', { name: names[index], exact: true })).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(page.getByRole('tooltip', { name: names[index], exact: true })).toBeHidden()
  }
  await page.getByRole('textbox', { name: 'Message' }).fill('Preserve this draft')
  await rail.getByRole('button', { name: 'Agents', exact: true }).click()
  await expect(page.getByRole('region', { name: 'Agents', exact: true })).toBeVisible()
  const mainBounds = await page.getByRole('main').evaluate(element => ({ x: element.getBoundingClientRect().x + element.clientLeft, width: element.clientWidth }))
  const agentsBounds = (await page.getByRole('region', { name: 'Agents', exact: true }).boundingBox())!
  expect(agentsBounds.x).toBe(mainBounds.x)
  expect(agentsBounds.width).toBe(mainBounds.width)
  await expect(page.getByRole('complementary', { name: 'Agent list' })).toBeHidden()
  await expect(page.getByRole('button', { name: 'Expand sidebar', exact: true })).toHaveAttribute('aria-controls', 'agents-sidebar')
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click()
  const agentList = page.getByRole('complementary', { name: 'Agent list' })
  await expect(agentList).toBeVisible()
  await expect(page.getByRole('complementary', { name: 'Main sidebar' })).toBeHidden()
  const menuWidth = (await agentList.boundingBox())!.width
  const detailWidth = (await page.locator('.agents-detail').boundingBox())!.width
  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  await expect(agentList).toBeHidden()
  expect(Math.abs((await page.locator('.agents-detail').boundingBox())!.width - detailWidth - menuWidth)).toBeLessThanOrEqual(1)
  await rail.getByRole('button', { name: 'Agents', exact: true }).click()
  await expect(page.getByRole('region', { name: 'Agents', exact: true })).toBeVisible()
  await rail.getByRole('button', { name: 'Home', exact: true }).click()
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Preserve this draft')
  await expect(page.getByRole('complementary', { name: 'Main sidebar' })).toBeHidden()
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click()
  await rail.getByRole('button', { name: 'Settings', exact: true }).hover()
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/fixed-rail-tooltips.png' })
})

test('appearance colours only the fixed rail while menus stay neutral across sections and collapse', async ({ page }) => {
  await connectSidebarFixture(page)
  const rail = page.getByRole('navigation', { name: 'Main rail' })
  const homeMenu = page.getByRole('complementary', { name: 'Main sidebar' })
  const menuBackground = await homeMenu.evaluate(element => getComputedStyle(element).backgroundColor)
  const chatBackground = await page.getByRole('main').evaluate(element => getComputedStyle(element).backgroundColor)
  await page.getByRole('textbox', { name: 'Message' }).fill('Keep the rail colour here')
  await page.getByRole('button', { name: 'Settings', exact: true }).click()
  await expect(page.getByRole('group', { name: 'Rail color' })).toBeVisible()
  for (const name of ['Graphite', 'Lime', 'Yellow', 'Orange', 'Red', 'Cyan', 'Blue', 'Violet', 'Pink']) {
    const option = page.getByRole('radio', { name, exact: true })
    await page.locator('.appearance-color-option').filter({ has: option }).click()
    await expect(option).toBeChecked()
    const swatch = await page.locator('.appearance-color-option.selected .appearance-color-swatch').evaluate(element => getComputedStyle(element).backgroundColor)
    expect(await rail.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(swatch)
    expect(await homeMenu.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(menuBackground)
    expect(await page.getByRole('main').evaluate(element => getComputedStyle(element).backgroundColor)).toBe(chatBackground)
    await expect.poll(async () => Math.min(...await rail.evaluate(element => {
      const context = document.createElement('canvas').getContext('2d')!
      const luminance = (color: string) => {
        context.fillStyle = color
        context.fillRect(0, 0, 1, 1)
        const [r, g, b] = Array.from(context.getImageData(0, 0, 1, 1).data).slice(0, 3).map(value => {
          const channel = value / 255
          return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
        })
        return r * 0.2126 + g * 0.7152 + b * 0.0722
      }
      const foreground = luminance(getComputedStyle(element).color)
      return [getComputedStyle(element).backgroundColor, getComputedStyle(element.querySelector('[aria-current="page"]')!).backgroundColor].map(color => {
        const background = luminance(color)
        return (Math.max(foreground, background) + 0.05) / (Math.min(foreground, background) + 0.05)
      })
    })), { message: `${name} rail icon contrast after the selection transition` }).toBeGreaterThanOrEqual(4.5)
  }
  await page.locator('.appearance-color-option').filter({ has: page.getByRole('radio', { name: 'Lime', exact: true }) }).click()
  await page.getByRole('button', { name: 'Close settings' }).click()
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep the rail colour here')
  const railBackground = await rail.evaluate(element => getComputedStyle(element).backgroundColor)
  expect(railBackground).not.toBe(menuBackground)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/colour-rail-expanded.png' })
  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  expect(await rail.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(railBackground)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/colour-rail-collapsed.png' })
  await page.getByRole('button', { name: 'Agents', exact: true }).click()
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click()
  expect(await page.getByRole('complementary', { name: 'Agent list' }).evaluate(element => getComputedStyle(element).backgroundColor)).toBe(menuBackground)
  expect(await rail.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(railBackground)
  await page.reload()
  expect(await rail.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(railBackground)
  expect(await homeMenu.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(menuBackground)
})
