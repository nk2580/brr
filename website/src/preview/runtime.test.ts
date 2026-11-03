import { afterEach, describe, expect, it, vi } from 'vitest'
import { createPreviewRuntime, installPreviewRuntime } from './runtime'

afterEach(() => vi.useRealTimers())

describe('preview runtime', () => {
  it('seeds realistic project conversations and resumes their completed history', async () => {
    const runtime = createPreviewRuntime()
    const projects = await runtime.invoke('list_projects') as { data: Array<{ id: string }> }
    const threads = await runtime.invoke('engine_request', { method: 'thread/list', params: {} }) as { data: Array<{ id: string; projectId: string | null }> }

    expect(projects.data.length).toBeGreaterThan(0)
    expect(threads.data.map(thread => thread.id)).toContain('preview-writing')
    expect(threads.data.every(thread => thread.projectId)).toBe(true)
    const resumed = await runtime.invoke('engine_request', { method: 'thread/resume', params: { threadId: 'preview-writing' } }) as { thread: { turns: Array<{ status: string; items: unknown[] }> } }
    expect(resumed.thread.turns).toHaveLength(1)
    expect(resumed.thread.turns[0].status).toBe('completed')
    expect(resumed.thread.turns[0].items.length).toBeGreaterThan(1)
  })

  it('keeps drafts continuous across reads and updates them by revision', async () => {
    const runtime = createPreviewRuntime()
    const draft = { id: 'draft-a', threadId: 'preview-writing', document: { version: 1, text: 'First' }, settings: { model: 'gpt-6.1-sol', effort: 'medium', mode: 'default' } }
    const saved = await runtime.invoke('chat_request', { method: 'drafts.save', params: { ...draft, expectedRevision: 0 } }) as { revision: number }
    const loaded = await runtime.invoke('chat_request', { method: 'drafts.get', params: { id: draft.id } }) as { revision: number; document: { text: string } }
    expect(saved.revision).toBe(1)
    expect(loaded.document.text).toBe('First')
    const updated = await runtime.invoke('chat_request', { method: 'drafts.save', params: { ...draft, document: { version: 1, text: 'Second' }, expectedRevision: 1 } }) as { revision: number }
    expect(updated.revision).toBe(2)
  })

  it('orders queued submissions, supports queue edits, and rejects unknown RPC without side effects', async () => {
    const runtime = createPreviewRuntime()
    const common = { threadId: 'preview-writing', document: { version: 1, text: 'One' }, settings: { model: 'gpt-6.1-sol', effort: 'medium', mode: 'default' }, delivery: 'queue' }
    await runtime.invoke('chat_request', { method: 'foreground.submit', params: common })
    await runtime.invoke('chat_request', { method: 'foreground.submit', params: { ...common, document: { version: 1, text: 'Two' } } })
    const before = await runtime.invoke('chat_request', { method: 'queue.list', params: { threadId: 'preview-writing' } }) as { entries: Array<{ id: string; document: { text: string } }> }
    expect(before.entries.map(entry => entry.document.text)).toEqual(['One', 'Two'])
    await expect(runtime.invoke('chat_request', { method: 'queue.steer', params: { id: before.entries[0].id } })).rejects.toThrow(/active preview turn/i)
    const preserved = await runtime.invoke('chat_request', { method: 'queue.list', params: { threadId: 'preview-writing' } }) as { entries: Array<{ id: string }> }
    expect(preserved.entries.map(entry => entry.id)).toEqual(before.entries.map(entry => entry.id))
    await runtime.invoke('chat_request', { method: 'queue.edit', params: { id: before.entries[0].id, text: 'Edited' } })
    const after = await runtime.invoke('chat_request', { method: 'queue.list', params: { threadId: 'preview-writing' } }) as { entries: Array<{ document: { text: string } }> }
    expect(after.entries[0].document.text).toBe('Edited')
    await expect(runtime.invoke('engine_request', { method: 'arbitrary/unknown', params: {} })).rejects.toThrow(/preview-only/i)
    expect(runtime.inspect().engineRequests).not.toContain('arbitrary/unknown')
  })

  it('lists bounded workspace folders and keeps the app event bridge connected to a staged preview response', async () => {
    vi.useFakeTimers()
    const runtime = installPreviewRuntime()
    const internals = (window as any).__TAURI_INTERNALS__
    const events: any[] = []
    const callbackId = internals.transformCallback((event: any) => events.push(event.payload))
    const eventId = await internals.invoke('plugin:event|listen', { event: 'engine-event', handler: callbackId })
    const rootListing = await internals.invoke('workspace_request', { method: 'directory.list', params: { root: '/preview/workspace/brr', path: '' } })
    const srcListing = await internals.invoke('workspace_request', { method: 'directory.list', params: { root: '/preview/workspace/brr', path: 'src' } })
    expect(rootListing.entries.some((entry: any) => entry.path === 'src')).toBe(true)
    expect(srcListing.entries.some((entry: any) => entry.path === 'src')).toBe(false)

    await internals.invoke('chat_request', { method: 'foreground.submit', params: { threadId: 'preview-writing', document: { version: 1, text: 'Say hello' }, settings: {}, delivery: 'send' } })
    expect(events.map(event => event.method)).toContain('turn/started')
    expect(events.map(event => event.method)).not.toContain('turn/completed')
    await vi.advanceTimersByTimeAsync(4000)
    expect(events.map(event => event.method)).toContain('turn/completed')
    expect(events.find(event => event.method === 'item/completed' && event.params.item.type === 'agentMessage').params.item.text).toMatch(/Preview response/i)
    await internals.invoke('plugin:event|unlisten', { eventId })
    expect(runtime.inspect().threads.find((thread: any) => thread.id === 'preview-writing')).toBeDefined()
  })

  it('cancels staged response events and records an interrupted turn when Stop is pressed', async () => {
    vi.useFakeTimers()
    const runtime = createPreviewRuntime()
    const events: any[] = []
    runtime.registerCallback((event: any) => events.push(event.payload))
    await runtime.invoke('plugin:event|listen', { handler: 1 })
    const sent = await runtime.invoke('chat_request', { method: 'foreground.submit', params: { threadId: 'preview-writing', document: { version: 1, text: 'Stop before the response' }, settings: {}, delivery: 'send' } })
    await vi.advanceTimersByTimeAsync(1000)
    await runtime.invoke('engine_request', { method: 'turn/interrupt', params: { threadId: 'preview-writing', turnId: sent.turnId } })
    await vi.advanceTimersByTimeAsync(5000)
    expect(events.map(event => event.method)).toContain('turn/completed')
    expect(events.find(event => event.method === 'turn/completed').params.turn.status).toBe('interrupted')
    expect(events.some(event => event.method === 'item/started' && event.params.item.type === 'agentMessage')).toBe(false)
    const resumed = await runtime.invoke('engine_request', { method: 'thread/resume', params: { threadId: 'preview-writing' } })
    expect(resumed.thread.turns.at(-1).status).toBe('interrupted')
    expect(resumed.thread.turns.at(-1).items.some((item: any) => item.type === 'agentMessage')).toBe(false)
  })

  it('adds a steer message to the existing active turn without duplicating its id', async () => {
    vi.useFakeTimers()
    const runtime = createPreviewRuntime()
    runtime.registerCallback(() => {})
    await runtime.invoke('plugin:event|listen', { handler: 1 })
    const first = await runtime.invoke('chat_request', { method: 'foreground.submit', params: { threadId: 'preview-writing', document: { version: 1, text: 'First message' }, settings: {}, delivery: 'send' } })
    const steered = await runtime.invoke('chat_request', { method: 'foreground.submit', params: { threadId: 'preview-writing', document: { version: 1, text: 'Follow-up while running' }, settings: {}, delivery: 'steer', expectedTurnId: first.turnId } })
    const resumed = await runtime.invoke('engine_request', { method: 'thread/resume', params: { threadId: 'preview-writing' } })
    const turns = resumed.thread.turns.filter((entry: any) => entry.id === first.turnId)
    expect(steered.turnId).toBe(first.turnId)
    expect(turns).toHaveLength(1)
    expect(turns[0].items.filter((item: any) => item.type === 'userMessage').map((item: any) => item.text)).toEqual(['First message', 'Follow-up while running'])
    expect(new Set(turns[0].items.map((item: any) => item.id)).size).toBe(turns[0].items.length)
    await vi.advanceTimersByTimeAsync(5000)
  })
})
