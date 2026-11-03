import { test, expect } from '@playwright/test'
import { mkdir } from 'node:fs/promises'

test('projects keep stable identity, folder snapshots, draft context and recent chats through move and deletion', async ({ page }) => {
  await mkdir('projects/desktop-agent/evidence/project-buckets', {recursive: true})
  await page.addInitScript(() => {
    const browser = window as any
    const requests: any[] = browser.__projectRequests = []
    const projects: any[] = []
    const threads: any[] = []
    let nextProject = 1
    let nextThread = 1
    let folderPick = 0
    browser.__projectCalls = requests
    browser.__projectThreads = threads
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'plugin:dialog|open') {
          if (args.options?.title === 'Add project folders') return folderPick++ === 0
            ? ['/work/research-a', '/work/research-b/with/a/very/long/folder/path/for/desktop/validation']
            : ['/work/research-c']
          return null
        }
        if (command === 'list_projects') return {data: projects.filter(project => !project.hidden)}
        if (command === 'create_project') {
          const input = args.project
          const project = {id: `project-${nextProject++}`, revision: 1, ...input, folders: [...input.folders], hidden: false, model: input.model ?? null, effort: input.effort ?? null, permission: input.permission ?? null}
          projects.push(project)
          return project
        }
        if (command === 'save_project') {
          const index = projects.findIndex(project => project.id === args.project.id)
          if (index < 0) throw new Error('unknown project')
          projects[index] = {...args.project, revision: projects[index].revision + 1}
          return projects[index]
        }
        if (command === 'delete_project') {
          const index = projects.findIndex(project => project.id === args.projectId)
          if (index >= 0) projects.splice(index, 1)
          for (const thread of threads) if (thread.projectId === args.projectId) thread.projectId = null
          return
        }
        if (command === 'move_chat_to_project') {
          const thread = threads.find(value => value.id === args.threadId)
          if (thread) thread.projectId = args.projectId
          return
        }
        if (command === 'archive_project_chats') {
          requests.push({command, ...args})
          return {archivedIds: [], skipped: [], failed: []}
        }
        if (command === 'chat_request') {
          const {method, params = {}} = args
          ;((window as any).__chatRequests ??= []).push({method, params})
          if (method === 'drafts.list' || method === 'attachments.list') return []
          if (method === 'drafts.get') throw new Error('No saved fixture draft')
          if (method === 'drafts.save') return {...params, revision: (params.expectedRevision ?? 0) + 1, updatedAt: '2026-11-02T00:00:00Z'}
          if (method === 'drafts.promote') return {id: params.id, threadId: params.threadId, projectId: params.projectId ?? null, agentId: params.agentId ?? null, document: params.document ?? {text: '', references: [], quotes: []}, settings: params.settings ?? {model: '', effort: 'medium', mode: 'default'}, revision: 1, updatedAt: '2026-11-02T00:00:00Z'}
          if (method === 'foreground.submit') return {turn: {id: 'fixture-turn'}, turnId: 'fixture-turn', submission: {id: 'fixture-submission', threadId: params.threadId, status: 'accepted', revision: 1}, queued: false}
          if (method === 'queue.list') return {entries: [], state: {paused: false}}
          return {}
        }
        if (command === 'agents_request' && args.method === 'questions.list') return {data: []}
        if (command !== 'engine_request') throw new Error(`Unexpected command ${command}`)
        requests.push(args)
        const {method, params = {}} = args
        if (method === 'account/read') return {account: {type: 'chatgpt', email: 'developer@example.com', planType: 'plus'}}
        if (method === 'model/list') return {data: [{id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT-6.1-Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{reasoningEffort: 'medium', description: 'Balanced'}]}]}
        if (method === 'thread/list') return {data: threads, nextCursor: null}
        if (method === 'brr/project/inspect') return {commands: []}
        if (method === 'thread/start') {
          const project = projects.find(value => value.id === params.projectId)
          const id = `thread-${nextThread++}`
          const cwd = project?.primaryFolder ?? ''
          const name = id.endsWith('1') ? 'Project scoped chat' : 'Updated project chat'
          const thread = {id, projectId: project?.id ?? null, name, preview: name, cwd, updatedAt: Date.now(), workspace: project ? {cwd, roots: [...project.folders], folders: project.folders.map((folder: string) => ({source: folder, path: folder})), worktrees: [], scopeProjectId: project.id, projectRevision: project.revision} : undefined, turns: []}
          threads.unshift(thread)
          return {thread}
        }
        if (method === 'turn/start') return {turn: {id: 'turn-1'}}
        if (method === 'thread/resume') return {thread: threads.find(value => value.id === params.threadId)}
        if (method === 'turn/interrupt') return {}
        throw new Error(`Unexpected request ${method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = {unregisterListener: () => {}}
  })
  await page.goto('/')
  await page.getByRole('button', {name: 'New project'}).click()
  await expect(page.getByRole('dialog', {name: 'Create project'})).toBeVisible()
  await page.screenshot({path: 'projects/desktop-agent/evidence/project-buckets/creation-dialog.png'})
  await page.getByRole('textbox', {name: 'Project name'}).fill('Research bucket')
  await page.getByRole('button', {name: 'Create project', exact: true}).click()
  await expect(page.getByRole('button', {name: 'New chat in Research bucket'})).toBeVisible()
  await page.getByRole('button', {name: 'New chat in Research bucket'}).click()
  await expect(page.getByText('No folders attached', {exact: true})).toBeVisible()

  await page.getByRole('button', {name: 'Manage project Research bucket'}).click()
  await page.getByRole('menuitem', {name: 'Project settings'}).click()
  const editDialog = page.getByRole('dialog', {name: 'Edit project'})
  await editDialog.getByRole('button', {name: 'Add folders'}).click()
  await expect(editDialog.getByText('/work/research-a')).toBeVisible()
  await expect(editDialog.getByText('/work/research-b/with/a/very/long/folder/path/for/desktop/validation')).toBeVisible()
  await editDialog.getByRole('radio', {name: 'Primary folder /work/research-b/with/a/very/long/folder/path/for/desktop/validation'}).check()
  await editDialog.getByRole('button', {name: 'Save changes'}).click()

  await page.getByRole('button', {name: 'New project'}).click()
  await page.getByRole('textbox', {name: 'Project name'}).fill('Other bucket')
  await page.getByRole('button', {name: 'Create project', exact: true}).click()
  await page.getByRole('button', {name: 'New chat in Research bucket'}).click()
  const message = page.getByRole('textbox', {name: 'Message'})
  await message.fill('Keep this project draft while I switch buckets')
  await page.screenshot({path: 'projects/desktop-agent/evidence/project-buckets/new-chat-desktop.png'})
  await page.setViewportSize({width: 800, height: 600})
  await expect(message).toHaveText('Keep this project draft while I switch buckets')
  await page.screenshot({path: 'projects/desktop-agent/evidence/project-buckets/new-chat-minimum.png'})
  await page.getByRole('combobox', {name: 'Project'}).selectOption({label: 'Other bucket'})
  await expect(message).toHaveText('Keep this project draft while I switch buckets')
  await page.getByRole('combobox', {name: 'Project'}).selectOption({label: 'Research bucket'})
  await page.getByRole('button', {name: 'Send message'}).click()
  await expect.poll(() => page.evaluate(() => (window as any).__projectCalls.some((call: any) => call.method === 'thread/start'))).toBe(true)
  const initialStart = await page.evaluate(() => (window as any).__projectCalls.find((call: any) => call.method === 'thread/start'))
  expect(initialStart.params.projectId).toBe('project-1')
  const initialWorkspace = await page.evaluate(() => JSON.parse(JSON.stringify((window as any).__projectThreads[0].workspace)))
  expect(initialWorkspace.roots).toEqual(['/work/research-a', '/work/research-b/with/a/very/long/folder/path/for/desktop/validation'])
  expect(initialWorkspace.cwd).toBe('/work/research-b/with/a/very/long/folder/path/for/desktop/validation')

  await page.getByRole('button', {name: 'Manage project Research bucket'}).click()
  await page.getByRole('menuitem', {name: 'Project settings'}).click()
  const changedDialog = page.getByRole('dialog', {name: 'Edit project'})
  await changedDialog.getByRole('button', {name: 'Add folders'}).click()
  await changedDialog.getByRole('button', {name: 'Save changes'}).click()
  const savedThread = await page.evaluate(() => (window as any).__projectCalls.find((call: any) => call.method === 'thread/start'))
  expect(savedThread.params.projectId).toBe('project-1')
  expect(await page.evaluate(() => (window as any).__projectThreads[0].workspace)).toEqual(initialWorkspace)

  await page.getByRole('button', {name: 'New conversation'}).click()
  await page.getByRole('combobox', {name: 'Project'}).selectOption({label: 'Research bucket'})
  await page.getByRole('textbox', {name: 'Message'}).fill('Use the newly saved project folders')
  await page.getByRole('button', {name: 'Send message'}).click()
  await expect.poll(() => page.evaluate(() => (window as any).__projectCalls.filter((call: any) => call.method === 'thread/start').length)).toBe(2)
  const updatedThread = await page.evaluate(() => (window as any).__projectThreads[0])
  expect(updatedThread.workspace.roots).toEqual(['/work/research-a', '/work/research-b/with/a/very/long/folder/path/for/desktop/validation', '/work/research-c'])
  expect(updatedThread.workspace).not.toEqual(initialWorkspace)

  const moving = page.getByRole('combobox', {name: 'Move chat to project'})
  await moving.selectOption('')
  await expect(page.getByRole('button', {name: 'Updated project chat', exact: true})).toBeVisible()
  await page.getByRole('button', {name: 'Manage project Research bucket'}).click()
  await page.getByRole('menuitem', {name: 'Delete project'}).click()
  await page.getByRole('button', {name: 'Delete project', exact: true}).click()
  await expect(page.getByRole('button', {name: 'Updated project chat', exact: true})).toBeVisible()
  await expect(page.getByRole('button', {name: 'Project scoped chat', exact: true})).toBeVisible()
  expect(await page.evaluate(() => (window as any).__projectCalls.some((call: any) => call.command === 'archive_project_chats'))).toBe(false)
})

test('many long project folders keep the compact setup dialog and composer in view', async ({page}) => {
  await page.setViewportSize({width: 800, height: 600})
  await page.addInitScript(() => {
    const browser = window as any
    const roots = Array.from({length: 14}, (_, index) => `/workspace/long-project-path-${index}/nested/source/and/validation/folder-${index}`)
    let project: any = null
    let thread: any = null
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'plugin:dialog|open') return roots
        if (command === 'list_projects') return {data: project ? [project] : []}
        if (command === 'create_project') { project = {id: 'project-many', revision: 1, ...args.project, hidden: false}; return project }
        if (command === 'chat_request') {
          const {method, params = {}} = args
          if (method === 'drafts.list' || method === 'attachments.list') return []
          if (method === 'drafts.get') throw new Error('No saved fixture draft')
          if (method === 'drafts.save') return {...params, revision: (params.expectedRevision ?? 0) + 1, updatedAt: '2026-11-02T00:00:00Z'}
          if (method === 'drafts.promote') return {id: params.id, threadId: params.threadId, document: params.document ?? {text: '', references: [], quotes: []}, settings: params.settings ?? {model: '', effort: 'medium', mode: 'default'}, revision: 1, updatedAt: '2026-11-02T00:00:00Z'}
          if (method === 'foreground.submit') return {turn: {id: 'turn-many'}, turnId: 'turn-many', submission: {id: 'submission-many', threadId: params.threadId, status: 'accepted', revision: 1}, queued: false}
          if (method === 'queue.list') return {entries: [], state: {paused: false}}
          return {}
        }
        if (command === 'agents_request' && args.method === 'questions.list') return {data: []}
        if (command !== 'engine_request') throw new Error(`Unexpected command ${command}`)
        const {method, params = {}} = args
        if (method === 'account/read') return {account: {type: 'chatgpt', email: 'developer@example.com', planType: 'plus'}}
        if (method === 'model/list') return {data: [{id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT-6.1-Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{reasoningEffort: 'medium', description: 'Balanced'}]}]}
        if (method === 'thread/list') return {data: thread ? [thread] : [], nextCursor: null}
        if (method === 'brr/project/inspect') return {commands: []}
        if (method === 'thread/start') {
          thread = {id: 'thread-many', projectId: project.id, cwd: project.primaryFolder, preview: '', updatedAt: Date.now(), workspace: {cwd: project.primaryFolder, roots: [...roots], folders: roots.map((source: string) => ({source, path: source})), worktrees: [], scopeProjectId: project.id, projectRevision: project.revision}, turns: []}
          return {thread}
        }
        if (method === 'thread/resume') return {thread}
        if (method === 'turn/start' || method === 'turn/interrupt') return {turn: {id: 'turn-many'}}
        throw new Error(`Unexpected request ${method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = {unregisterListener: () => {}}
  })
  await page.goto('/')
  await page.getByRole('button', {name: 'New project'}).click()
  const dialog = page.getByRole('dialog', {name: 'Create project'})
  await dialog.getByRole('textbox', {name: 'Project name'}).fill('Many roots')
  await dialog.getByRole('button', {name: 'Add folders'}).click()
  await expect(dialog.locator('.project-dialog-folders li')).toHaveCount(14)
  const dialogBox = await dialog.boundingBox()
  expect(dialogBox).not.toBeNull()
  expect(dialogBox!.y).toBeGreaterThanOrEqual(0)
  expect(dialogBox!.y + dialogBox!.height).toBeLessThanOrEqual(600)
  const scrollMetrics = await dialog.evaluate(element => ({client: element.clientHeight, scroll: element.scrollHeight}))
  expect(scrollMetrics.scroll).toBeGreaterThan(scrollMetrics.client)
  await dialog.evaluate(element => { element.scrollTop = element.scrollHeight })
  for (const name of ['Create project', 'Cancel']) {
    const box = await dialog.getByRole('button', {name, exact: true}).boundingBox()
    expect(box).not.toBeNull()
    expect(box!.y).toBeGreaterThanOrEqual(0)
    expect(box!.y + box!.height).toBeLessThanOrEqual(600)
  }
  await page.screenshot({path: 'projects/desktop-agent/evidence/project-buckets/many-roots-minimum.png'})
  await dialog.getByRole('button', {name: 'Create project', exact: true}).click()
  await page.getByRole('button', {name: 'New chat in Many roots'}).click()
  const composer = await page.locator('.composer-area').boundingBox()
  expect(composer).not.toBeNull()
  expect(composer!.y + composer!.height).toBeLessThanOrEqual(600)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
})
