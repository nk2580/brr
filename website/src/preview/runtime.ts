type RpcArgs = { method?: string; params?: Record<string, any>; [key: string]: any }
type PreviewThread = { id: string; projectId: string; cwd: string; preview: string; updatedAt: number; workspace: any; turns: any[] }

const root = '/preview/workspace/brr'
const projects = [
  { id: 'preview-project-writing', name: 'Writing desk', folders: [root], primaryFolder: root, model: null, effort: null, permission: 'read-only', revision: 1, hidden: false },
  { id: 'preview-project-research', name: 'Research notes', folders: [root], primaryFolder: root, model: null, effort: null, permission: 'read-only', revision: 1, hidden: false },
  { id: 'preview-project-making', name: 'Small tools', folders: [root], primaryFolder: root, model: null, effort: null, permission: 'read-only', revision: 1, hidden: false },
]
const workspace = (projectId: string) => ({ cwd: root, roots: [root], folders: [{ source: root, path: root }], worktrees: [], scopeProjectId: projectId, projectRevision: 1 })
const turn = (id: string, userText: string, assistantText: string, extra: any[] = []) => ({
  id, status: 'completed', items: [
    { id: `${id}-user`, type: 'userMessage', turnId: id, turnStatus: 'completed', text: userText },
    ...extra,
    { id: `${id}-assistant`, type: 'agentMessage', turnId: id, turnStatus: 'completed', text: assistantText },
  ],
})
const threadsSeed: PreviewThread[] = [
  {
    id: 'preview-writing', projectId: projects[0].id, cwd: root, preview: 'Shape a short welcome note', updatedAt: 3, workspace: workspace(projects[0].id),
    turns: [turn('writing-1', 'Help me write a warm welcome note for new contributors.', 'Welcome! We’re glad you’re here. Start with the README, pick a small issue, and ask questions as you go.', [
      { id: 'writing-file', type: 'fileChange', turnId: 'writing-1', turnStatus: 'completed', changes: [{ path: 'README.md', diff: '@@ -1,2 +1,5 @@\n+# Welcome\n+Thanks for joining the project.' }] },
    ])],
  },
  {
    id: 'preview-research', projectId: projects[1].id, cwd: root, preview: 'Compare two research directions', updatedAt: 2, workspace: workspace(projects[1].id),
    turns: [turn('research-1', 'Summarize what to compare before choosing a direction.', 'Compare the audience, evidence quality, effort, and what decision each direction would change.')],
  },
  {
    id: 'preview-making', projectId: projects[2].id, cwd: root, preview: 'Improve the project README', updatedAt: 1, workspace: workspace(projects[2].id),
    turns: [turn('making-1', 'Can you make this README easier to scan?', 'A clearer README starts with one sentence about the project, followed by setup steps and a short example.')],
  },
]

function clone<T>(value: T): T { return structuredClone(value) }
function previewOnly(action: string): Error { return new Error(`${action} is unavailable in this preview-only workspace.`) }

