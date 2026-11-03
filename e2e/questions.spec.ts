import { mkdir } from 'node:fs/promises'
import { expect, test } from '@playwright/test'

const question = {
  id: 'question-browser', threadId: 'question-thread', turnId: 'turn-1', itemId: 'input-item', agentId: 'agent-release', taskId: 'task-browser',
  kind: 'blocking', questions: [
    { id: 'purpose', header: 'Website', question: 'What should the website be about, and who is it for?' },
    { id: 'choice', header: 'Delivery', question: 'How would you like the finished website delivered?', options: [
      { label: 'Local files (Recommended)', description: 'A static website you can open and host anywhere.' },
      { label: 'Hosted with Sites', description: 'Publish through Sites and provide its URL.' },
    ] },
  ], draft: {}, resolution: 'pending', delivery: 'none', createdAt: 1, updatedAt: 1,
}

test('persists and navigates agent questions in an 800 by 600 conversation', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 600 })
  await page.addInitScript(initial => {
    const browser = window as any
    browser.__tauriCallbacks = {}
    let nextCallback = 1
    if (!localStorage.getItem('questionFixture')) localStorage.setItem('questionFixture', JSON.stringify(initial))
    const thread = { id: 'question-thread', projectId: null, cwd: '/workspace/project', workspace: {cwd: '/workspace/project', roots: ['/workspace/project'], folders: [{source: '/workspace/project', path: '/workspace/project'}], worktrees: [], scopeProjectId: null, projectRevision: null}, preview: 'Question conversation', updatedAt: 1 }
    const currentQuestions = () => JSON.parse(localStorage.getItem('questionFixture') || '[]')
    const saveQuestions = (values: unknown[]) => localStorage.setItem('questionFixture', JSON.stringify(values))
    const emit = (method: string, params: Record<string, unknown>) => Object.values(browser.__tauriCallbacks).forEach((callback: any) => callback({ event: 'engine-event', id: 1, payload: { method, params } }))
    browser.__TAURI_INTERNALS__ = {
      transformCallback: (callback: (event: unknown) => void) => { const id = nextCallback++; browser.__tauriCallbacks[id] = callback; return id },
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect') return 1
        if (command === 'plugin:event|listen') return args.handler
        if (command === 'plugin:event|unlisten') { delete browser.__tauriCallbacks[args.eventId]; return 1 }
        if (command === 'list_projects') return { data: [] }
        if (command === 'agents_request') {
          const { method, params = {} } = args
          if (method === 'agents.list') return { data: [] }
          if (method === 'agents.settings') return { maxBackground: 4, maxPerAgent: 2 }
          if (method === 'agents.decisions') return { data: [], runtimeRequests: [], drafts: [], questions: currentQuestions().filter((entry: any) => entry.resolution === 'pending' || entry.delivery === 'uncertain') }
          if (method === 'agents.tasks') return { data: [] }
          if (method === 'agents.drafts') return { data: [] }
          if (method === 'memory.projects') return { data: [] }
          if (method === 'questions.list') return { data: currentQuestions().filter((entry: any) => !params.threadId || entry.threadId === params.threadId) }
          if (method === 'questions.saveDraft') {
            const values = currentQuestions().map((entry: any) => entry.id === params.requestId ? { ...entry, draft: params.answers, updatedAt: Date.now() } : entry)
            saveQuestions(values); return values.find((entry: any) => entry.id === params.requestId)
          }
          if (method === 'questions.answer') {
            const values = currentQuestions().map((entry: any) => entry.id === params.requestId ? { ...entry, answers: params.answers, resolution: 'answered', delivery: 'queued', updatedAt: Date.now() } : entry)
            saveQuestions(values); emit('brr/questions/changed', { requestId: params.requestId, threadId: initial[0].threadId }); return values.find((entry: any) => entry.id === params.requestId)
          }
          if (method === 'questions.dismiss') {
            const values = currentQuestions().map((entry: any) => entry.id === params.requestId ? { ...entry, resolution: 'dismissed', updatedAt: Date.now() } : entry)
            saveQuestions(values); emit('brr/questions/changed', { requestId: params.requestId, threadId: initial[0].threadId }); return values.find((entry: any) => entry.id === params.requestId)
          }
          if (method === 'memory.list') return { data: [] }
          if (method === 'skills.list') return { data: [] }
          throw new Error(`Unexpected agents request ${method}`)
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
        const { method } = args
        if (method === 'account/read') return { account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' } }
        if (method === 'model/list') return { data: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }] }
        if (method === 'thread/list') return { data: [thread], nextCursor: null }
        if (method === 'thread/resume') return { thread: { ...thread, turns: [{ id: 'turn-1', status: 'inProgress', items: [] }] } }
        if (method === 'turn/interrupt') return {}
        throw new Error(`Unexpected engine request ${method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  }, [question])

  await page.goto('/')
  await page.getByRole('button', { name: 'Question conversation' }).click()
  const composer = page.getByRole('region', { name: 'Answer agent questions' })
  await expect(composer).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Send message' })).toHaveCount(0)
  await expect(page.getByRole('textbox')).toHaveCount(1)
  await page.getByRole('button', { name: 'Agents' }).click()
  const questionList = page.getByRole('region', { name: 'Pending questions' })
  await expect(questionList).toContainText('Website')
  await questionList.getByRole('button', { name: 'Open question' }).click()
  await expect(composer).toBeFocused()
  await expect(composer).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)

  await mkdir('projects/desktop-agent/evidence/questions', { recursive: true })
  await page.screenshot({ path: 'projects/desktop-agent/evidence/questions/question-composer-text-800x600.png' })
  const purposeAnswer = composer.getByRole('textbox', { name: 'What should the website be about, and who is it for?' })
  await page.keyboard.press('Tab')
  await expect(purposeAnswer).toBeFocused()
  await expect(composer.getByRole('button', { name: 'Next' })).toBeInViewport()
  await expect(page.getByRole('button', { name: 'Stop', exact: true })).toBeInViewport()
  await purposeAnswer.fill('A portfolio for an independent architect')
  await composer.getByRole('button', { name: 'Next' }).click()
  const firstChoice = composer.getByRole('radio', { name: /Local files/ })
  const secondChoice = composer.getByRole('radio', { name: /Hosted with Sites/ })
  const customAnswer = composer.getByRole('textbox', { name: 'Other answer for Delivery' })
  await firstChoice.focus()
  await expect(firstChoice).toBeFocused()
  await page.keyboard.press('ArrowDown')
  await expect(secondChoice).toBeFocused()
  await expect(secondChoice).toBeChecked()
  await page.keyboard.press('Tab')
  await expect(customAnswer).toBeFocused()
  await page.keyboard.type('Local files in my project folder')

  await expect.poll(() => page.evaluate(() => {
    const stored = JSON.parse(localStorage.getItem('questionFixture') || '[]')[0]
    return { draft: stored.draft, resolution: stored.resolution, answers: stored.answers }
  })).toEqual({ draft: { purpose: ['A portfolio for an independent architect'], choice: ['Local files in my project folder'] }, resolution: 'pending', answers: undefined })
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveCount(0)

  await page.reload()
  await page.getByRole('button', { name: 'Question conversation' }).click()
  const restoredDraft = page.getByRole('region', { name: 'Answer agent questions' })
  await expect(restoredDraft.getByRole('textbox', { name: 'What should the website be about, and who is it for?' })).toHaveValue('A portfolio for an independent architect')
  await restoredDraft.getByRole('button', { name: 'Next' }).click()
  await expect(restoredDraft.getByRole('textbox', { name: 'Other answer for Delivery' })).toHaveValue('Local files in my project folder')
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  const restoredSubmit = restoredDraft.getByRole('button', { name: 'Submit answers' })
  await expect(restoredSubmit).toBeInViewport()
  await restoredDraft.getByRole('textbox', { name: 'Other answer for Delivery' }).click()
  await page.screenshot({ path: 'projects/desktop-agent/evidence/questions/question-form-800x600.png' })
  await page.keyboard.press('Tab')
  await expect(restoredDraft.getByRole('button', { name: 'Dismiss' })).toBeFocused()
  await page.keyboard.press('Tab')
  await expect(restoredDraft.getByRole('button', { name: 'Back' })).toBeFocused()
  await page.keyboard.press('Tab')
  await expect(restoredSubmit).toBeFocused()
  await page.keyboard.press('Enter')

  const questionCardAfterSubmit = page.locator('#question-question-browser')
  await expect(questionCardAfterSubmit.getByRole('status')).toHaveText('Answer queued')
  await expect(questionCardAfterSubmit.getByText('Local files in my project folder')).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Answer agent questions' })).toHaveCount(0)
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)

  await page.reload()
  await page.getByRole('button', { name: 'Question conversation' }).click()
  const restored = page.locator('#question-question-browser')
  await expect(restored.getByRole('status')).toHaveText('Answer queued')
  await expect(restored.getByText('Local files in my project folder')).toBeVisible()
})
