import type { Page } from '@playwright/test'

/** A durable Tauri boundary for browser acceptance. Lexical and the transcript stay real. */
export async function installChatExperienceFixture(page: Page) {
  await page.addInitScript(() => {
    const browser = window as any
    const STORE = 'chatExperience.fixture.v1'
    const now = '2026-11-02T00:00:00.000Z'
    const workspace = { cwd: '/workspace/brr', roots: ['/workspace/brr'], folders: [{ source: '/workspace/brr', path: '/workspace/brr' }], worktrees: [], scopeProjectId: null, projectRevision: null }
    const thread = (id: string, preview: string) => ({ id, projectId: null, cwd: '/workspace/brr', preview, updatedAt: 1, workspace })
    const turns: any[] = [
      { id: 'turn-old', status: 'completed', items: [
        { id: 'user-old', type: 'userMessage', turnId: 'turn-old', turnStatus: 'completed', text: 'Inspect the project' },
        { id: 'reason-old', type: 'reasoning', turnId: 'turn-old', turnStatus: 'completed', text: 'I will inspect the files.' },
        { id: 'command-old', type: 'commandExecution', turnId: 'turn-old', turnStatus: 'completed', command: 'ls', aggregatedOutput: 'README.md' },
        { id: 'file-old', type: 'fileChange', turnId: 'turn-old', turnStatus: 'completed', changes: [{ path: 'README.md', diff: '@@ -1 +1 @@\n-old\n+new' }] },
        { id: 'agent-old', type: 'agentMessage', turnId: 'turn-old', turnStatus: 'completed', text: 'The project has a README.' },
      ] },
      { id: 'turn-live', status: 'inProgress', items: [
        { id: 'user-live', type: 'userMessage', turnId: 'turn-live', turnStatus: 'inProgress', text: 'Keep working on the project' },
        { id: 'agent-live', type: 'agentMessage', turnId: 'turn-live', turnStatus: 'inProgress', text: 'I am reviewing the current changes.' },
      ] },
    ]
    for (let index = 0; index < 24; index += 1) {
      turns[0].items.push({ id: `history-${index}`, type: 'agentMessage', turnId: 'turn-old', turnStatus: 'completed', text: `Historical detail ${index + 1}: this completed work stays available while reading the conversation.` })
    }
    const seed = {
      drafts: [] as any[], attachments: [] as any[], queue: [] as any[], paused: false,
      requests: [] as any[], engineRequests: [] as any[], agentCalls: [] as string[],
      threads: [thread('chat-history', 'Project review'), thread('chat-running', 'Active work')],
    }
    if (!localStorage.getItem(STORE)) localStorage.setItem(STORE, JSON.stringify(seed))
    const state = () => JSON.parse(localStorage.getItem(STORE) || JSON.stringify(seed))
    const save = (next: any) => localStorage.setItem(STORE, JSON.stringify(next))
    browser.__chatExperience = {
      inspect: state,
      emit: (method: string, params: Record<string, unknown>) => {
        for (const callback of Object.values(browser.__tauriCallbacks ?? {}) as Array<(event: unknown) => void>) {
          callback({ event: 'engine-event', id: 1, payload: { method, params } })
        }
      },
    }
    browser.__tauriCallbacks = {}
    let nextCallback = 1
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
        if (command === 'plugin:dialog|open') return ['/workspace/brr/README.md']
        if (command === 'list_projects') return { data: [] }
        if (command === 'browser_request') return {}
        if (command === 'workspace_request') {
          const { method, params = {} } = args
          if (method === 'directory.list') return { entries: [{ name: 'README.md', path: 'README.md', kind: 'file', size: 41 }] }
          if (method === 'file.read') return { path: params.path, content: '# Fixture worktree\n\nThis file belongs to the selected chat workspace.\n', version: 'fixture-v1' }
          throw new Error(`Unexpected workspace request ${method}`)
        }
        if (command === 'agents_request') {
          const { method } = args
          const current = state(); current.agentCalls.push(method); save(current)
          if (method === 'agents.list') return { data: [{ id: 'agent-release', revision: 1, createdAt: 1, updatedAt: 1, name: 'Release guide', description: 'Reviews changes', instructions: 'Review changes.', soul: 'Direct.', model: 'gpt-6.1-sol', effort: 'medium' }] }
          if (method === 'agents.decisions') return { data: [], runtimeRequests: [], questions: [] }
          if (method === 'tasks.list' || method === 'agents.drafts' || method === 'questions.list') return { data: [] }
          if (method === 'agents.settings') return { maxBackground: 4, maxPerAgent: 2 }
          throw new Error(`Unexpected agents request ${method}`)
        }
        if (command === 'chat_request') {
          const current = state()
          const { method, params = {} } = args
          current.requests.push({ method, params })
          const write = (value: unknown) => { save(current); return value }
          if (method === 'drafts.list') return current.drafts.filter((draft: any) => draft.document?.text || draft.document?.references?.length || draft.document?.quotes?.length || draft.document?.attachmentIds?.length)
          if (method === 'drafts.get') {
            const draft = current.drafts.find((entry: any) => entry.id === params.id)
            if (!draft) throw new Error('Draft not found')
            return draft
          }
          if (method === 'drafts.save') {
            const index = current.drafts.findIndex((entry: any) => entry.id === params.id)
            const revision = index >= 0 ? current.drafts[index].revision : 0
            if (revision !== params.expectedRevision) throw new Error('Draft revision conflict')
            const record = { ...params, threadId: params.threadId ?? null, projectPath: params.projectPath ?? null, agentId: params.agentId ?? null, revision: revision + 1, updatedAt: new Date().toISOString() }
            if (index >= 0) current.drafts[index] = record; else current.drafts.push(record)
            return write(record)
          }
          if (method === 'drafts.promote') {
            const index = current.drafts.findIndex((entry: any) => entry.id === params.id)
            const record = index >= 0 ? current.drafts[index] : { id: params.id, document: { version: 1, text: '', references: [], quotes: [], attachmentIds: [] }, settings: { model: 'gpt-6.1-sol', modelId: 'sol', effort: 'medium', mode: 'default' }, revision: 0 }
            current.drafts = current.drafts.filter((entry: any) => entry.id !== params.id)
            const promoted = { ...record, id: params.id, threadId: params.threadId, revision: record.revision + 1, updatedAt: now }
            current.drafts.push(promoted)
            return write(promoted)
          }
          if (method === 'drafts.delete') return write({ deleted: true })
          if (method === 'attachments.list') return current.attachments.filter((item: any) => item.ownerId === params.ownerId).map(({ ownerId, content, ...item }: any) => item)
          if (method === 'attachments.import' || method === 'attachments.importBytes') {
            const files = method === 'attachments.importBytes'
              ? (params.files ?? []).map((file: any) => ({ name: file.name, mime: file.mime, content: atob(file.data), base64: file.data }))
              : (params.paths ?? []).map((path: string) => ({ name: path.split('/').at(-1), mime: 'text/plain', content: 'Example file from the workspace.' }))
            const imported = files.map((file: any) => {
              const bytes = new TextEncoder().encode(file.content)
              const image = /^(image\/(png|jpeg|gif|webp))$/i.test(file.mime)
              const item = { id: crypto.randomUUID(), ownerId: params.ownerId, name: file.name, mime: file.mime || 'text/plain', size: bytes.length, kind: image ? 'image' : 'file', status: 'ready', content: file.content, base64: file.base64 ?? btoa(file.content) }
              current.attachments.push(item)
              return item
            })
            return write(imported.map(({ ownerId, content, base64, ...item }: any) => item))
          }
          if (method === 'attachments.preview') {
            const item = current.attachments.find((entry: any) => entry.ownerId === params.ownerId && entry.id === params.id)
            if (!item) throw new Error('Attachment not found')
            const preview = item.kind === 'image' ? `data:${item.mime};base64,${item.base64}` : item.mime.startsWith('text/') ? item.content : null
            return { attachment: { id: item.id, name: item.name, mime: item.mime, size: item.size, kind: item.kind, status: item.status }, preview, ...(preview === null ? { reason: 'preview-binary' } : {}) }
          }
          if (method === 'attachments.remove') {
            current.attachments = current.attachments.filter((item: any) => !(item.ownerId === params.ownerId && item.id === params.id))
            return write({ removed: true })
          }
          if (method === 'foreground.submit') {
            const entry: any = { id: params.id ?? crypto.randomUUID(), threadId: params.threadId, document: params.document, attachmentIds: params.attachmentIds ?? [], settings: params.settings, delivery: params.delivery, expectedTurnId: params.expectedTurnId, status: params.delivery === 'queue' ? 'queued' : 'accepted', revision: 1, createdAt: now }
            if (params.delivery === 'queue') current.queue.push(entry)
            if (params.delivery === 'steer') entry.turnId = 'turn-live'
            return write({ submission: entry, queued: params.delivery === 'queue', ...(params.delivery === 'steer' ? { turn: { id: 'turn-live' }, turnId: 'turn-live' } : {}) })
          }
          if (method === 'queue.list') return { entries: current.queue.filter((entry: any) => entry.threadId === params.threadId), state: { paused: current.paused, ...(current.paused ? { reason: 'Paused by you' } : {}) } }
          if (method === 'queue.pause') { current.paused = true; return write({ paused: true }) }
          if (method === 'queue.resume') { current.paused = false; return write({ paused: false }) }
          if (method === 'queue.remove') { current.queue = current.queue.filter((entry: any) => entry.id !== params.id); return write({ removed: true }) }
          if (method === 'queue.edit') {
            current.queue = current.queue.map((entry: any) => entry.id === params.id && entry.status === 'queued' ? { ...entry, document: { ...entry.document, text: params.text }, revision: entry.revision + 1 } : entry)
            return write({ updated: true })
          }
          if (method === 'queue.steer') {
            current.queue = current.queue.map((entry: any) => entry.id === params.id ? { ...entry, status: 'accepted', turnId: 'turn-live', revision: entry.revision + 1 } : entry)
            return write({ accepted: true })
          }
          if (method === 'references.search') return [{ id: 'readme-ref', name: 'README.md', path: 'README.md', type: 'file' }]
          if (method === 'references.resolve') return { root: '/workspace/brr', path: 'README.md', absolutePath: '/workspace/brr/README.md' }
          if (method === 'branches.pending') return write({ data: current.pendingBranches ?? [] })
          if (method === 'recovery.list') return write({ items: [] })
          if (method === 'queue.state') return { paused: current.paused }
          throw new Error(`Unexpected chat request ${method}`)
        }
        if (command !== 'engine_request') throw new Error(`Unexpected command ${command}`)
        const current = state()
        current.engineRequests.push(args)
        save(current)
        const { method, params = {} } = args
        if (method === 'account/read') return { account: { type: 'chatgpt', email: 'developer@example.com', planType: 'plus' } }
        if (method === 'model/list') return { data: [
          { id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{ reasoningEffort: 'low', description: 'Fast' }, { reasoningEffort: 'medium', description: 'Balanced' }, { reasoningEffort: 'high', description: 'Thorough' }] },
          { id: 'astra', model: 'gpt-6-astra', displayName: 'GPT Astra', isDefault: false, defaultReasoningEffort: 'high', supportedReasoningEfforts: [{ reasoningEffort: 'medium', description: 'Balanced' }, { reasoningEffort: 'high', description: 'Thorough' }] },
        ] }
        if (method === 'thread/list') return { data: current.threads, nextCursor: null }
        if (method === 'thread/resume') {
          const id = params.threadId
          const selected = current.threads.find((value: any) => value.id === id) ?? thread(id, id)
          return { thread: { ...selected, turns: id === 'chat-running' ? [turns[0], { id: 'turn-live', status: 'inProgress', items: turns[1].items }] : turns } }
        }
        if (method === 'thread/turns/list') return { data: [], nextCursor: null }
        if (method === 'skills/list') return { data: [] }
        if (method === 'turn/interrupt') {
          window.setTimeout(() => browser.__chatExperience.emit('turn/completed', { threadId: params.threadId, turn: { id: params.turnId, status: 'interrupted', items: [] } }), 0)
          return {}
        }
        if (method === 'thread/start') return { thread: thread('chat-created', 'New chat') }
        if (method === 'brr/project/inspect') return { commands: [] }
        throw new Error(`Unexpected engine request ${method}`)
      },
    }
    browser.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  })
}
