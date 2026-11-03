import { test, expect, type Page } from '@playwright/test'

async function connectSidebarFixture(page: Page, manyChats = false) {
  await page.addInitScript(({ many }) => {
    const browser = window as any
    let threads = [
      { id: 'brr-ui', projectId: 'project-brr', name: 'Match the desktop navigation', preview: 'Match the desktop navigation', cwd: '/workspace/brr.ui', workspace: {cwd: '/workspace/brr.ui', roots: ['/workspace/brr.ui'], folders: [{source: '/workspace/brr', path: '/workspace/brr.ui'}], worktrees: [{projectPath: '/workspace/brr', path: '/workspace/brr.ui', cwd: '/workspace/brr.ui', branch: 'codex/navigation'}], scopeProjectId: 'project-brr', projectRevision: 1}, updatedAt: 3,
        worktree: { projectPath: '/workspace/brr', cwd: '/workspace/brr.ui', path: '/workspace/brr.ui', branch: 'codex/navigation' } },
      { id: 'brr-agents', projectId: 'project-brr', name: 'Research and design agents', preview: 'Research and design agents', cwd: '/workspace/brr.agents', workspace: {cwd: '/workspace/brr.agents', roots: ['/workspace/brr.agents'], folders: [{source: '/workspace/brr', path: '/workspace/brr.agents'}], worktrees: [{projectPath: '/workspace/brr', path: '/workspace/brr.agents', cwd: '/workspace/brr.agents', branch: 'codex/agents'}], scopeProjectId: 'project-brr', projectRevision: 1}, updatedAt: 2,
        worktree: { projectPath: '/workspace/brr', cwd: '/workspace/brr.agents', path: '/workspace/brr.agents', branch: 'codex/agents' } },
      { id: 'atlas', projectId: 'project-atlas', name: 'Review the release checklist', preview: 'Review the release checklist', cwd: '/workspace/atlas', workspace: {cwd: '/workspace/atlas', roots: ['/workspace/atlas'], folders: [{source: '/workspace/atlas', path: '/workspace/atlas'}], worktrees: [], scopeProjectId: 'project-atlas', projectRevision: 1}, updatedAt: 1 },
      ...(many ? Array.from({ length: 40 }, (_, index) => ({ id: `older-${index}`, projectId: null, name: `Earlier conversation ${index + 1}`, preview: `Earlier conversation ${index + 1}`, cwd: '', workspace: {cwd: '', roots: [], folders: [], worktrees: [], scopeProjectId: null, projectRevision: null}, updatedAt: 0 })) : []),
    ]
    const archivedIds = new Set<string>(JSON.parse(localStorage.getItem('fixture.archivedChats') ?? '[]'))
    threads = threads.filter(thread => !archivedIds.has(thread.id))
    const projects = JSON.parse(localStorage.getItem('fixture.projects') ?? '[]') as any[]
    if (!projects.length) projects.push({id: 'project-brr', name: 'brr', folders: ['/workspace/brr'], primaryFolder: '/workspace/brr', revision: 1, model: null, effort: null, permission: null, hidden: false}, {id: 'project-atlas', name: 'atlas', folders: ['/workspace/atlas'], primaryFolder: '/workspace/atlas', revision: 1, model: null, effort: null, permission: null, hidden: false})
    browser.__sidebarRequests = []
    browser.__sidebarNativeRequests = []
    browser.__failLogout = false
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'list_projects') return {data: projects}
        if (command === 'create_project') {
          const project = {id: `project-${projects.length + 1}`, revision: 1, ...args.project, hidden: false}
          projects.push(project); localStorage.setItem('fixture.projects', JSON.stringify(projects)); return project
        }
        if (command === 'save_project') {
          const project = {...args.project, hidden: false}
          const index = projects.findIndex(item => item.id === project.id)
          project.revision = (projects[index]?.revision ?? project.revision ?? 1) + 1
          if (index < 0) projects.push(project); else projects[index] = project
          localStorage.setItem('fixture.projects', JSON.stringify(projects))
          return project
        }
        if (command === 'delete_project') {
          const index = projects.findIndex(item => item.id === args.projectId)
          if (index >= 0) projects.splice(index, 1)
          localStorage.setItem('fixture.projects', JSON.stringify(projects)); return
        }
        if (command === 'move_chat_to_project') {
          const thread = threads.find(item => item.id === args.threadId)
          if (thread) thread.projectId = args.projectId
          return
        }
        if (command === 'reveal_project_folder') { browser.__sidebarNativeRequests.push({command, ...args}); return }
        if (command === 'plugin:dialog|open') return '/workspace/brr'
        if (command === 'archive_project_chats') {
          browser.__sidebarNativeRequests.push({command, ...args})
          const project = projects.find(item => item.id === args.projectId)
          const ids = threads.filter(thread => thread.projectId === args.projectId).map(thread => thread.id)
          ids.forEach(id => archivedIds.add(id))
          localStorage.setItem('fixture.archivedChats', JSON.stringify([...archivedIds]))
          threads = threads.filter(thread => !archivedIds.has(thread.id))
          return {archivedIds: ids, skipped: [], failed: []}
        }
        if (command === 'agents_request') return args.method === 'agents.settings' ? { maxBackground: 4, maxPerAgent: 2 } : args.method === 'questions.list' ? { data: [] } : { data: [], nextCursor: null, runtimeRequests: [] }
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
        if (args.method === 'account/logout' && browser.__failLogout) throw new Error('Fixture sign-out failed')
        browser.__sidebarRequests.push(args)
        if (args.method === 'plugin/list') return {marketplaces: [], marketplaceLoadErrors: []}
        if (args.method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (args.method === 'model/list') return { data: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT-6.1-Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'medium', description: 'Balanced' }, {reasoningEffort: 'high', description: 'More reasoning'}] }] }
        if (args.method === 'thread/list') return { data: threads, nextCursor: null }
        if (args.method === 'thread/resume') return { thread: { ...threads.find(thread => thread.id === args.params.threadId), turns: [{ id: 'saved-turn', status: 'completed', items: [
          { id: 'user', type: 'userMessage', content: [{ type: 'text', text: 'Match the desktop navigation.' }] },
          { id: 'assistant', type: 'agentMessage', text: 'The icon rail stays visible. Expand the sidebar to browse your projects and recent chats.\n\n```ts\nconst project = "Brr"\n```' },
        ] }] } }
        throw new Error(`Unexpected request ${args.method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  }, { many: manyChats })
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'Match the desktop navigation', exact: true })).toBeVisible()
}

test('uses the public site typography and surface family across the app', async ({ page }) => {
  await connectSidebarFixture(page)
  const home = page.getByRole('button', {name: 'Home', exact: true})
  const fontOf = (selector: string) => page.locator(selector).first().evaluate(element => getComputedStyle(element).fontFamily)
  expect(await page.evaluate(async () => { await document.fonts.load('400 16px Archivo'); return document.fonts.check('400 16px Archivo') })).toBe(true)
  for (const selector of ['body', '.sidebar', '.wordmark', '.thread-row', '.composer-area [role="textbox"]', '.composer-model-controls select']) {
    expect(await fontOf(selector)).toContain('Archivo')
  }
  const base = await page.locator('.main-panel').evaluate(element => getComputedStyle(element).backgroundColor)
  const sidebar = await page.locator('.sidebar').evaluate(element => getComputedStyle(element).backgroundColor)
  const raised = await page.locator('.composer-area').evaluate(element => getComputedStyle(element).backgroundColor)
  expect(base).not.toBe(sidebar)
  expect(sidebar).not.toBe(raised)
  await page.getByRole('button', {name: 'Match the desktop navigation', exact: true}).click()
  expect(await fontOf('.agent-message')).toContain('Archivo')
  expect(await fontOf('.agent-message code')).toContain('monospace')
  await page.screenshot({path: 'projects/desktop-agent/evidence/theme-cohesion/chat-desktop.png'})
  await page.getByRole('button', {name: 'Manage project brr', exact: true}).click()
  expect(await fontOf('.project-menu')).toContain('Archivo')
  await expect(page.locator('.project-menu')).toHaveCSS('background-color', raised)
  await page.getByRole('menuitem', {name: 'Project settings'}).click()
  const editProject = page.getByRole('dialog', {name: 'Edit project'})
  await expect(editProject).toBeVisible()
  expect(await fontOf('.project-dialog')).toContain('Archivo')
  expect(await fontOf('.project-dialog-form input')).toContain('Archivo')
  await editProject.locator('.project-dialog-defaults summary').click()
  expect(await fontOf('.project-dialog-defaults select')).toContain('Archivo')
  await editProject.getByRole('button', {name: 'Close project dialog'}).click()
  await home.click()
  await page.getByRole('button', {name: 'Profile', exact: true}).click()
  expect(await fontOf('.profile-page')).toContain('Archivo')
  await expect(page.locator('.main-panel')).toHaveCSS('background-color', base)
  await home.click()
  await page.getByRole('button', {name: 'Agents', exact: true}).click()
  expect(await fontOf('.agents-panel')).toContain('Archivo')
  await expect(page.locator('.agents-panel')).toHaveCSS('background-color', base)
  await expect(page.locator('.agents-list')).toHaveCSS('background-color', sidebar)
  await page.screenshot({path: 'projects/desktop-agent/evidence/theme-cohesion/agents-desktop.png'})
  await page.getByRole('button', {name: 'Plugins', exact: true}).click()
  expect(await fontOf('.extensions-panel')).toContain('Archivo')
  await expect(page.getByRole('alert')).toHaveCount(0)
  await expect(page.locator('.extensions-panel')).toHaveCSS('background-color', base)
  await expect(page.locator('.extensions-tabs')).toHaveCSS('background-color', sidebar)
  await page.screenshot({path: 'projects/desktop-agent/evidence/theme-cohesion/plugins-desktop.png'})
  await page.setViewportSize({width: 800, height: 600})
  await page.screenshot({path: 'projects/desktop-agent/evidence/theme-cohesion/plugins-minimum.png'})
  await home.click()
  await page.getByRole('textbox', {name: 'Message'}).fill('A draft stays readable across the shared theme.')
  await page.mouse.move(790, 580)
  await expect(page.getByRole('button', {name: 'Send message'})).toBeInViewport()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({path: 'projects/desktop-agent/evidence/theme-cohesion/chat-minimum.png'})
  await page.getByRole('button', {name: 'Collapse sidebar'}).click()
  await expect(page.getByRole('button', {name: 'Send message'})).toBeInViewport()
  await page.screenshot({path: 'projects/desktop-agent/evidence/theme-cohesion/chat-minimum-collapsed.png'})
})

test('project menus edit persistent defaults, reveal the original folder and archive before removal', async ({ page }) => {
  await page.setViewportSize({width: 800, height: 600})
  await connectSidebarFixture(page)
  const manage = () => page.getByRole('button', {name: 'Manage project brr', exact: true})
  await manage().click()
  const menu = page.getByRole('menu', {name: 'brr actions'})
  await expect(menu.getByRole('menuitem')).toHaveCount(6)
  await expect(menu).toBeInViewport()
  await page.screenshot({path: 'projects/desktop-agent/evidence/sidebar-rail/project-menu-minimum.png'})
  await menu.getByRole('menuitem', {name: /Reveal in|Open in file manager/}).click()
  expect(await page.evaluate(() => (window as any).__sidebarNativeRequests)).toContainEqual({command: 'reveal_project_folder', path: '/workspace/brr'})
  await page.getByRole('button', {name: 'Expand chats in brr', exact: true}).click()
  await expect(page.locator('.project-disclosure[data-project-id="project-brr"]')).toHaveAttribute('aria-expanded', 'true')
  await manage().click()
  await page.getByRole('menuitem', {name: 'Project settings'}).click()
  const projectSettings = page.getByRole('dialog', {name: 'Edit project'})
  await expect(projectSettings).toBeVisible()
  await expect(projectSettings.getByRole('textbox', {name: 'Project name'})).toBeFocused()
  await projectSettings.getByRole('textbox', {name: 'Project name'}).fill('Brr desktop')
  await projectSettings.locator('.project-dialog-defaults summary').click()
  await projectSettings.getByRole('combobox', {name: 'Default model'}).selectOption('gpt-6.1-sol')
  await projectSettings.getByRole('combobox', {name: 'Default reasoning'}).selectOption('high')
  await projectSettings.getByRole('combobox', {name: 'Default permissions'}).selectOption('read-only')
  await page.screenshot({path: 'projects/desktop-agent/evidence/sidebar-rail/project-settings-minimum.png'})
  await projectSettings.getByRole('button', {name: 'Save changes'}).click()
  await page.reload()
  const renamed = () => page.getByRole('button', {name: 'Manage project Brr desktop', exact: true})
  await expect(renamed()).toBeVisible()
  await page.getByRole('combobox', {name: 'Project'}).selectOption('project-brr')
  await page.getByRole('button', {name: 'Chat options'}).click()
  await expect(page.getByRole('checkbox', {name: 'Read only'})).toBeChecked()
  await expect(page.getByRole('combobox', {name: 'Reasoning effort'})).toHaveValue('high')
  await page.getByRole('textbox', {name: 'Message'}).fill('Preserve my draft')
  await renamed().click()
  await page.getByRole('menuitem', {name: 'Archive all chats'}).click()
  expect(await page.evaluate(() => (window as any).__sidebarNativeRequests)).not.toContainEqual({command: 'archive_project_chats', projectId: 'project-brr'})
  await page.getByRole('button', {name: 'Archive chats', exact: true}).click()
  await expect(page.getByText('2 chats archived.', {exact: true})).toBeVisible()
  await page.getByRole('button', {name: 'Done', exact: true}).click()
  await expect(renamed()).toBeVisible()
  await expect(page.getByRole('button', {name: 'Match the desktop navigation', exact: true})).toHaveCount(0)
  await expect(page.getByRole('textbox', {name: 'Message'})).toHaveText('Preserve my draft')
  await renamed().click()
  await page.getByRole('menuitem', {name: 'Delete project', exact: true}).click()
  await page.getByRole('button', {name: 'Delete project', exact: true}).click()
  await expect(renamed()).toHaveCount(0)
  await expect(page.getByRole('button', {name: 'New project', exact: true})).toBeVisible()
  await page.screenshot({path: 'projects/desktop-agent/evidence/sidebar-rail/project-removed-minimum.png'})
  await page.getByRole('button', {name: 'Chat options'}).click()
  await expect(page.getByRole('checkbox', {name: 'Read only'})).not.toBeChecked()
  await page.reload()
  await expect(page.getByRole('button', {name: 'Manage project atlas', exact: true})).toBeVisible()
  await expect(renamed()).toHaveCount(0)
})

test('fixed rail survives collapse, moves chat beside it, and restores the saved layout', async ({ page }) => {
  await connectSidebarFixture(page)
  const rail = page.getByRole('navigation', { name: 'Main rail' })
  const sidebar = page.getByRole('complementary', { name: 'Main sidebar' })
  const message = page.getByRole('textbox', { name: 'Message' })
  await page.getByRole('button', { name: 'Match the desktop navigation', exact: true }).click()
  await message.fill('Keep this draft and conversation')
  await expect(page.getByText('The icon rail stays visible.', { exact: false })).toBeVisible()
  await page.getByRole('button', { name: 'Expand chats in brr', exact: true }).click()
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
  await expect(message).toHaveText('Keep this draft and conversation')
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
  await expect(page.getByRole('region', { name: 'User profile' })).toBeFocused()
  await expect(page.getByRole('button', { name: 'Sign out', exact: true })).toBeInViewport()
  await page.getByRole('button', { name: 'Close profile', exact: true }).click()
  await expect(page.getByRole('complementary', { name: 'Main sidebar' })).toBeHidden()
  await expect(page.getByRole('button', { name: 'Expand sidebar', exact: true })).toBeVisible()
})

test('account actions stay on Profile instead of filling the Home sidebar', async ({ page }) => {
  await connectSidebarFixture(page)
  const sidebar = page.getByRole('complementary', { name: 'Main sidebar' })
  const profile = page.getByRole('button', { name: 'Profile', exact: true })
  await profile.click()
  const profilePage = page.getByRole('region', { name: 'User profile' })
  await expect(profilePage.getByText('developer@example.com', { exact: true })).toBeVisible()
  await expect(profilePage.getByRole('button', { name: 'Sign out', exact: true })).toBeVisible()
  await profilePage.getByRole('button', { name: 'Close profile', exact: true }).click()
  await expect(sidebar.getByRole('region', { name: 'Account controls' })).toHaveCount(0)
  await expect(sidebar.getByText('developer@example.com', { exact: true })).toHaveCount(0)
  await expect(sidebar.getByRole('button', { name: 'Sign out', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click()
  await expect(sidebar.getByRole('region', { name: 'Account controls' })).toHaveCount(0)
  await expect(sidebar.getByText('developer@example.com', { exact: true })).toHaveCount(0)
  await expect(sidebar.getByRole('button', { name: 'Sign out', exact: true })).toHaveCount(0)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/sidebar-without-account-footer.png' })
})

test('profile is a full workspace page and returns to the section and draft it replaced', async ({ page }) => {
  page.on('pageerror', error => console.error('Profile browser error:', error.message))
  await connectSidebarFixture(page)
  const rail = page.getByRole('navigation', { name: 'Main rail' })
  const profileButton = rail.getByRole('button', { name: 'Profile', exact: true })
  const profilePage = page.getByRole('region', { name: 'User profile' })
  const message = page.getByRole('textbox', { name: 'Message' })
  const toolbar = page.locator('.desktop-toolbar')

  await message.fill('Preserve this draft through Profile')
  await profileButton.click()
  await expect(profilePage).toBeVisible()
  await expect(profilePage).toBeFocused()
  await expect(profileButton).toHaveAttribute('aria-current', 'page')
  await expect(profileButton).not.toHaveAttribute('aria-haspopup')
  await expect(page.getByRole('button', { name: 'Collapse sidebar', exact: true })).toHaveCount(0)
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveCount(0)
  await expect(profilePage.getByText('developer@example.com', { exact: true })).toBeVisible()
  await expect(profilePage.getByRole('button', { name: 'Sign out', exact: true })).toBeInViewport()
  await expect(rail.getByRole('button', { name: 'Home', exact: true })).not.toHaveAttribute('aria-current')
  await expect(rail.getByRole('button', { name: 'Settings', exact: true })).not.toHaveAttribute('aria-current')

  const railBox = (await rail.boundingBox())!
  const pageBox = (await profilePage.boundingBox())!
  expect(pageBox.x).toBe(railBox.x + railBox.width)
  expect(pageBox.width).toBe(1280 - pageBox.x)
  expect(await toolbar.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(await rail.evaluate(element => getComputedStyle(element).backgroundColor))
  await profilePage.click({ position: { x: 700, y: 500 } })
  await expect(profilePage).toBeVisible()
  await profileButton.click()
  await expect(profilePage).toBeVisible()
  await expect(profilePage).toBeFocused()
  await page.mouse.move(pageBox.x + 350, pageBox.y + 120)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/profile-page-desktop.png' })

  await page.keyboard.press('Escape')
  await expect(profilePage).toHaveCount(0)
  await expect(profileButton).toBeFocused()
  await expect(message).toHaveText('Preserve this draft through Profile')
  await expect(page.getByRole('complementary', { name: 'Main sidebar' })).toBeVisible()

  await rail.getByRole('button', { name: 'Agents', exact: true }).click()
  await expect(page.getByRole('region', { name: 'Agents', exact: true })).toBeVisible()
  await profileButton.click()
  await expect(profilePage).toBeVisible()
  await profilePage.getByRole('button', { name: 'Close profile', exact: true }).click()
  await expect(profilePage).toHaveCount(0)
  await expect(page.getByRole('region', { name: 'Agents', exact: true })).toBeVisible()
  await expect(rail.getByRole('button', { name: 'Agents', exact: true })).toHaveAttribute('aria-current', 'page')
  await expect(rail.getByRole('button', { name: 'Profile', exact: true })).not.toHaveAttribute('aria-current')

  await page.setViewportSize({ width: 800, height: 600 })
  await profileButton.click()
  await expect(profilePage).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await expect(profilePage.getByRole('button', { name: 'Sign out', exact: true })).toBeInViewport()
  const minimumPageBox = (await profilePage.boundingBox())!
  await page.mouse.move(minimumPageBox.x + 300, minimumPageBox.y + 120)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/profile-page-minimum.png' })
})

test('Profile keeps sign-out failure feedback visible', async ({ page }) => {
  await connectSidebarFixture(page)
  const profile = page.getByRole('button', { name: 'Profile', exact: true })
  const profilePage = page.getByRole('region', { name: 'User profile' })
  await profile.click()
  await page.evaluate(() => { (window as any).__failLogout = true })
  await profilePage.getByRole('button', { name: 'Sign out', exact: true }).click()
  await expect(profilePage.getByRole('alert')).toHaveText('Error: Fixture sign-out failed')
  await expect(profilePage.getByRole('button', { name: 'Sign out', exact: true })).toBeVisible()
  await expect(profile).toHaveAttribute('aria-current', 'page')
})

test('six fixed rail buttons retain their positions and show hover and keyboard tooltips', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 600 })
  await connectSidebarFixture(page)
  const rail = page.getByRole('navigation', { name: 'Main rail' })
  const names = ['Home', 'Agents', 'Tickets', 'Plugins', 'Profile', 'Settings']
  await expect(rail.getByRole('button')).toHaveCount(6)
  expect(await rail.getByRole('button').evaluateAll(buttons => buttons.map(button => button.getAttribute('aria-label')))).toEqual(names)
  const boxes = await Promise.all(names.map(name => rail.getByRole('button', { name, exact: true }).boundingBox()))
  expect(boxes[3]!.y).toBeGreaterThan(boxes[2]!.y + boxes[2]!.height)
  expect(boxes[4]!.y).toBeGreaterThan(450)
  expect(boxes[5]!.y).toBeGreaterThan(boxes[4]!.y + boxes[4]!.height)
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
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveText('Preserve this draft')
  await expect(page.getByRole('complementary', { name: 'Main sidebar' })).toBeHidden()
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click()
  await rail.getByRole('button', { name: 'Settings', exact: true }).hover()
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/fixed-rail-tooltips.png' })
})

test('appearance colours the fixed rail and toolbar while menus stay neutral across sections and collapse', async ({ page }) => {
  await connectSidebarFixture(page)
  const rail = page.getByRole('navigation', { name: 'Main rail' })
  const toolbar = page.locator('.desktop-toolbar')
  const homeMenu = page.getByRole('complementary', { name: 'Main sidebar' })
  const menuBackground = await homeMenu.evaluate(element => getComputedStyle(element).backgroundColor)
  const chatBackground = await page.getByRole('main').evaluate(element => getComputedStyle(element).backgroundColor)
  await page.getByRole('textbox', { name: 'Message' }).fill('Keep the rail colour here')
  await page.getByRole('button', { name: 'Settings', exact: true }).click()
  await expect(page.getByRole('group', { name: 'Theme color' })).toBeVisible()
  for (const name of ['Graphite', 'Lime', 'Yellow', 'Orange', 'Red', 'Cyan', 'Blue', 'Violet', 'Pink']) {
    const option = page.getByRole('radio', { name, exact: true })
    await page.locator('.appearance-color-option').filter({ has: option }).click()
    await expect(option).toBeChecked()
    const swatch = await page.locator('.appearance-color-option.selected .appearance-color-swatch').evaluate(element => getComputedStyle(element).backgroundColor)
    expect(await rail.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(swatch)
    expect(await toolbar.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(swatch)
    const accent = await page.locator('.appearance-color-option.selected').evaluate(element => getComputedStyle(element).borderColor)
    expect(await page.locator('.rail-avatar').evaluate(element => getComputedStyle(element).backgroundColor)).toBe(accent)
    const accentContrast = await page.locator('.appearance-color-option.selected').evaluate(element => {
      const context = document.createElement('canvas').getContext('2d')!
      const luminance = (color: string) => {
        context.fillStyle = color; context.fillRect(0, 0, 1, 1)
        const [r, g, b] = Array.from(context.getImageData(0, 0, 1, 1).data).slice(0, 3).map(value => {
          const channel = value / 255
          return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
        })
        return r * .2126 + g * .7152 + b * .0722
      }
      const contrast = (a: string, b: string) => {
        const first = luminance(a), second = luminance(b)
        return (Math.max(first, second) + .05) / (Math.min(first, second) + .05)
      }
      const accent = getComputedStyle(element).borderColor
      const avatar = getComputedStyle(document.querySelector('.rail-avatar')!)
      return Math.min(
        contrast(accent, getComputedStyle(document.querySelector('.main-panel')!).backgroundColor),
        contrast(accent, getComputedStyle(document.querySelector('.appearance-color-option:not(.selected)')!).backgroundColor),
        contrast(avatar.color, avatar.backgroundColor),
      )
    })
    expect(accentContrast, `${name} accent and avatar contrast`).toBeGreaterThanOrEqual(4.5)
    expect(await toolbar.evaluate(element => getComputedStyle(element).color)).toBe(await rail.evaluate(element => getComputedStyle(element).color))
    expect(await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).evaluate(element => getComputedStyle(element).color)).toBe(await rail.evaluate(element => getComputedStyle(element).color))
    const back = page.getByRole('button', { name: 'Back', exact: true })
    const forward = page.getByRole('button', { name: 'Forward', exact: true })
    await expect(back).toBeDisabled()
    await expect(forward).toBeDisabled()
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
  const toggle = page.getByRole('button', { name: 'Collapse sidebar', exact: true })
  expect(await toggle.evaluate(element => getComputedStyle(element).color)).toBe(await rail.evaluate(element => getComputedStyle(element).color))
  await toggle.hover()
  expect(await toggle.evaluate(element => getComputedStyle(element).color)).toBe(await rail.evaluate(element => getComputedStyle(element).color))
  await toggle.evaluate(element => (element as HTMLElement).focus({ focusVisible: true } as FocusOptions))
  expect(await toggle.evaluate(element => getComputedStyle(element).outlineColor)).toBe(await rail.evaluate(element => getComputedStyle(element).color))
  await page.getByRole('button', { name: 'Close settings' }).focus()
  await page.getByRole('button', { name: 'Close settings' }).click()
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveText('Keep the rail colour here')
  const railBackground = await rail.evaluate(element => getComputedStyle(element).backgroundColor)
  expect(railBackground).not.toBe(menuBackground)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/toolbar-rail-expanded.png' })
  await page.getByRole('button', { name: 'Collapse sidebar', exact: true }).click()
  expect(await rail.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(railBackground)
  expect(await toolbar.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(railBackground)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/sidebar-rail/toolbar-rail-collapsed.png' })
  await page.getByRole('button', { name: 'Agents', exact: true }).click()
  await page.getByRole('button', { name: 'Expand sidebar', exact: true }).click()
  expect(await page.getByRole('complementary', { name: 'Agent list' }).evaluate(element => getComputedStyle(element).backgroundColor)).toBe(menuBackground)
  expect(await rail.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(railBackground)
  expect(await toolbar.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(railBackground)
  await page.reload()
  expect(await rail.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(railBackground)
  expect(await toolbar.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(railBackground)
  expect(await homeMenu.evaluate(element => getComputedStyle(element).backgroundColor)).toBe(menuBackground)
})

test('theme accents follow the saved colour in the workspace and portalled chat options', async ({ page }) => {
  await connectSidebarFixture(page)
  await page.getByRole('button', { name: 'Settings', exact: true }).click()
  await page.locator('.appearance-color-option').filter({ has: page.getByRole('radio', { name: 'Pink', exact: true }) }).click()
  const pink = await page.locator('.appearance-color-option.selected .appearance-color-swatch').evaluate(element => getComputedStyle(element).backgroundColor)
  await page.getByRole('button', { name: 'Close settings' }).click()
  await expect.poll(() => page.locator('.welcome-symbol').evaluate(element => getComputedStyle(element).color)).toBe(pink)
  expect(await page.locator('.rail-avatar').evaluate(element => getComputedStyle(element).backgroundColor)).toBe(pink)
  await page.getByRole('textbox', { name: 'Message' }).fill('Keep my draft while changing the theme')
  await page.getByRole('button', { name: 'Chat options', exact: true }).click()
  const identity = page.getByRole('combobox', { name: 'Chat identity' })
  await identity.evaluate(element => (element as HTMLElement).focus({ focusVisible: true } as FocusOptions))
  expect(await identity.evaluate(element => getComputedStyle(element).outlineColor)).toBe(pink)
  expect(await page.getByRole('checkbox', { name: 'Read only' }).evaluate(element => getComputedStyle(element).accentColor)).toBe(pink)
  await page.screenshot({ path: 'projects/desktop-agent/evidence/theme-accents-pink.png' })
  await page.keyboard.press('Escape')
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveText('Keep my draft while changing the theme')
  await page.reload()
  expect(await page.locator('.welcome-symbol').evaluate(element => getComputedStyle(element).color)).toBe(pink)
  await page.getByRole('button', { name: 'Chat options', exact: true }).click()
  await page.getByRole('combobox', { name: 'Chat identity' }).evaluate(element => (element as HTMLElement).focus({ focusVisible: true } as FocusOptions))
  expect(await page.getByRole('combobox', { name: 'Chat identity' }).evaluate(element => getComputedStyle(element).outlineColor)).toBe(pink)
})
