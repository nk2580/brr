import { mkdir } from 'node:fs/promises'
import { expect, test, type Page } from '@playwright/test'

async function localFixture(page: Page, offline = false, multiFolder = false) {
  await page.addInitScript(({ offline, multiFolder }) => {
    const browser = window as any
    const project = { id: 'project-tickets', folders: multiFolder ? ['/synthetic/ticket-project', '/synthetic/second-folder'] : ['/synthetic/ticket-project'], primaryFolder: '/synthetic/ticket-project', revision: 1, name: 'Ticket fixture', model: null, effort: null, permission: 'read-only', hidden: false }
    const agent = { id: 'ticket-agent', name: 'Ticket helper', model: 'luna', effort: 'medium' }
    const callbacks: Record<number, (event: unknown) => void> = {}
    let callbackId = 1
    const read = () => JSON.parse(localStorage.getItem('ticket-data') || '[]')
    const write = (items: any[]) => localStorage.setItem('ticket-data', JSON.stringify(items))
    const comments = () => JSON.parse(localStorage.getItem('ticket-comments') || '[]')
    const emit = () => Object.values(callbacks).forEach(callback => callback({ event: 'engine-event', id: 1, payload: { method: 'brr/tickets/changed', params: {} } }))
    const log = (method: string, params: unknown) => {
      const calls = JSON.parse(localStorage.getItem('ticket-calls') || '[]')
      calls.push({ method, params }); localStorage.setItem('ticket-calls', JSON.stringify(calls))
    }
    browser.__TAURI_INTERNALS__ = {
      transformCallback: (callback: (event: unknown) => void) => { const id = callbackId++; callbacks[id] = callback; return id },
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect') { if (offline) throw new Error('Synthetic runtime unavailable'); return null }
        if (command === 'plugin:event|listen') return args.handler
        if (command === 'plugin:event|unlisten') { delete callbacks[args.eventId]; return null }
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
                if (command === 'list_projects') return { data: [project] }
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
        if (command === 'tickets_request') {
          const { method, params = {} } = args
          log(method, params)
          if (method === 'projects') return { data: [project] }
          if (method === 'agents') return { data: [agent] }
          if (method === 'list') return { data: read().filter((item: any) => item.projectId === params.projectId && item.archived === !!params.archived && (!params.agentId || item.agentId === params.agentId) && (!params.priority || item.priority === params.priority)), nextCursor: null }
          if (method === 'read') return { ticket: read().find((item: any) => item.id === params.ticketId), comments: { data: comments().filter((item: any) => item.ticketId === params.ticketId), nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [] }
          if (method === 'proposals') return { data: JSON.parse(localStorage.getItem('ticket-proposals') || '[]').filter((proposal: any) => !params.ticketId || proposal.ticketId === params.ticketId) }
          if (method === 'create') {
            const values = read()
            const ticket = { id: `ticket-${values.length + 1}`, number: `BRR-${values.length + 1}`, projectId: params.projectId, title: params.title, description: '', acceptanceCriteria: [], agentId: null, priority: 'none', status: 'todo', position: (values.length + 1) * 1000, revision: 1, statusRevision: 1, archived: false, createdAt: 1, updatedAt: 1 }
            write([...values, ticket]); emit(); return ticket
          }
          if (method === 'update' || method === 'move') {
            const values = read()
            const current = values.find((item: any) => item.id === params.ticketId)
            if (current.revision !== params.revision) throw JSON.stringify({ code: 'conflict', current })
            const changes = method === 'update' ? params.changes : { status: params.status }
            const updated = { ...current, ...changes, revision: current.revision + 1 }
            if (method === 'move') {
              const siblings = values.filter((item: any) => item.id !== current.id && item.status === params.status).sort((a: any, b: any) => a.position - b.position)
              const index = params.beforeId ? siblings.findIndex((item: any) => item.id === params.beforeId) : siblings.length
              siblings.splice(index < 0 ? siblings.length : index, 0, updated)
              siblings.forEach((item: any, rank: number) => { item.position = (rank + 1) * 1000 })
            }
            write(values.map((item: any) => item.id === current.id ? updated : item)); emit(); return updated
          }
          if (method === 'comment') {
            const comment = { id: `comment-${comments().length + 1}`, ticketId: params.ticketId, body: params.body, actor: { kind: 'human' }, createdAt: 1 }
            localStorage.setItem('ticket-comments', JSON.stringify([...comments(), comment])); emit(); return comment
          }
          if (method === 'proposeRun') {
            if (multiFolder && !params.folder) throw new Error('Choose a run folder')
            const ticket = read().find((item: any) => item.id === params.ticketId)
            const proposal = { id: 'proposal-1', ticketId: ticket.id, ticketRevision: ticket.revision, hash: 'fixture-review-hash', status: 'pending', snapshot: { agentId: agent.id, title: ticket.title, instructions: ticket.description, acceptanceCriteria: ticket.acceptanceCriteria.map((item: any) => item.text), projectId: project.id, cwd: params.folder ?? project.folders[0], context: `Execute fixture ticket\nBrr project snapshot: ${JSON.stringify({ id: project.id, folders: project.folders, revision: project.revision })}`, model: agent.model, effort: agent.effort, workspacePermission: 'read-only' }, createdAt: 1 }
            localStorage.setItem('ticket-proposals', JSON.stringify([proposal])); emit(); return proposal
          }
          if (method === 'approveRun') {
            localStorage.setItem('ticket-proposals', '[]')
            write(read().map((item: any) => item.id === params.ticketId ? { ...item, execution: { taskId: 'linked-task', status: 'queued', result: null, evidence: null } } : item))
            emit()
            return { id: 'linked-task', agentId: agent.id, status: 'queued', title: 'Linked ticket run', acceptanceCriteria: ['Verified'] }
          }
          if (method === 'rejectRun') { localStorage.setItem('ticket-proposals', '[]'); return {} }
          throw new Error(`Unexpected ticket request ${method}`)
        }
        if (command === 'agents_request') {
          if (args.method === 'agents.settings') return { maxBackground: 4, maxPerAgent: 2 }
          if (args.method === 'agents.decisions') return { data: [], runtimeRequests: [], drafts: [], questions: [] }
          return { data: [] }
        }
        if (command === 'engine_request') {
          if (args.method === 'account/read') return { account: { type: 'chatgpt', email: 'synthetic@example.invalid' } }
          if (args.method === 'model/list') return { data: [{ id: 'luna', model: 'luna', displayName: 'Luna', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }] }
          if (args.method === 'thread/list') return { data: [], nextCursor: null }
        }
        throw new Error(`Unexpected command ${command}: ${args.method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  }, { offline, multiFolder })
}

async function createTicket(page: Page, title: string) {
  await page.getByRole('textbox', { name: 'New ticket title' }).fill(title)
  await page.getByRole('button', { name: 'Add ticket', exact: true }).click()
  return page.getByRole('dialog', { name: 'Ticket details', exact: true })
}

test('opens the local ticket board when the coding runtime is unavailable', async ({ page }) => {
  await page.setViewportSize({ width: 800, height: 600 })
  await page.addInitScript(() => {
    const browser = window as any
    let nextCallback = 1
    browser.__TAURI_INTERNALS__ = {
      transformCallback: () => nextCallback++,
      invoke: async (command: string, args: any = {}) => {
        if (command === 'engine_connect') throw new Error('Synthetic runtime unavailable')
        if (command === 'plugin:event|listen') return args.handler
        if (command === 'plugin:event|unlisten') return null
        const project = { id: 'project-tickets', folders: ['/synthetic/ticket-project'], primaryFolder: '/synthetic/ticket-project', revision: 1, name: 'Ticket fixture', model: null, effort: null, permission: 'read-only', hidden: false }
        if (command === 'list_projects') return { data: [project] }
        if (command === 'tickets_request') {
          if (args.method === 'projects') return { data: [project] }
          if (args.method === 'agents') return { data: [] }
          if (args.method === 'list' || args.method === 'proposals') return { data: [], nextCursor: null }
          throw new Error(`Unexpected ticket request ${args.method}`)
        }
        if (command === 'agents_request') return { data: [] }
        throw new Error(`Unexpected command ${command}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  })
  await page.goto('/')
  await page.getByRole('button', { name: 'Tickets', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Todo', exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'In progress', exact: true })).toBeAttached()
  await expect(page.getByRole('heading', { name: 'Review', exact: true })).toBeAttached()
  await expect(page.getByRole('heading', { name: 'Complete', exact: true })).toBeAttached()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
})

test('creates, edits, comments, moves and restores tickets without losing the chat draft', async ({ page }) => {
  await localFixture(page)
  await page.goto('/')
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Keep my unfinished chat')
  await page.getByRole('button', { name: 'Tickets', exact: true }).click()
  const detail = await createTicket(page, 'Investigate startup')
  await detail.getByRole('textbox', { name: 'Description', exact: true }).fill('Check startup with a read-only diagnostic.')
  await detail.getByRole('button', { name: 'Save description' }).click()
  await detail.getByRole('combobox', { name: 'Priority', exact: true }).selectOption('high')
  await detail.getByRole('combobox', { name: 'Assigned agent', exact: true }).selectOption('ticket-agent')
  await detail.getByRole('textbox', { name: 'Comment', exact: true }).fill('Reproduce before changing code.')
  await detail.getByRole('button', { name: 'Comment', exact: true }).click()
  await expect(detail.getByText('Reproduce before changing code.', { exact: true })).toBeVisible()
  await detail.getByRole('combobox', { name: 'Status', exact: true }).selectOption('review')
  await detail.getByRole('button', { name: 'Archive', exact: true }).click()
  await expect(page.getByRole('button', { name: 'BRR-1 Investigate startup', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Archived', exact: true }).click()
  await page.getByRole('button', { name: 'BRR-1 Investigate startup', exact: true }).click()
  await detail.getByRole('button', { name: 'Restore', exact: true }).click()
  await page.getByRole('button', { name: 'Home', exact: true }).click()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveText('Keep my unfinished chat')
  await page.getByRole('button', { name: 'Tickets', exact: true }).click()
  await page.getByRole('button', { name: 'BRR-1 Investigate startup', exact: true }).click()
  await expect(detail.getByRole('textbox', { name: 'Description', exact: true })).toHaveValue('Check startup with a read-only diagnostic.')
  await page.reload()
  await page.getByRole('button', { name: 'Tickets', exact: true }).click()
  await page.getByRole('button', { name: 'BRR-1 Investigate startup', exact: true }).click()
  await expect(detail.getByRole('combobox', { name: 'Status', exact: true })).toHaveValue('review')
})

test('keeps conflicting text until the user chooses how to reconcile it', async ({ page }) => {
  await localFixture(page)
  await page.goto('/')
  await page.getByRole('button', { name: 'Tickets', exact: true }).click()
  const detail = await createTicket(page, 'Concurrent edit')
  await detail.getByRole('textbox', { name: 'Description', exact: true }).fill('My unsaved description')
  await page.evaluate(() => {
    const values = JSON.parse(localStorage.getItem('ticket-data') || '[]')
    values[0].description = 'Another editor changed this'
    values[0].revision += 1
    localStorage.setItem('ticket-data', JSON.stringify(values))
  })
  await detail.getByRole('button', { name: 'Save description' }).click()
  await expect(page.getByRole('alert')).toContainText('changed elsewhere')
  await expect(detail.getByRole('textbox', { name: 'Description', exact: true })).toHaveValue('My unsaved description')
  await detail.getByRole('button', { name: 'Save description' }).click()
  await expect(page.getByRole('alert')).toHaveCount(0)
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('ticket-data') || '[]')[0].description)).toBe('My unsaved description')
})

test('moves cards with the pointer and keyboard at the minimum window size', async ({ page }) => {
  await localFixture(page, true)
  await page.setViewportSize({ width: 800, height: 600 })
  await page.goto('/')
  await page.getByRole('button', { name: 'Tickets', exact: true }).click()
  const detail = await createTicket(page, 'First pointer card')
  await detail.getByRole('button', { name: 'Close ticket details' }).click()
  await createTicket(page, 'Second keyboard card')
  await detail.getByRole('button', { name: 'Close ticket details' }).click()
  const secondMove = page.getByRole('combobox', { name: 'Move BRR-2', exact: true })
  await secondMove.focus()
  await page.keyboard.press('i')
  await page.keyboard.press('Enter')
  await expect(secondMove).toHaveValue('in_progress')
  const card = page.locator('article.ticket-card').filter({ hasText: 'First pointer card' })
  await card.dragTo(page.getByRole('region', { name: 'In progress', exact: true }))
  await expect(page.getByRole('region', { name: 'In progress', exact: true }).getByText('First pointer card', { exact: true })).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
  await mkdir('projects/tickets/evidence', { recursive: true })
  await page.screenshot({ path: 'projects/tickets/evidence/board-800x600.png' })
})

test('reviews the exact execution settings before queuing linked agent work', async ({ page }) => {
  await localFixture(page)
  await page.goto('/')
  await page.getByRole('button', { name: 'Tickets', exact: true }).click()
  const detail = await createTicket(page, 'Review before execution')
  await detail.getByRole('textbox', { name: 'Description', exact: true }).fill('Read marker.txt and publish the exact observed marker.')
  await detail.getByRole('button', { name: 'Save description' }).click()
  await detail.getByRole('combobox', { name: 'Assigned agent', exact: true }).selectOption('ticket-agent')
  await detail.getByRole('textbox', { name: 'New acceptance criterion' }).fill('The marker is verified with command evidence.')
  await detail.getByRole('button', { name: 'Add', exact: true }).click()
  await detail.getByRole('button', { name: 'Save checklist' }).click()
  await detail.getByRole('button', { name: 'Propose agent run' }).click()
  const review = detail.getByRole('region', { name: 'Review proposed agent run' })
  await expect(review).toContainText('/synthetic/ticket-project')
  await expect(review).toContainText('read-only')
  await expect(review).toContainText('luna')
  await expect(review).toContainText('medium')
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('ticket-calls') || '[]').filter((call: any) => call.method === 'approveRun').length)).toBe(0)
  await mkdir('projects/tickets/evidence', { recursive: true })
  await page.screenshot({ path: 'projects/tickets/evidence/review-before-run.png' })
  await review.getByRole('button', { name: 'Approve and run' }).click()
  await page.getByRole('button', { name: 'Tickets', exact: true }).click()
  await expect(page.getByLabel('Agent execution: queued')).toBeVisible()
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('ticket-calls') || '[]').filter((call: any) => call.method === 'approveRun').length)).toBe(1)
})

