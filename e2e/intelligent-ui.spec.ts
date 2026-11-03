import { test, expect, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'

const nativeDocument = {
  version: 1, kind: 'native', title: 'Compare rollout options', text: 'Explore the tradeoffs, then ask the agent to use your selections.',
  initialState: {priority: 'reliability', notes: '', detail: 'summary'},
  actions: [{id: 'choose', label: 'Ask agent', prompt: 'Use these rollout preferences.'}],
  components: [
    {id: 'priority', type: 'select', label: 'Optimize for', stateKey: 'priority', options: [{label: 'Reliability', value: 'reliability'}, {label: 'Cost', value: 'cost'}]},
    {id: 'selection', type: 'text', text: 'Current priority: {{priority}}'},
    {id: 'details', type: 'comparison', columns: [{label: 'Canary rollout', text: 'Introduce the release gradually and watch the results.'}, {label: 'Full rollout', text: 'Release to everyone after verification.'}]},
    {id: 'impact', type: 'chart', chartType: 'bar', points: [{label: 'Canary', value: 75}, {label: 'Full', value: -25}]},
    {id: 'notes', type: 'input', label: 'Additional context', stateKey: 'notes'},
    {id: 'ask', type: 'button', label: 'Ask agent', actionId: 'choose'},
  ],
}

async function fixture(page: Page, appSandbox?: {url: string; token: string; hostOrigin?: string}) {
  await page.addInitScript(({doc, appSandbox}) => {
    const browser = window as any
    const requests: any[] = browser.__uiRequests = []
    const app = {version: 1, kind: 'app', title: 'Color studio', text: 'Choose a color for the next agent response.', initialState: {hue: 210}, actions: [{id: 'record', label: 'Ask agent', prompt: 'Use this color.'}], html: '<p>Served by the actual Rust fixture</p>'}
    const docs: Record<string, any> = {'native-call': doc, 'app-call': app}
    const thread = {id: 'ui-chat', projectId: null, cwd: '/project', workspace: {cwd: '/project', roots: ['/project'], folders: [{source: '/project', path: '/project'}], worktrees: [], scopeProjectId: null, projectRevision: null}, preview: 'Explore rollout options', updatedAt: 1, collaborationMode: {mode: 'plan'}, turns: [{id: 'done', status: 'completed', items: [{id: 'native-call', type: 'dynamicToolCall', tool: 'brr_ui_render', status: 'completed', success: true, arguments: doc}, ...(appSandbox ? [{id: 'app-call', type: 'dynamicToolCall', tool: 'brr_ui_render', status: 'completed', success: true, arguments: app}] : [])]}]}
    const saved = (callId: string) => JSON.parse(localStorage.getItem(`ui-fixture-${callId}`) ?? JSON.stringify({state: docs[callId].initialState, revision: 0}))
    let review: any = null, claimed = false
    browser.__TAURI_INTERNALS__ = {transformCallback: () => 1, invoke: async (command: string, args: any = {}) => {
      if (command.startsWith('plugin:event|') || command === 'engine_connect') return 1
      if (command === 'workspace_browser_request') return {}
      if (command === 'list_projects') return {data: []}
      if (command === 'create_project') return {id: 'project-ui', revision: 1, ...args.project, hidden: false}
      if (command === 'save_project') return {...args.project, revision: (args.project.revision ?? 1) + 1, hidden: false}
      if (command === 'delete_project' || command === 'move_chat_to_project') return null
      if (command === 'agents_request') {
        if (args.method === 'agents.settings') return {maxBackground: 4, maxPerAgent: 2}
        return {data: []}
      }
      if (command === 'chat_request') {
        const {method, params = {}} = args
        ;((window as any).__chatRequests ??= []).push({method, params})
        if (method === 'drafts.list' || method === 'attachments.list') return []
        if (method === 'drafts.get') throw new Error('No saved fixture draft')
        if (method === 'drafts.save') return {...params, revision: (params.expectedRevision ?? 0) + 1, updatedAt: '2026-11-02T00:00:00Z'}
        if (method === 'foreground.submit') return {turn: {id: 'followup'}, turnId: 'followup', submission: {id: 'submission', threadId: params.threadId, status: 'accepted', revision: 1}, queued: false}
        if (method === 'queue.list') return {entries: [], state: {paused: false}}
        throw new Error(`Unexpected chat method ${method}`)
      }
      requests.push({command, ...args})
      if (command === 'intelligent_ui_request') {
        const p = args.params, doc = docs[p.callId]
        if (args.method === 'document.read') return {document: doc, documentHash: `hash-${p.callId}`}
        if (args.method === 'state.read') return saved(p.callId)
        if (args.method === 'state.write') {const old = saved(p.callId); if (old.revision !== p.revision) throw new Error('Stale state'); const next = {state: p.state, revision: p.revision + 1}; localStorage.setItem(`ui-fixture-${p.callId}`, JSON.stringify(next)); return next}
        if (args.method === 'action.prepare') {review = {reviewId: 'review', threadId: 'ui-chat', callId: p.callId, documentHash: `hash-${p.callId}`, text: `${doc.actions[0].prompt}\n\nCaptured inputs: ${JSON.stringify(saved(p.callId).state)}`}; claimed = false; return review}
        if (args.method === 'action.claim') {if (claimed) throw new Error('Already sent'); claimed = true; return review}
        if (args.method === 'sandbox.create' && appSandbox) return appSandbox
        if (args.method === 'sandbox.destroy') return null
        throw new Error(`Unexpected UI method ${args.method}`)
      }
      if (command === 'engine_request') {
        if (args.method === 'account/read') return {account: {type: 'chatgpt', email: 'fixture@example.com', planType: 'plus'}}
        if (args.method === 'model/list') return {data: [{id: 'sol', model: 'gpt-6.1-sol', displayName: 'Sol', defaultReasoningEffort: 'medium', supportedReasoningEfforts: []}]}
        if (args.method === 'thread/list') return {data: [thread], nextCursor: null}
        if (args.method === 'thread/resume') return {thread}
        if (args.method === 'turn/start') return {turn: {id: 'followup'}}
      }
      throw new Error(`Unexpected fixture request ${command} ${args.method}`)
    }}
  }, {doc: nativeDocument, appSandbox})
  await page.goto(appSandbox?.hostOrigin ?? '/')
  await page.getByRole('button', {name: 'Explore rollout options'}).click()
}

test('native UI preserves controls across expansion and reload, then resets explicitly', async ({page}) => {
  await fixture(page)
  await page.getByRole('combobox', {name: 'Optimize for'}).selectOption('cost')
  await expect(page.getByText('Current priority: cost', {exact: true})).toBeVisible()
  await page.getByRole('button', {name: 'Expand', exact: true}).click()
  await expect(page.getByRole('tab', {name: nativeDocument.title})).toBeVisible()
  await expect(page.getByRole('combobox', {name: 'Optimize for'})).toHaveValue('cost')
  await page.getByRole('button', {name: `Close ${nativeDocument.title} tab`}).click()
  await page.reload()
  await page.getByRole('button', {name: 'Explore rollout options'}).click()
  await expect(page.getByRole('combobox', {name: 'Optimize for'})).toHaveValue('cost')
  await page.getByRole('button', {name: 'Reset', exact: true}).click()
  await expect(page.getByRole('combobox', {name: 'Optimize for'})).toHaveValue('reliability')
  await page.screenshot({path: 'evidence/intelligent-ui/native-desktop.png', fullPage: true})
  await page.setViewportSize({width: 800, height: 600})
  await expect(page.getByRole('button', {name: 'Send message'})).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  await page.screenshot({path: 'evidence/intelligent-ui/native-800x600.png', fullPage: true})
})

test('reviewed agent actions preserve the unsent message and current Plan mode', async ({page}) => {
  await fixture(page)
  await page.getByRole('textbox', {name: 'Message'}).fill('Keep my draft')
  await page.getByRole('button', {name: 'Ask agent', exact: true}).click()
  await expect(page.getByRole('region', {name: 'Review UI follow-up'})).toContainText('Captured inputs')
  expect(await page.evaluate(() => (window as any).__uiRequests.filter((r: any) => r.method === 'turn/start').length)).toBe(0)
  await page.getByRole('button', {name: 'Send follow-up'}).click()
  await expect(page.getByRole('textbox', {name: 'Message'})).toHaveText('Keep my draft')
  expect(await page.evaluate(() => (window as any).__chatRequests.find((r: any) => r.method === 'foreground.submit')?.params.settings.mode)).toBe('plan')
})

test('actual Rust sandbox runs scripts, blocks network, and synchronizes saved state and Reset', async ({page}) => {
  test.skip(!process.env.BRR_UI_FIXTURE_READY, 'Start the Rust browser_fixture and supply its ready.json path')
  const sandbox = JSON.parse(readFileSync(process.env.BRR_UI_FIXTURE_READY!, 'utf8'))
  page.on('dialog', async dialog => { console.log('Sandbox dialog:', dialog.message()); await dialog.dismiss() })
  await fixture(page, sandbox)
  await page.locator('[data-item-type="intelligentUi"]').nth(1).scrollIntoViewIfNeeded()
  const view = page.getByRole('region', {name: 'Color studio', exact: true})
  const app = page.frameLocator('iframe[title="Color studio interactive app"]').frameLocator('iframe')
  await expect(app.getByRole('heading', {name: 'Sandbox state fixture'})).toBeVisible()
  await expect(app.getByText('network: blocked', {exact: true})).toBeVisible()
  await expect(app.getByText('native API: absent', {exact: true})).toBeVisible()
  await app.getByRole('slider', {name: 'Hue'}).focus()
  await app.getByRole('slider', {name: 'Hue'}).press('End')
  await expect(app.getByText('saved hue: 360', {exact: true})).toBeVisible()
  await page.getByRole('textbox', {name: 'Message'}).fill('Typing must not restart the app')
  await expect(app.getByText('saved hue: 360', {exact: true})).toBeVisible()
  await view.getByRole('button', {name: 'Reset', exact: true}).click()
  await expect(app.getByText('saved hue: 210', {exact: true})).toBeVisible()
  await app.getByRole('button', {name: 'Prepare follow-up'}).click()
  try { await expect(page.getByRole('region', {name: 'Review UI follow-up'})).toContainText('"hue":210') }
  catch (error) { console.log('UI fixture requests:', await page.evaluate(() => (window as any).__uiRequests.filter((r: any) => r.command === 'intelligent_ui_request'))); console.log('UI text:', await page.locator('body').innerText()); throw error }
  await page.screenshot({path: 'evidence/intelligent-ui/custom-app-browser.png', fullPage: true})
  await view.getByRole('button', {name: 'Stop app', exact: true}).click()
  await expect(page.locator('iframe[title="Color studio interactive app"]')).toHaveCount(0)
})
