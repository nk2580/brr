import {test, expect, type Page} from '@playwright/test'

async function desktopFixture(page: Page) {
  await page.addInitScript(() => {
    const host = window as any
    host.__workspaceCalls = []
    let pendingSelection = false, annotating = false, file = 'Original file\n', version = 'v1'
    let url = 'about:blank'
    const thread = {id: 'existing', projectId: 'project-atlas', cwd: '/projects/atlas.session', preview: 'Saved project chat', updatedAt: 1, worktree: {projectPath: '/projects/atlas', path: '/projects/atlas.session', cwd: '/projects/atlas.session', branch: 'codex/test'}, workspace: {cwd: '/projects/atlas.session', roots: ['/projects/atlas.session'], folders: [{source: '/projects/atlas', path: '/projects/atlas.session'}], worktrees: [{projectPath: '/projects/atlas', path: '/projects/atlas.session', cwd: '/projects/atlas.session', branch: 'codex/test'}], scopeProjectId: 'project-atlas', projectRevision: 1}, turns: []}
    host.__TAURI_INTERNALS__ = {
      transformCallback: () => 1,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect' || command.startsWith('plugin:event|')) return 1
        if (command === 'list_projects') return {data: [{id: 'project-atlas', name: 'Atlas', folders: ['/projects/atlas'], primaryFolder: '/projects/atlas', revision: 1, hidden: false}]}
        if (command === 'agents_request') return {data: [], nextCursor: null}
        host.__workspaceCalls.push({command, ...args})
        if (command === 'workspace_browser_request') {
          if (args.method === 'browser.create') return {id: 'browser-1'}
          if (args.method === 'browser.navigate') { url = args.params.url; return {} }
          if (args.method === 'browser.action') { annotating = Boolean(args.params.enabled); pendingSelection = annotating; return {} }
          if (args.method === 'browser.snapshot') {
            const selection = pendingSelection ? {url, title: 'Example site', selector: '#hero', quote: 'Build something useful'} : null
            pendingSelection = false
            return {url, title: url === 'about:blank' ? '' : 'Example site', annotating, selection}
          }
          return {}
        }
        if (command === 'workspace_request') {
          const {method, params} = args
          if (method === 'directory.list') return {entries: [{name: 'note.txt', path: 'note.txt', kind: 'file', size: file.length}]}
          if (method === 'file.read') return {path: 'note.txt', content: file, version}
          if (method === 'file.write') { file = params.content; version = 'v2'; return {path: 'note.txt', content: file, version} }
          if (method === 'git.status') return {branch: 'main', files: [{path: 'note.txt', indexStatus: ' ', worktreeStatus: 'M'}]}
          if (method === 'git.diff') return {patch: 'diff --git a/note.txt b/note.txt\n--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-Original file\n+Edited file\n', truncated: false}
          if (method === 'terminal.create') return {id: 'pty-1'}
          if (method === 'terminal.read') return {chunks: params.after ? [] : [{seq: 1, data: Array.from(new TextEncoder().encode('Atlas terminal ready\r\n$ '))}], cursor: 1, exited: false}
          return {}
        }
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
        if (args.method === 'account/read') return {account: {type: 'chatgpt', email: 'developer@example.com'}}
        if (args.method === 'model/list') return {data: [{id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT-6.1-Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: []}]}
        if (args.method === 'thread/list') return {data: [thread], nextCursor: null}
        if (args.method === 'thread/resume') return {thread}
        return {}
      },
    }
    host.__TAURI_EVENT_PLUGIN_INTERNALS__ = {unregisterListener: () => {}}
  })
  await page.goto('/')
  await page.getByRole('button', {name: 'Saved project chat', exact: true}).click()
  await expect(page.getByRole('textbox', {name: 'Message'})).toBeEnabled()
}

