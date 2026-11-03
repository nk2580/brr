import { test, expect, type Page } from '@playwright/test'

const profile = {
  id: 'agent-release', revision: 1, createdAt: 1, updatedAt: 1,
  name: 'Release guide', description: 'Helps ship safely',
  instructions: 'Review changes and release risks.', soul: 'Calm, direct, evidence-led.',
  model: 'gpt-6.1-sol', effort: 'high',
}
const candidate = {
  name: profile.name, description: profile.description, instructions: profile.instructions,
  soul: profile.soul, model: profile.model, effort: profile.effort,
}

async function connectAgentsFixture(page: Page) {
  await page.addInitScript(({ initialProfile, initialCandidate }) => {
    const browser = window as any
    browser.__agentEngineRequests = []
    browser.__agentRequests = []
    browser.__tauriCallbacks = {}
    let nextCallback = 1
    let created = false
    let forgotten = false
    let mentionTask: any = null
    const draft = { id: 'draft-release', threadId: 'interview-1', candidate: initialCandidate, hash: 'sha256:release', status: 'proposed' }
    const memory = { id: 'memory-release', scope: 'agent', scopeId: initialProfile.id, kind: 'preference', content: 'Include release notes in the checklist.', revision: 1, sourceIds: ['source-release'], createdAt: 1, updatedAt: 1 }
    const source = { sourceId: 'source-release', revision: 2, threadId: 'memory-source-thread', agentId: initialProfile.id, content: 'Please include release notes in future checklists.', active: true }
    const project = {id: 'project-atlas', name: 'Atlas', folders: ['/workspace/atlas'], primaryFolder: '/workspace/atlas', revision: 1, model: null, effort: null, permission: null, hidden: false}
    const thread = (id: string, preview: string, brr?: unknown) => ({ id, projectId: 'project-atlas', cwd: '/workspace/atlas', preview, updatedAt: 1, workspace: {cwd: '/workspace/atlas', roots: ['/workspace/atlas'], folders: [{source: '/workspace/atlas', path: '/workspace/atlas'}], worktrees: [], scopeProjectId: 'project-atlas', projectRevision: 1}, ...(brr ? { brr } : {}) })
    browser.__TAURI_INTERNALS__ = {
      transformCallback: (callback: (event: unknown) => void) => {
        const id = nextCallback++
        browser.__tauriCallbacks[id] = callback
        return id
      },
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect') return 1
        if (command === 'plugin:event|listen') return args.handler
        if (command === 'plugin:event|unlisten') { delete browser.__tauriCallbacks[args.eventId]; return 1 }
        if (command === 'plugin:dialog|open') return '/workspace/atlas'
        if (command === 'agents_request') {
          browser.__agentRequests.push(args)
          const { method, params = {} } = args
          if (method === 'agents.list') return { data: created ? [initialProfile] : [] }
          if (method === 'agents.settings') return { maxBackground: 4, maxPerAgent: 2 }
          if (method === 'agents.settings.update') return params
          if (method === 'agents.decisions') return { data: [], runtimeRequests: [], drafts: created ? [] : [draft] }
          if (method === 'questions.list') return { data: [] }
          if (method === 'agents.drafts') return { data: created ? [] : [draft] }
          if (method === 'agents.interview') return { thread: thread('interview-1', 'New agent interview') }
          if (method === 'agents.edit') return { thread: thread('profile-edit-1', 'Edit Release guide') }
          if (method === 'agents.approve') {
            created = true
            for (const callback of Object.values(browser.__tauriCallbacks) as Array<(event: unknown) => void>) {
              callback({ event: 'engine-event', id: 1, payload: { method: 'brr/agents/changed', params: {} } })
            }
            return { agent: initialProfile }
          }
          if (method === 'agents.reject') return {}
          if (method === 'agents.sessions') return { data: [thread('agent-session-1', 'Release review', { agentId: initialProfile.id, kind: 'agent' })] }
          if (method === 'agents.open') return { thread: {...thread('agent-session-new', 'New Release guide session', { agentId: initialProfile.id, kind: 'agent' }), projectId: params.projectId ?? null} }
          if (method === 'agents.workspace.inspect') return { git: false, commands: [] }
          if (method === 'tasks.create') {
            mentionTask = { id: 'mention-task-1', agentId: params.agentId, title: params.title, instructions: params.instructions, acceptanceCriteria: params.acceptanceCriteria, status: 'queued', createdAt: 2, updatedAt: 2 }
            return mentionTask
          }
          if (method === 'tasks.list') return { data: mentionTask ? [mentionTask] : [], nextCursor: null }
          if (method === 'tasks.proposals') return { data: [] }
          if (method === 'workflows.list') return { data: [] }
          if (method === 'memory.projects') return { data: ['/workspace/atlas'] }
          if (method === 'memory.list') return { data: forgotten ? [] : [memory], sources: forgotten ? [] : [source] }
          if (method === 'memory.search') return { data: forgotten ? [] : [memory], sources: forgotten ? [] : [source] }
          if (method === 'memory.forget') { forgotten = true; return { eventId: 23 } }
          if (method === 'memory.undo') { forgotten = false; return {} }
          if (method === 'skills.list') return { data: [] }
          if (method === 'agents.events') return { data: [] }
          throw new Error(`Unexpected agents request ${method}`)
        }
        if (command === 'list_projects') return { data: [project] }
        if (command === 'create_project') return {id: `created-${args.project.name}`, revision: 1, ...args.project, hidden: false}
        if (command === 'save_project') return { ...args.project, revision: args.project.revision + 1, hidden: false }
        if (command === 'delete_project' || command === 'move_chat_to_project') return null
        if (command === 'chat_request') {
          const {method, params = {}} = args
          ;((window as any).__chatRequests ??= []).push({method, params})
          if (method === 'tasks.assign') {
            ;((window as any).__taskAssignmentRequests ??= []).push(params)
            mentionTask = { id: 'mention-task-1', ...params.task, status: 'queued', createdAt: 2, updatedAt: 2 }
            return {task: mentionTask}
          }
          if (method === 'drafts.list' || method === 'attachments.list') return []
          if (method === 'drafts.get') throw new Error('No saved fixture draft')
          if (method === 'drafts.save') return {...params, id: params.id, threadId: params.threadId ?? null, projectId: params.projectId ?? null, agentId: params.agentId ?? null, revision: (params.expectedRevision ?? 0) + 1, updatedAt: '2026-11-02T00:00:00Z'}
          if (method === 'drafts.promote') return {id: params.id, threadId: params.threadId, projectId: params.projectId ?? null, agentId: params.agentId ?? null, document: params.document ?? {text: '', references: [], quotes: []}, settings: params.settings ?? {model: '', effort: 'medium', mode: 'default'}, revision: 1, updatedAt: '2026-11-02T00:00:00Z'}
          if (method === 'foreground.submit') return {turn: {id: 'fixture-turn'}, turnId: 'fixture-turn', submission: {id: 'fixture-submission', threadId: params.threadId, status: 'accepted', revision: 1}, queued: false}
          if (method === 'queue.list') return {entries: [], state: {paused: false}}
          return {}
        }
        if (command !== 'engine_request') throw new Error(`Unexpected command ${command}`)
        browser.__agentEngineRequests.push(args)
        const { method } = args
        if (method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (method === 'model/list') return { data: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'medium', description: 'Balanced' }, { reasoningEffort: 'high', description: 'Thorough' }] }] }
        if (method === 'thread/list') return { data: [], nextCursor: null }
        if (method === 'thread/resume') return { thread: thread(args.params.threadId, 'Agent conversation', { agentId: initialProfile.id, kind: 'agent' }) }
        if (method === 'thread/turns/list') return { data: [], nextCursor: null }
        if (method === 'brr/project/inspect') return { commands: [] }
        if (method === 'brr/project/approve') return { approved: true }
        if (method === 'turn/start') return { turn: { id: 'turn-1' } }
        if (method === 'turn/interrupt') return {}
        throw new Error(`Unexpected engine request ${method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  }, { initialProfile: profile, initialCandidate: candidate })
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'Agents' })).toBeVisible()
}

test('Agents supports interview, review, named chats, separate mention tasks, and compact keyboard use', async ({ page }) => {
  const pageErrors: string[] = []
  page.on('pageerror', error => pageErrors.push(error.message))
  await connectAgentsFixture(page)

  // Choose a project first so both named sessions and mention tasks carry the project path.
  await page.getByRole('combobox', { name: 'Project' }).selectOption('project-atlas')
  await page.getByRole('button', { name: 'Agents' }).click()
  await page.getByRole('complementary', { name: 'Agent list' }).getByRole('button', { name: 'New agent' }).click()
  await expect(page.getByRole('region', { name: 'Agents' })).toHaveCount(0)
  await expect.poll(() => page.evaluate(() => (window as any).__agentRequests.some((request: any) => request.method === 'agents.interview'))).toBe(true)
  await page.getByRole('button', { name: 'Chat options' }).click()
  await expect(page.getByRole('combobox', { name: 'Chat identity' })).toHaveValue('')

  // Returning to Agents loads the pending proposal generated by the interview thread.
  await page.getByRole('button', { name: 'Agents' }).click()
  const review = page.getByRole('region', { name: 'Review agent profile' })
  await expect(review).toContainText('instructions.md')
  await expect(review).toContainText('Review changes and release risks.')
  await expect(review).toContainText('soul.md')
  await page.screenshot({ path: 'projects/agents/evidence/agents-profile-review.png' })
  await review.getByRole('button', { name: 'Create agent' }).click()
  await expect(page.getByRole('heading', { name: 'Release guide' }).first()).toBeVisible()
  await expect.poll(() => page.evaluate(() => (window as any).__agentRequests.some((request: any) => request.method === 'agents.approve' && request.params.hash === 'sha256:release'))).toBe(true)

  // A named session opens in the existing chat, with the agent's saved model and effort.
  await page.getByRole('button', { name: 'New session' }).click()
  await expect.poll(() => page.evaluate(() => (window as any).__agentRequests.some((request: any) => request.method === 'agents.open' && request.params.agentId === 'agent-release' && request.params.projectId === 'project-atlas'))).toBe(true)
  await page.getByRole('button', { name: 'Chat options' }).click()
  await expect(page.getByRole('combobox', { name: 'Chat identity' })).toHaveValue('agent-release')
  await expect(page.getByRole('combobox', { name: 'Model' })).toHaveValue('sol')
  await expect(page.getByRole('combobox', { name: 'Reasoning effort' })).toHaveValue('high')
  await expect(page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).resolves.toBe(true)
  await page.screenshot({ path: 'projects/agents/evidence/agents-named-chat.png' })

  // @mention creates a separate task and carries only the message text, not chat history.
  const message = page.getByRole('textbox', { name: 'Message' })
  await message.fill('Review the next release. @Release guide list concrete blockers.')
  await page.getByRole('button', { name: 'Send message' }).click()
  await expect(page.getByRole('status')).toContainText('Separate task assigned to Release guide')
  const mentionRequest = await page.evaluate(() => (window as any).__taskAssignmentRequests[0])
  expect(mentionRequest).toMatchObject({
    document: {text: 'Review the next release. @Release guide list concrete blockers.'},
    task: {
    agentId: 'agent-release', instructions: 'Review the next release. list concrete blockers.', projectId: 'project-atlas',
    acceptanceCriteria: [expect.any(String)],
    sourceThreadId: 'agent-session-new',
    },
  })
  expect(await page.evaluate(() => (window as any).__agentEngineRequests.some((request: any) => request.method === 'turn/start'))).toBe(false)

  // Reach agent management tabs via keyboard and inspect memory evidence and profile.
  await page.getByRole('button', { name: 'Agents' }).click()
  const tasksTab = page.getByRole('button', { name: 'Tasks' })
  await tasksTab.focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('region', { name: 'Agent tasks' })).toBeVisible()
  await expect(page.locator('.agents-task-record > div > strong')).toHaveText('Review the next release. list concrete blockers.')
  await expect(page.getByRole('spinbutton', { name: 'Total background workers' })).toHaveValue('4')
  await expect(page.getByRole('spinbutton', { name: 'Workers per agent' })).toHaveValue('2')
  await expect(page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).resolves.toBe(true)
  await page.screenshot({ path: 'projects/agents/evidence/agents-tasks.png' })
  await page.getByRole('button', { name: 'Memory' }).click()
  await expect(page.getByText('Include release notes in the checklist.')).toBeVisible()
  await page.getByText('Source · revision 2').click()
  await expect(page.getByText('Please include release notes in future checklists.')).toBeVisible()
  await page.screenshot({ path: 'projects/agents/evidence/agents-memory-provenance.png' })
  await page.getByRole('region', { name: 'Agents', exact: true }).getByRole('button', { name: 'Profile', exact: true }).click()
  await expect(page.getByRole('region', { name: 'Agent profile' })).toContainText('Calm, direct, evidence-led.')

  // Compact 800x600 acceptance: reachable tabs, controls, and no horizontal page overflow.
  await page.setViewportSize({ width: 800, height: 600 })
  await expect(page.getByRole('region', { name: 'Agents', exact: true }).getByRole('button', { name: 'Profile', exact: true })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Agent profile' }).getByRole('button', { name: 'Edit through chat' })).toBeVisible()
  await expect(page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).resolves.toBe(true)
  await page.getByRole('button', { name: 'Tasks' }).focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('region', { name: 'Agent tasks' })).toBeVisible()
  await expect(page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).resolves.toBe(true)
  await page.screenshot({ path: 'projects/agents/evidence/agents-tasks-800x600.png' })
  await page.getByRole('region', { name: 'Agents', exact: true }).getByRole('button', { name: 'Profile', exact: true }).focus()
  await page.keyboard.press('Enter')
  await expect(page.getByRole('region', { name: 'Agent profile' })).toBeVisible()
  await page.screenshot({ path: 'projects/agents/evidence/agents-profile-800x600.png' })
  await expect(page.getByRole('button', { name: 'Tasks' })).toBeEnabled()
  await expect(page.getByRole('button', { name: 'Memory' })).toBeEnabled()
  expect(pageErrors).toEqual([])
})