test('requires a folder choice for a multi-folder ticket run', async ({ page }) => {
  await localFixture(page, false, true)
  await page.goto('/')
  await page.getByRole('button', { name: 'Tickets', exact: true }).click()
  const detail = await createTicket(page, 'Run in the second folder')
  await detail.getByRole('textbox', { name: 'Description', exact: true }).fill('Inspect the selected folder and publish evidence.')
  await detail.getByRole('button', { name: 'Save description' }).click()
  await detail.getByRole('combobox', { name: 'Assigned agent', exact: true }).selectOption('ticket-agent')
  await detail.getByRole('textbox', { name: 'New acceptance criterion' }).fill('The selected folder was inspected.')
  await detail.getByRole('button', { name: 'Add', exact: true }).click()
  await detail.getByRole('button', { name: 'Save checklist' }).click()
  await expect(detail.getByRole('button', { name: 'Propose agent run' })).toBeDisabled()
  await detail.getByRole('combobox', { name: 'Run folder', exact: true }).selectOption('/synthetic/second-folder')
  await detail.getByRole('button', { name: 'Propose agent run' }).click()
  const review = detail.getByRole('region', { name: 'Review proposed agent run' })
  await expect(review).toContainText('/synthetic/second-folder')
  await expect(review).toContainText('/synthetic/ticket-project')
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('ticket-calls') || '[]').find((call: any) => call.method === 'proposeRun').params.folder)).toBe('/synthetic/second-folder')
})