test('workspace tools use the saved worktree path, preserve edits and keep tabs when the panel closes', async ({page}) => {
  await desktopFixture(page)
  await page.getByRole('textbox', {name: 'Message'}).fill('Keep my chat draft')
  await page.getByRole('button', {name: 'New tab'}).click()
  const panel = page.getByRole('complementary', {name: 'Workspace tools'})
  await panel.getByRole('button', {name: 'Files', exact: true}).click()
  await panel.getByRole('button', {name: 'note.txt', exact: true}).click()
  const editor = panel.getByRole('textbox', {name: 'File contents'})
  await editor.fill('Edited file\n')
  await page.getByRole('button', {name: 'Add workspace tab'}).click()
  await panel.getByRole('button', {name: 'Git diff', exact: true}).click()
  await panel.getByRole('button', {name: 'note.txt', exact: true}).click()
  await expect(panel.getByText('+Edited file', {exact: true})).toBeVisible()
  await page.getByRole('tab', {name: /^Files/}).click()
  await expect(editor).toHaveValue('Edited file\n')
  await page.getByRole('button', {name: 'Close Files tab'}).click()
  await expect(panel.getByRole('alert')).toContainText('unsaved edits')
  await panel.getByRole('button', {name: 'Keep tab'}).click()
  await panel.getByRole('button', {name: 'Save', exact: true}).click()
  await expect(panel.getByLabel('Unsaved changes')).toHaveCount(0)
  await page.getByRole('button', {name: 'Add workspace tab'}).click()
  await panel.getByRole('button', {name: 'Terminal', exact: true}).click()
  await expect(panel.locator('.xterm-screen')).toContainText('Atlas terminal ready')
  await panel.locator('.xterm-helper-textarea').focus()
  await page.keyboard.type('pwd')
  await page.keyboard.press('Enter')
  await expect.poll(() => page.evaluate(() => (window as any).__workspaceCalls.some((call: any) => call.method === 'terminal.write'))).toBe(true)
  const calls = await page.evaluate(() => (window as any).__workspaceCalls.filter((call: any) => call.command === 'workspace_request'))
  expect(calls.filter((call: any) => call.params.root).every((call: any) => call.params.root === '/projects/atlas.session')).toBe(true)
  await page.getByRole('button', {name: 'Close workspace panel'}).click()
  await expect(panel).not.toBeVisible()
  await expect(page.getByRole('textbox', {name: 'Message'})).toHaveText('Keep my chat draft')
  await page.getByRole('button', {name: 'New tab'}).click()
  await expect(page.getByRole('tab', {name: /^Files/})).toBeVisible()
  await expect(page.getByRole('tab', {name: 'Terminal'})).toBeVisible()
  await page.screenshot({path: 'projects/desktop-agent/evidence/workspace-tools/tabs.png'})
})

test('browser notes require explicit sharing and the panel fits beside chat at 800x600', async ({page}) => {
  await desktopFixture(page)
  const draft = page.getByRole('textbox', {name: 'Message'})
  await draft.fill('Please review this page')
  await page.getByRole('button', {name: 'New tab'}).click()
  const panel = page.getByRole('complementary', {name: 'Workspace tools'})
  await panel.getByRole('button', {name: 'Browser', exact: true}).click()
  await panel.getByRole('textbox', {name: 'Website address'}).fill('https://example.com')
  await panel.getByRole('button', {name: 'Go', exact: true}).click()
  await panel.getByRole('button', {name: 'Annotate page', exact: true}).click()
  await panel.getByRole('textbox', {name: 'Annotation note'}).fill('Give this heading more space')
  await panel.getByRole('button', {name: 'Add note', exact: true}).click()
  await expect(draft).toHaveText('Please review this page')
  await panel.getByRole('button', {name: 'Expand workspace panel'}).click()
  await expect(panel.getByRole('button', {name: 'Restore split view'})).toBeVisible()
  await panel.getByRole('button', {name: 'Add notes to message'}).click()
  await expect(draft).toBeVisible()
  await expect(draft).toHaveText(/Please review this page\n\nWeb page annotations:/)
  await expect(draft).toHaveText(/Give this heading more space/)
  expect(await page.evaluate(() => (window as any).__workspaceCalls.some((call: any) => call.method === 'turn/start'))).toBe(false)
  await page.setViewportSize({width: 800, height: 600})
  const resizer = page.getByRole('separator', {name: 'Resize workspace panel'})
  await resizer.focus()
  await page.keyboard.press('ArrowRight')
  const paneBox = (await panel.boundingBox())!
  const sendBox = (await page.getByRole('button', {name: 'Send message'}).boundingBox())!
  expect(paneBox.x).toBeGreaterThanOrEqual(360)
  expect(paneBox.x + paneBox.width).toBeLessThanOrEqual(800)
  expect(sendBox.x + sendBox.width).toBeLessThanOrEqual(paneBox.x)
  expect(sendBox.y + sendBox.height).toBeLessThanOrEqual(600)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await page.screenshot({path: 'projects/desktop-agent/evidence/workspace-tools/browser-minimum.png'})
  await page.getByRole('button', {name: 'Expand sidebar'}).click()
  await expect(panel).not.toBeVisible()
  await expect(page.getByRole('complementary', {name: 'Main sidebar'})).toBeVisible()
})