export function createPreviewRuntime() {
  const state: any = {
    threads: clone(threadsSeed), drafts: [], attachments: [], queue: [], pausedThreads: new Set<string>(), activeRuns: new Map<string, any>(),
    requests: [] as Array<{ method: string; params: any }>, engineRequests: [] as string[], agentCalls: [] as string[],
    callbacks: new Map<number, (event: unknown) => void>(), callbackRegistry: new Map<number, (event: unknown) => void>(), nextCallback: 1, nextId: 1,
  }
  const threadFor = (id: string) => state.threads.find((thread: PreviewThread) => thread.id === id)
  const emit = (method: string, params: any) => {
    const event = { event: 'engine-event', id: 1, payload: { method, params } }
    for (const callback of state.callbacks.values()) callback(event)
  }
  const id = (prefix: string) => `${prefix}-${state.nextId++}`

  const engineRequest = async (method: string, params: any) => {
    if (!['account/read', 'model/list', 'thread/list', 'thread/resume', 'thread/turns/list', 'skills/list', 'brr/project/inspect', 'thread/start', 'turn/interrupt'].includes(method)) throw previewOnly(`Engine request “${method}”`)
    state.engineRequests.push(method)
    if (method === 'account/read') return { account: { type: 'chatgpt', email: 'preview@example.invalid', planType: 'preview' } }
    if (method === 'model/list') return { data: [
      { id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'low', description: 'Fast' }, { reasoningEffort: 'medium', description: 'Balanced' }, { reasoningEffort: 'high', description: 'Thorough' }] },
      { id: 'astra', model: 'gpt-6-astra', displayName: 'GPT Astra', isDefault: false, defaultReasoningEffort: 'high', supportedReasoningEfforts: [{ reasoningEffort: 'medium', description: 'Balanced' }, { reasoningEffort: 'high', description: 'Thorough' }] },
    ] }
    if (method === 'thread/list') return { data: clone(state.threads.map(({ turns: _turns, ...thread }: PreviewThread) => thread)), nextCursor: null }
    if (method === 'thread/resume') {
      const thread = threadFor(params.threadId)
      if (!thread) throw new Error('Preview conversation not found.')
      return { thread: clone(thread) }
    }
    if (method === 'thread/turns/list') return { data: clone(threadFor(params.threadId)?.turns ?? []), nextCursor: null }
    if (method === 'skills/list') return { data: [{ skills: [{ name: 'README helper', path: `${root}/.preview/skills/readme-helper/SKILL.md`, description: 'Help organize a README', enabled: true }] }] }
    if (method === 'brr/project/inspect') return { commands: [] }
    if (method === 'thread/start') {
      const selectedProject = projects.find(project => project.id === params.projectId) ?? projects[0]
      const thread = { id: id('preview-chat'), projectId: selectedProject.id, cwd: root, preview: 'A new preview conversation', updatedAt: Date.now(), workspace: workspace(selectedProject.id), turns: [] }
      state.threads.unshift(thread)
      return { thread: clone(thread) }
    }
    if (method === 'turn/interrupt') {
      const active = state.activeRuns.get(params.turnId)
      if (active && active.threadId === params.threadId) {
        for (const timer of active.timers) clearTimeout(timer)
        active.turn.status = 'interrupted'
        for (const item of active.turn.items) item.turnStatus = 'interrupted'
        state.activeRuns.delete(params.turnId)
        emit('turn/completed', { threadId: params.threadId, turn: clone(active.turn) })
      } else {
        queueMicrotask(() => emit('turn/completed', { threadId: params.threadId, turn: { id: params.turnId, status: 'interrupted', items: [] } }))
      }
      return {}
    }
    throw new Error(`Preview engine handler missing for “${method}”.`)
  }

  const agentsRequest = async (method: string, params: any) => {
    state.agentCalls.push(method)
    if (method === 'agents.list') return { data: [{ id: 'preview-research-partner', revision: 1, createdAt: 1, updatedAt: 1, name: 'Research partner', description: 'Helps compare ideas and find clear next steps.', instructions: 'Offer concise, evidence-minded research support.', soul: 'Curious and practical.', model: 'gpt-6.1-sol', effort: 'medium' }] }
    if (method === 'agents.decisions') return { data: [], runtimeRequests: [], drafts: [], questions: [] }
    if (method === 'agents.settings') return { maxBackground: 0, maxPerAgent: 0 }
    if (['agents.drafts', 'agents.sessions', 'memory.list', 'memory.search', 'memory.projects', 'skills.list', 'tasks.list', 'tasks.proposals', 'workflows.list', 'agents.events', 'questions.list'].includes(method)) return { data: [], nextCursor: null, sources: [] }
    if (method === 'agents.open') throw previewOnly('Opening a separate agent session')
    if (params && ['agents.read'].includes(method)) throw previewOnly('Editing agent profiles')
    throw previewOnly(`Agent action “${method}”`)
  }

  const chatRequest = async (method: string, params: any): Promise<any> => {
    state.requests.push({ method, params: clone(params) })
    if (method === 'drafts.list') return clone(state.drafts.filter((draft: any) => draft.document?.text || draft.document?.references?.length || draft.document?.quotes?.length || draft.document?.attachmentIds?.length))
    if (method === 'drafts.get') {
      const draft = state.drafts.find((entry: any) => entry.id === params.id)
      if (!draft) throw new Error('Preview draft not found.')
      return clone(draft)
    }
    if (method === 'drafts.save') {
      const index = state.drafts.findIndex((entry: any) => entry.id === params.id)
      const revision = index < 0 ? 0 : state.drafts[index].revision
      if (revision !== params.expectedRevision) throw new Error('Preview draft revision conflict.')
      const record = { ...clone(params), threadId: params.threadId ?? null, projectPath: params.projectPath ?? null, agentId: params.agentId ?? null, revision: revision + 1, updatedAt: new Date().toISOString() }
      if (index < 0) state.drafts.push(record); else state.drafts[index] = record
      return clone(record)
    }
    if (method === 'drafts.promote') {
      const index = state.drafts.findIndex((entry: any) => entry.id === params.id)
      const record = state.drafts[index] ?? { id: params.id, document: { version: 1, text: '', references: [], quotes: [], attachmentIds: [] }, settings: {}, revision: 0 }
      if (index >= 0) state.drafts.splice(index, 1)
      const promoted = { ...record, threadId: params.threadId, revision: record.revision + 1, updatedAt: new Date().toISOString() }
      state.drafts.push(promoted)
      return clone(promoted)
    }
    if (method === 'drafts.delete') { state.drafts = state.drafts.filter((entry: any) => entry.id !== params.id); return { deleted: true } }
    if (method === 'attachments.list') return clone(state.attachments.filter((entry: any) => entry.ownerId === params.ownerId).map(({ ownerId: _owner, content: _content, ...entry }: any) => entry))
    if (method === 'attachments.import' || method === 'attachments.importBytes') {
      const files = method === 'attachments.importBytes' ? (params.files ?? []).map((file: any) => ({ name: file.name, mime: file.mime, content: 'Synthetic preview attachment' })) : (params.paths ?? []).map((path: string) => ({ name: path.split('/').at(-1), mime: 'text/plain', content: 'Synthetic preview attachment' }))
      const imported = files.map((file: any) => {
        const attachment = { id: id('preview-attachment'), ownerId: params.ownerId, name: file.name, mime: file.mime || 'text/plain', size: file.content.length, kind: file.mime?.startsWith('image/') ? 'image' : 'file', status: 'ready', content: file.content }
        state.attachments.push(attachment)
        return attachment
      })
      return clone(imported.map(({ ownerId: _owner, content: _content, ...entry }: any) => entry))
    }
    if (method === 'attachments.preview') {
      const attachment = state.attachments.find((entry: any) => entry.ownerId === params.ownerId && entry.id === params.id)
      if (!attachment) throw new Error('Preview attachment not found.')
      return { attachment: clone(Object.fromEntries(Object.entries(attachment).filter(([key]) => !['ownerId', 'content'].includes(key)))), preview: attachment.kind === 'file' ? attachment.content : null, ...(attachment.kind === 'image' ? { reason: 'Synthetic images are not rendered in preview.' } : {}) }
    }
    if (method === 'attachments.remove') { state.attachments = state.attachments.filter((entry: any) => !(entry.ownerId === params.ownerId && entry.id === params.id)); return { removed: true } }
    if (method === 'foreground.submit') {
      const thread = threadFor(params.threadId)
      if (!thread) throw new Error('Preview conversation not found.')
      const submission: { id: string; threadId: string; document: any; attachmentIds: string[]; settings: any; delivery: string; expectedTurnId?: string; status: string; revision: number; createdAt: string; turnId?: string } = { id: params.id ?? id('preview-submission'), threadId: params.threadId, document: clone(params.document), attachmentIds: params.attachmentIds ?? [], settings: clone(params.settings), delivery: params.delivery, expectedTurnId: params.expectedTurnId, status: params.delivery === 'queue' ? 'queued' : 'accepted', revision: 1, createdAt: new Date().toISOString() }
      if (params.delivery === 'queue') { state.queue.push(submission); return { submission: clone(submission), queued: true } }
      if (params.delivery === 'steer') {
        const active = state.activeRuns.get(params.expectedTurnId)
        if (!active || active.threadId !== thread.id) throw new Error('There is no active preview turn to steer.')
        const text = String(params.document?.text ?? '').trim()
        const userItem = { id: `${active.turn.id}-steer-${submission.id}`, type: 'userMessage', turnId: active.turn.id, turnStatus: 'inProgress', text }
        const responseItem = { id: `${active.turn.id}-steer-response-${submission.id}`, type: 'agentMessage', turnId: active.turn.id, turnStatus: 'completed', text: `Preview response: I’ve added “${text || 'your follow-up'}” to the current turn. This local demo does not contact a model or external service.` }
        active.turn.items.push(userItem)
        thread.preview = text || thread.preview
        thread.updatedAt = Date.now()
        emit('item/started', { threadId: thread.id, turnId: active.turn.id, item: userItem })
        active.timers.push(setTimeout(() => {
          if (state.activeRuns.get(active.turn.id) !== active) return
          userItem.turnStatus = 'completed'
          emit('item/completed', { threadId: thread.id, turnId: active.turn.id, item: userItem })
        }, 150))
        active.timers.push(setTimeout(() => {
          if (state.activeRuns.get(active.turn.id) !== active) return
          active.turn.items.push({ ...responseItem, turnStatus: 'inProgress' })
          emit('item/started', { threadId: thread.id, turnId: active.turn.id, item: { ...responseItem, turnStatus: 'inProgress' } })
        }, 800))
        active.timers.push(setTimeout(() => {
          if (state.activeRuns.get(active.turn.id) !== active) return
          active.turn.items = active.turn.items.map((item: any) => item.id === responseItem.id ? responseItem : item)
          emit('item/completed', { threadId: thread.id, turnId: active.turn.id, item: responseItem })
        }, 1200))
        if (active.completionTimer) clearTimeout(active.completionTimer)
        active.completionTimer = setTimeout(() => {
          if (state.activeRuns.get(active.turn.id) !== active) return
          active.turn.status = 'completed'
          active.turn.items = active.turn.items.map((item: any) => ({ ...item, turnStatus: 'completed' }))
          state.activeRuns.delete(active.turn.id)
          emit('turn/completed', { threadId: thread.id, turn: clone(active.turn) })
        }, 1500)
        active.timers.push(active.completionTimer)
        return { submission: { ...submission, turnId: active.turn.id }, queued: false, turn: { id: active.turn.id }, turnId: active.turn.id }
      }
      const turnId = params.expectedTurnId ?? id('preview-turn')
      const text = String(params.document?.text ?? '').trim()
      const response = `Preview response: I’ve received “${text || 'your message'}”. This conversation is running in a local demo, so no model or external service was contacted.`
      const items = [
        { id: `${turnId}-user`, type: 'userMessage', turnId, turnStatus: 'completed', text },
        { id: `${turnId}-assistant`, type: 'agentMessage', turnId, turnStatus: 'completed', text: response },
      ]
      const historyTurn = { id: turnId, status: 'inProgress', items: [{ ...items[0] }] }
      thread.turns.push(historyTurn)
      thread.preview = text || thread.preview
      thread.updatedAt = Date.now()
      const timers: ReturnType<typeof setTimeout>[] = []
      const active = { threadId: thread.id, turn: historyTurn, timers, completionTimer: null as ReturnType<typeof setTimeout> | null }
      state.activeRuns.set(turnId, active)
      emit('turn/started', { threadId: thread.id, turn: { id: turnId, status: 'inProgress', items: [] } })
      emit('item/started', { threadId: thread.id, turnId, item: { ...items[0], turnStatus: 'inProgress' } })
      timers.push(setTimeout(() => emit('item/completed', { threadId: thread.id, turnId, item: items[0] }), 200))
      timers.push(setTimeout(() => {
        if (state.activeRuns.get(turnId) !== active) return
        historyTurn.items.push({ ...items[1], turnStatus: 'inProgress' })
        emit('item/started', { threadId: thread.id, turnId, item: { ...items[1], turnStatus: 'inProgress' } })
      }, 1500))
      timers.push(setTimeout(() => {
        if (state.activeRuns.get(turnId) !== active) return
        historyTurn.items = historyTurn.items.map((item: any) => item.id === items[1].id ? { ...items[1] } : item)
        emit('item/completed', { threadId: thread.id, turnId, item: items[1] })
      }, 2200))
      const completionTimer = setTimeout(() => {
        if (state.activeRuns.get(turnId) !== active) return
        historyTurn.status = 'completed'
        historyTurn.items = historyTurn.items.map((item: any) => ({ ...item, turnStatus: 'completed' }))
        state.activeRuns.delete(turnId)
        emit('turn/completed', { threadId: thread.id, turn: clone(historyTurn) })
      }, 4000)
      active.completionTimer = completionTimer
      timers.push(completionTimer)
      return { submission: clone(submission), queued: false, turn: { id: turnId }, turnId }
    }
    if (method === 'queue.list') { const paused = state.pausedThreads.has(params.threadId); return { entries: clone(state.queue.filter((entry: any) => entry.threadId === params.threadId)), state: { paused, ...(paused ? { reason: 'Paused by you' } : {}) } } }
    if (method === 'queue.pause') { state.pausedThreads.add(params.threadId); return { paused: true } }
    if (method === 'queue.resume') { state.pausedThreads.delete(params.threadId); return { paused: false } }
    if (method === 'queue.remove') { state.queue = state.queue.filter((entry: any) => entry.id !== params.id); return { removed: true } }
    if (method === 'queue.edit') { state.queue = state.queue.map((entry: any) => entry.id === params.id && entry.status === 'queued' ? { ...entry, document: { ...entry.document, text: params.text }, revision: entry.revision + 1 } : entry); return { updated: true } }
    if (method === 'queue.steer') {
      const entry = state.queue.find((item: any) => item.id === params.id)
      if (!entry) throw new Error('Queued preview message not found.')
      const active = Array.from(state.activeRuns.values()).find((run: any) => run.threadId === entry.threadId) as any
      if (!active) throw new Error('There is no active preview turn to steer.')
      const result: any = await chatRequest('foreground.submit', { ...entry, delivery: 'steer', expectedTurnId: active.turn.id })
      state.queue = state.queue.filter((item: any) => item.id !== params.id)
      return result
    }
    if (method === 'references.search') return [{ id: 'README.md', name: 'README.md', path: 'README.md', type: 'file' }, { id: 'src/App.tsx', name: 'App.tsx', path: 'src/App.tsx', type: 'file' }].filter((entry: any) => `${entry.path} ${entry.name}`.toLowerCase().includes(String(params.query ?? '').toLowerCase()))
    if (method === 'references.resolve') return { root, path: params.id, absolutePath: `${root}/${params.id}` }
    if (method === 'branches.pending') return { data: [] }
    if (method === 'recovery.list') return { items: [] }
    if (method === 'queue.state') return { paused: state.pausedThreads.has(params.threadId) }
    if (method === 'tasks.assign') throw previewOnly('Assigning tasks')
    throw previewOnly(`Chat action “${method}”`)
  }

  const invoke = async (command: string, args: RpcArgs = {}): Promise<any> => {
    const params = args.params ?? {}
    if (command === 'engine_connect') return 1
    if (command === 'engine_request') return engineRequest(String(args.method), params)
    if (command === 'agents_request') return agentsRequest(String(args.method), params)
    if (command === 'chat_request') return chatRequest(String(args.method), params)
    if (command === 'list_projects') return { data: clone(projects) }
    if (command === 'workspace_request') {
      if (args.method === 'directory.list') {
        if (params.path === '' || params.path == null) return { entries: [{ name: 'README.md', path: 'README.md', kind: 'file', size: 276 }, { name: 'src', path: 'src', kind: 'directory', size: 0 }] }
        if (params.path === 'src') return { entries: [{ name: 'App.tsx', path: 'src/App.tsx', kind: 'file', size: 31 }] }
        throw previewOnly(`Workspace folder “${params.path}”`)
      }
      if (args.method === 'file.read') {
        if (params.path === 'README.md') return { path: params.path, content: '# Brr preview workspace\n\nA synthetic workspace for the interactive product tour. Changes here are not written to disk.\n', version: 'preview-v1' }
        if (params.path === 'src/App.tsx') return { path: params.path, content: '// Synthetic preview file only.\n', version: 'preview-v1' }
        throw previewOnly(`Workspace file “${params.path}”`)
      }
      throw previewOnly(`Workspace request “${args.method}”`)
    }
    if (command === 'tickets_request') {
      if (args.method === 'projects') return { data: clone(projects) }
      if (args.method === 'agents') return { data: [{ id: 'preview-research-partner', name: 'Research partner', description: 'Helps compare ideas and find clear next steps.' }] }
      if (args.method === 'list') return { data: [], nextCursor: null }
      if (args.method === 'proposals') return { data: [] }
      throw previewOnly(`Ticket action “${args.method}”`)
    }
    if (command === 'browser_request' || command === 'workspace_browser_request') {
      if (args.method === 'browser.list') return { tabs: [] }
      if (args.method === 'browser.annotations.list') return { annotations: [] }
      throw previewOnly(`Browser action “${args.method}”`)
    }
    if (command === 'plugin:event|listen') {
      const callbackId = Number(args.handler)
      const callback = state.callbackRegistry.get(callbackId)
      if (!callback) throw new Error('Preview event listener callback was not registered.')
      state.callbacks.set(Number(args.eventId ?? callbackId), callback)
      return Number(args.eventId ?? callbackId)
    }
    if (command === 'plugin:event|unlisten') { state.callbacks.delete(Number(args.eventId)); state.callbackRegistry.delete(Number(args.eventId)); return 1 }
    if (command === 'engine_respond') return { accepted: true }
    if (command === 'chatgpt_login' || command === 'open_external_url' || command === 'reveal_project_folder' || command === 'create_project' || command === 'save_project' || command === 'move_chat_to_project' || command === 'archive_project_chats' || command === 'delete_project' || command === 'intelligent_ui_request' || command === 'destroy_mcp_sandbox') throw previewOnly(`Native action “${command}”`)
    throw previewOnly(`Native command “${command}”`)
  }

  return {
    invoke,
    emit,
    registerCallback(callback: (event: unknown) => void) { const callbackId = state.nextCallback++; state.callbackRegistry.set(callbackId, callback); return callbackId },
    inspect: () => clone({ threads: state.threads, drafts: state.drafts, queue: state.queue, requests: state.requests, engineRequests: state.engineRequests, agentCalls: state.agentCalls }),
  }
}

/** Install before importing the real application into the isolated preview iframe. */
export function installPreviewRuntime() {
  const runtime = createPreviewRuntime()
  const callbacks = new Map<number, (event: unknown) => void>()
  const internals = {
    callbacks,
    transformCallback(callback: (event: unknown) => void) { const callbackId = runtime.registerCallback(callback); callbacks.set(callbackId, callback); return callbackId },
    invoke: (command: string, args?: RpcArgs) => runtime.invoke(command, args),
  }
  ;(window as any).__TAURI_INTERNALS__ = internals
  ;(window as any).__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: (eventId: number) => { callbacks.delete(eventId) } }
  try {
    if (!localStorage.getItem('brr.appearance.sidebarColor')) localStorage.setItem('brr.appearance.sidebarColor', 'yellow')
    if (!localStorage.getItem('brr.layout.sidebarCollapsed')) localStorage.setItem('brr.layout.sidebarCollapsed', 'false')
  } catch { /* Storage is optional in embedded preview contexts. */ }
  return runtime
}