test('workspace expands across the app and restores the split with draft, edits and width intact', async ({page}) => {
  await desktopFixture(page)
  const draft = page.getByRole('textbox', {name: 'Message', includeHidden: true})
  await draft.fill('Keep my draft while I focus on files')
  await page.getByRole('button', {name: 'New tab'}).click()
  const panel = page.getByRole('complementary', {name: 'Workspace tools'})
  await panel.getByRole('button', {name: 'Files', exact: true}).click()
  await panel.getByRole('button', {name: 'note.txt', exact: true}).click()
  const editor = panel.getByRole('textbox', {name: 'File contents'})
  await editor.fill('An unsaved edit to keep\n')
  const splitWidth = (await panel.boundingBox())!.width

  await panel.getByRole('button', {name: 'Expand workspace panel'}).click()
  await expect(draft).not.toBeVisible()
  await expect(page.getByRole('button', {name: 'New tab'})).not.toBeVisible()
  await expect(page.getByRole('separator', {name: 'Resize workspace panel'})).toHaveCount(0)
  await expect(editor).toHaveValue('An unsaved edit to keep\n')
  const rail = (await page.getByRole('navigation', {name: 'Main rail'}).boundingBox())!
  const expanded = (await panel.boundingBox())!
  expect(Math.abs(expanded.x - rail.x - rail.width)).toBeLessThanOrEqual(2)
  expect(expanded.x + expanded.width).toBeCloseTo(page.viewportSize()!.width, 0)
  await page.getByRole('button', {name: 'Settings', exact: true}).click()
  await expect(panel).not.toBeVisible()
  await page.getByRole('button', {name: 'Collapse sidebar'}).click()
  await expect(page.getByRole('button', {name: 'Expand sidebar'})).toBeVisible()
  await page.getByRole('button', {name: 'Expand sidebar'}).click()
  await page.getByRole('button', {name: 'Close settings'}).click()
  await expect(panel.getByRole('button', {name: 'Restore split view'})).toBeVisible()
  await page.setViewportSize({width: 800, height: 600})
  const narrow = (await panel.boundingBox())!
  expect(narrow.width).toBeGreaterThan(700)
  expect(narrow.x + narrow.width).toBeLessThanOrEqual(800)
  await page.screenshot({path: 'projects/desktop-agent/evidence/workspace-tools/expanded-minimum.png'})
  await page.setViewportSize({width: 1280, height: 860})
  await panel.getByRole('button', {name: 'Restore split view'}).click()
  await expect(draft).toBeVisible()
  await expect(draft).toHaveText('Keep my draft while I focus on files')
  await expect(editor).toHaveValue('An unsaved edit to keep\n')
  await expect(panel.getByRole('button', {name: 'Save', exact: true})).toBeEnabled()
  expect((await panel.boundingBox())!.width).toBeCloseTo(splitWidth, 0)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})
