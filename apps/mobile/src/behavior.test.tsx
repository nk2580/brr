import { act, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { MobileApp } from './App'
import { RemoteClient, type NativeBridge, type Profile, type Snapshot } from '@brr/remote-client'

const profile: Profile = { id: 'phone', name: 'Primary', endpoint: 'https://primary.local', fingerprint: 'f'.repeat(64), nodeId: 'primary' }
const worker = { nodeId: 'worker', name: 'Worker', primary: false, online: true, epoch: 'worker-epoch-8' }
const baseSnapshot = (extras: Partial<Snapshot> = {}): Snapshot => ({
  protocolVersion: 1, nodeId: 'primary', epoch: 'primary-epoch-2', cursor: { epoch: 'primary-epoch-2', sequence: 0 },
  nodes: [{ nodeId: 'primary', name: 'Primary', primary: true, online: true, epoch: 'primary-epoch-2' }, worker],
  account: null, models: [{ id: 'model-x' }], threads: [], projects: [], agents: [], tasks: [], questions: [], approvals: [], sync: {}, errors: [], ...extras,
})

function renderConnected(snapshot: Snapshot, route: (request: Record<string, unknown>) => unknown = () => ({}), cached: Record<string, unknown> = {}, readSnapshot: () => Snapshot = () => snapshot, onCachePut?: (key: string, value: unknown) => Promise<void>) {
  const listeners = new Map<string, (payload: unknown) => void>()
  const cache = { ...cached }
  const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
    if (command === 'remote_profiles') return [profile] as T
    if (command === 'remote_snapshot') return readSnapshot() as T
    if (command === 'remote_cache_get') return (cache[String(args?.key)] ?? null) as T
    if (command === 'remote_cache_put') { if (onCachePut) await onCachePut(String(args?.key), args?.value); cache[String(args?.key)] = args?.value; return undefined as T }
    if (command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
    if (command === 'remote_request') {
      const request = (args?.request ?? {}) as Record<string, unknown>
      const result = await route(request)
      return { id: request.id, result } as T
    }
    return undefined as T
  })
  const listen = vi.fn(async (event: string, handler: (payload: unknown) => void) => { listeners.set(event, handler); return () => listeners.delete(event) })
  const client = new RemoteClient({ invoke, listen } as NativeBridge)
  return { invoke, listen, cache, client, emit: (event: string, payload: unknown) => listeners.get(event)?.(payload), user: userEvent.setup(), view: render(<MobileApp client={client} native />) }
}

function sent(invoke: ReturnType<typeof vi.fn>, method: string) {
  return invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === method)
}

function attentionRoute(snapshot: Snapshot) {
  return (request: Record<string, unknown>) => request.channel === 'questions' ? { data: snapshot.questions }
    : request.channel === 'approvals' ? { data: snapshot.approvals } : {}
}

describe('mobile owner-scoped actions', () => {
  it('shows an attachment verification error while keeping the conversation readable', async () => {
    const thread = { id: 'unverified-file-thread', title: 'Conversation with an unavailable file', ownerNodeId: 'primary', status: { type: 'idle' }, turns: [] }
    const { user } = renderConnected(baseSnapshot({ threads: [thread] }), request => request.method === 'read'
      ? { ...thread, attachmentError: 'Saved attachments could not be verified. Refresh this conversation to try again.' }
      : { data: [], nextCursor: null })
    await user.click(await screen.findByRole('button', { name: /Conversation with an unavailable file/ }))
    expect(await screen.findByText('Saved attachments could not be verified. Refresh this conversation to try again.')).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Conversation with an unavailable file' })).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Message' })).toBeInTheDocument()
  })

  it('previews a shared attachment through its original offline owner without dispatching a mutation', async () => {
    const attachment = { id: 'saved-file', name: 'saved-report.txt', mime: 'text/plain', kind: 'file', sizeBytes: 25, sha256: 'a'.repeat(64), submissionId: 'saved-submission', turnId: 'saved-turn' }
    const thread = { id: 'saved-thread', title: 'Saved attachment conversation', ownerNodeId: 'worker', historical: true, readOnly: true, attachments: [attachment], turns: [] }
    const snapshot = baseSnapshot({ nodes: [{ nodeId: 'primary', name: 'Primary', online: true, primary: true }, { ...worker, online: false }], threads: [thread] })
    const { user, invoke } = renderConnected(snapshot, request => request.channel === 'sessions' && request.method === 'attachment.preview'
      ? { attachment, preview: 'Verified attachment bytes' } : {})
    await user.click(await screen.findByRole('button', { name: /Saved attachment conversation/ }))
    await user.click(await screen.findByRole('button', { name: 'Preview saved-report.txt' }))
    expect(await screen.findByText('Verified attachment bytes')).toBeInTheDocument()
    const preview = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'attachment.preview')
    expect(preview?.[1]?.request).toMatchObject({ channel: 'sessions', method: 'attachment.preview', targetNodeId: 'worker', params: { threadId: 'saved-thread', sha256: attachment.sha256 } })
    expect(preview?.[1]?.request).not.toHaveProperty('operationId')
    expect(invoke.mock.calls.some(([command, args]) => command === 'remote_cache_put' && String((args as { key?: string })?.key).startsWith('remote:pending-mutation:'))).toBe(false)
  })

  it('retries an attachment preview after a temporary read failure', async () => {
    const attachment = { id: 'retry-file', name: 'retry.txt', mime: 'text/plain', kind: 'file', sizeBytes: 8, sha256: 'b'.repeat(64), submissionId: 'retry-submission' }
    let attempts = 0
    const snapshot = baseSnapshot({ threads: [{ id: 'retry-thread', title: 'Retry attachment conversation', ownerNodeId: 'worker', historical: true, attachments: [attachment] }] })
    const { user } = renderConnected(snapshot, request => {
      if (request.method !== 'attachment.preview') return {}
      if (++attempts === 1) throw new Error('Preview temporarily unavailable')
      return { attachment, preview: 'Restored' }
    })
    await user.click(await screen.findByRole('button', { name: /Retry attachment conversation/ }))
    await user.click(await screen.findByRole('button', { name: 'Preview retry.txt' }))
    expect(await screen.findByText('Preview temporarily unavailable')).toBeInTheDocument()
    await user.click(within(screen.getByRole('region', { name: 'Conversation attachments' })).getByRole('button', { name: 'Try again' }))
    expect(await screen.findByText('Restored')).toBeInTheDocument()
    expect(attempts).toBe(2)
  })

  it('retains a verified attachment preview across an offline cold relaunch', async () => {
    const attachment = { id: 'cached-file', name: 'cached.txt', mime: 'text/plain', kind: 'file', sizeBytes: 7, sha256: 'c'.repeat(64), submissionId: 'cached-submission' }
    const snapshot = baseSnapshot({ threads: [{ id: 'cached-thread', title: 'Cached attachment conversation', ownerNodeId: 'worker', historical: true, attachments: [attachment] }] })
    const first = renderConnected(snapshot, request => request.method === 'attachment.preview' ? { attachment, preview: 'Keep me' } : {})
    await first.user.click(await screen.findByRole('button', { name: /Cached attachment conversation/ }))
    await first.user.click(await screen.findByRole('button', { name: 'Preview cached.txt' }))
    expect(await screen.findByText('Keep me')).toBeInTheDocument()
    first.view.unmount()

    const second = renderConnected(snapshot, () => { throw new Error('Primary disconnected') }, { ...first.cache, 'snapshot:phone': snapshot }, () => { throw new Error('Primary disconnected') })
    await second.user.click(await screen.findByRole('button', { name: /Cached attachment conversation/ }))
    await second.user.click(await screen.findByRole('button', { name: 'Preview cached.txt' }))
    expect(await screen.findByText('Keep me')).toBeInTheDocument()
    expect(sent(second.invoke, 'attachment.preview')).toBe(false)
  })

  it('keeps duplicate-SHA attachment rows independently expandable while sharing cached bytes', async () => {
    const sha256 = 'd'.repeat(64)
    const first = { id: 'first-file', name: 'first.txt', mime: 'text/plain', kind: 'file', sizeBytes: 12, sha256, submissionId: 'first-submission' }
    const second = { id: 'second-file', name: 'second.txt', mime: 'text/plain', kind: 'file', sizeBytes: 12, sha256, submissionId: 'second-submission' }
    const snapshot = baseSnapshot({ threads: [{ id: 'duplicate-thread', title: 'Duplicate attachment conversation', ownerNodeId: 'worker', historical: true, attachments: [first, second] }] })
    const { user, invoke } = renderConnected(snapshot, request => request.method === 'attachment.preview' ? { attachment: first, preview: 'Shared bytes' } : {})
    await user.click(await screen.findByRole('button', { name: /Duplicate attachment conversation/ }))
    const region = screen.getByRole('region', { name: 'Conversation attachments' })
    const firstRow = within(region).getByRole('button', { name: /Preview first.txt/ })
    const secondRow = within(region).getByRole('button', { name: /Preview second.txt/ })

    await user.click(firstRow)
    await waitFor(() => expect(firstRow).toHaveAttribute('aria-expanded', 'true'))
    expect(secondRow).toHaveAttribute('aria-expanded', 'false')
    await user.click(secondRow)

    expect(firstRow).toHaveAttribute('aria-expanded', 'true')
    expect(secondRow).toHaveAttribute('aria-expanded', 'true')
    expect(within(region).getAllByText('Shared bytes')).toHaveLength(2)
    expect(invoke.mock.calls.filter(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'attachment.preview')).toHaveLength(1)
  })

  it('does not restore an attachment preview to cache when its profile is forgotten mid-request', async () => {
    const attachment = { id: 'forgotten-file', name: 'private.txt', mime: 'text/plain', kind: 'file', sizeBytes: 13, sha256: 'e'.repeat(64), submissionId: 'forgotten-submission' }
    const snapshot = baseSnapshot({ threads: [{ id: 'forgotten-thread', title: 'Forgotten attachment conversation', ownerNodeId: 'worker', historical: true, attachments: [attachment] }] })
    let finishPreview: ((value: unknown) => void) | undefined
    let previewStarted = false
    const previewPending = new Promise<unknown>(resolve => { finishPreview = resolve })
    const rendered = renderConnected(snapshot, async request => {
      if (request.method !== 'attachment.preview') return {}
      previewStarted = true
      return previewPending
    })
    await rendered.user.click(await screen.findByRole('button', { name: /Forgotten attachment conversation/ }))
    await rendered.user.click(await screen.findByRole('button', { name: 'Preview private.txt' }))
    await waitFor(() => expect(previewStarted).toBe(true))

    await rendered.client.forget(profile.id)
    await act(async () => {
      finishPreview?.({ attachment, preview: 'Should not return after Forget' })
      await previewPending
    })

    expect(screen.queryByText('Should not return after Forget')).not.toBeInTheDocument()
    expect(Object.keys(rendered.cache).filter(key => key.startsWith('attachment-preview:'))).toEqual([])
  })

  it('shows readable attention owners while routing with the exact roster node ID', async () => {
    const workerId = `node_${'a'.repeat(64)}`
    const unknownId = `node_${'b'.repeat(64)}`
    const question = (id: string, ownerNodeId: string, prompt: string) => ({ id, threadId: `thread-${id}`, ownerNodeId, ownerEpoch: 'worker-epoch-8', kind: 'async', resolution: 'pending', delivery: 'none', questions: [{ id: 'answer', header: 'Answer', question: prompt }], draft: {} })
    const snapshot = baseSnapshot({
      nodes: [{ nodeId: 'primary', name: 'Primary', primary: true, online: true, epoch: 'primary-epoch-2' }, { nodeId: workerId, name: 'Build worker', primary: false, online: true, epoch: 'worker-epoch-8' }],
      questions: [question('named-owner', workerId, 'Answer the worker?'), question('unknown-owner', unknownId, 'Answer the missing node?')],
      approvals: [{ id: 'approval-hash', requestId: 81, ownerNodeId: workerId, ownerEpoch: 'worker-epoch-8', status: 'pending', method: 'item/commandExecution/requestApproval', params: { command: 'echo reviewed', cwd: '/workspace' } }],
    })
    const { invoke, user } = renderConnected(snapshot, attentionRoute(snapshot))
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))

    const workerQuestion = screen.getByText('Answer the worker?').closest<HTMLElement>('.attention-card')!
    expect(within(workerQuestion).getByText('Build worker')).toBeInTheDocument()
    expect(within(workerQuestion).queryByText(workerId)).not.toBeInTheDocument()
    await user.type(within(workerQuestion).getByRole('textbox'), 'Proceed')
    await user.click(within(workerQuestion).getByRole('button', { name: 'Submit answers' }))
    await waitFor(() => expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string; targetNodeId?: string } })?.request?.channel === 'questions' && (args as { request?: { method?: string } })?.request?.method === 'answer' && (args as { request?: { targetNodeId?: string } })?.request?.targetNodeId === workerId)).toBe(true))

    const unknownQuestion = screen.getByText('Answer the missing node?').closest<HTMLElement>('.attention-card')!
    const unknownOwnerLabel = within(unknownQuestion).getByTitle(`Node ID ${unknownId}`)
    expect(unknownOwnerLabel.textContent).toBe('node_bbbbb…bbbbbbb')
    expect(unknownOwnerLabel.textContent!.length).toBeLessThanOrEqual(20)
    const approval = screen.getByText('echo reviewed').closest<HTMLElement>('.approval-card')!
    expect(within(approval).getByText('Build worker')).toBeInTheDocument()
    await user.click(within(approval).getByRole('button', { name: 'Accept' }))
    await waitFor(() => expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string; targetNodeId?: string } })?.request?.channel === 'approvals' && (args as { request?: { method?: string } })?.request?.method === 'answer' && (args as { request?: { targetNodeId?: string } })?.request?.targetNodeId === workerId)).toBe(true))
  })

  it('labels Fleet connectivity as cached after the phone disconnects and wraps diagnostic IDs', async () => {
    const diagnosticId = `node_${'c'.repeat(64)}`
    const snapshot = baseSnapshot({ nodes: [{ nodeId: 'primary', name: 'Primary', primary: true, online: true, epoch: 'primary-epoch-2' }, { nodeId: diagnosticId, name: 'Long ID worker', online: true, epoch: 'worker-epoch-8' }], sync: { status: 'Up to date' } })
    const { user, emit, listen } = renderConnected(snapshot)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(listen).toHaveBeenCalledWith('remote-disconnect', expect.any(Function)))
    act(() => emit('remote-disconnect', { profileId: profile.id, message: 'Phone connection ended' }))
    expect(await screen.findByText('Phone connection ended')).toBeInTheDocument()
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))

    const summary = document.querySelector('.fleet-summary')!
    expect(within(summary as HTMLElement).getByText('2')).toBeInTheDocument()
    expect(within(summary as HTMLElement).getByText('cached')).toBeInTheDocument()
    expect(within(summary as HTMLElement).getByText(/last saved snapshot/i)).toBeInTheDocument()
    expect(screen.getByText('Last reported · Up to date')).toBeInTheDocument()
    const workerCard = screen.getByText('Long ID worker').closest<HTMLElement>('.node-card')!
    expect(workerCard.textContent).toContain('Cached · last reported connected')
    const diagnostic = workerCard.querySelector<HTMLElement>('.diagnostic-id')!
    expect(diagnostic).toHaveClass('diagnostic-id')
    expect(diagnostic.textContent!.length).toBeLessThan(100)
    expect(diagnostic.textContent).toContain(diagnosticId)
    expect(workerCard.querySelectorAll('.diagnostic-id')[1].textContent).toContain('Epoch · worker-epoch-8')
  })

  it('keeps a deferred local question draft but skips its remote write after disconnect', async () => {
    const question = { id: 'deferred-question', threadId: 'deferred-thread', ownerNodeId: 'primary', ownerEpoch: 'primary-epoch-2', kind: 'async', resolution: 'pending', delivery: 'none', questions: [{ id: 'answer', header: 'Answer', question: 'What should happen?' }], draft: {} }
    const snapshot = baseSnapshot({ questions: [question] })
    let releaseCacheWrite!: () => void
    const cacheWritePending = new Promise<void>(resolve => { releaseCacheWrite = resolve })
    const { invoke, user, emit } = renderConnected(snapshot, attentionRoute(snapshot), {}, () => snapshot, key => key === 'draft:question:phone:deferred-question' ? cacheWritePending : Promise.resolve())
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))
    await user.type(screen.getByRole('textbox'), 'Keep this answer')
    await waitFor(() => expect(invoke.mock.calls.some(([command, args]) => command === 'remote_cache_put' && (args as { key?: string })?.key === 'draft:question:phone:deferred-question')).toBe(true))

    act(() => emit('remote-disconnect', { profileId: profile.id, message: 'Connection lost during draft save' }))
    expect(await screen.findByText('Connection lost during draft save')).toBeInTheDocument()
    await act(async () => {
      releaseCacheWrite()
      await cacheWritePending
      await new Promise<void>(resolve => setTimeout(resolve, 0))
    })
    expect(invoke).toHaveBeenCalledWith('remote_cache_put', expect.objectContaining({ key: 'draft:question:phone:deferred-question', value: { answer: ['Keep this answer'] } }))
    expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'questions' && (args as { request?: { method?: string } })?.request?.method === 'draft')).toBe(false)
  })

  it('gates Attention responses by each exact owner while keeping local question drafts editable', async () => {
    const question = (id: string, ownerNodeId: string, prompt: string) => ({
      id, threadId: `thread-${id}`, ownerNodeId, ownerEpoch: ownerNodeId === 'primary' ? 'primary-epoch-2' : 'worker-epoch-8',
      kind: 'async', resolution: 'pending', delivery: 'none', questions: [{ id: 'decision', header: 'Decision', question: prompt }, ...(ownerNodeId === 'worker' ? [{ id: 'reason', header: 'Reason', question: 'Why this decision?' }] : [])], draft: {},
    })
    const approval = (id: string, ownerNodeId: string) => ({
      id, requestId: id, ownerNodeId, ownerEpoch: ownerNodeId === 'primary' ? 'primary-epoch-2' : 'worker-epoch-8', status: 'pending',
      method: 'item/commandExecution/requestApproval', params: { command: `echo ${id}`, cwd: '/workspace' },
    })
    const snapshot = baseSnapshot({
      nodes: [{ nodeId: 'primary', name: 'Primary', primary: true, online: true, epoch: 'primary-epoch-2' }, { ...worker, online: false }],
      questions: [question('worker-question', 'worker', 'Worker decision?'), question('primary-question', 'primary', 'Primary decision?')],
      approvals: [approval('worker-approval', 'worker'), approval('primary-approval', 'primary')],
    })
    const { invoke, user } = renderConnected(snapshot, attentionRoute(snapshot))
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))

    const workerQuestion = screen.getByText('Worker decision?').closest<HTMLElement>('.attention-card')!
    const primaryQuestion = screen.getByText('Primary decision?').closest<HTMLElement>('.attention-card')!
    expect(within(workerQuestion).getByRole('button', { name: 'Next' })).toBeDisabled()
    expect(within(workerQuestion).getByRole('button', { name: 'Dismiss' })).toBeDisabled()
    expect(within(primaryQuestion).getByRole('button', { name: 'Dismiss' })).toBeEnabled()
    await user.type(within(workerQuestion).getByRole('textbox'), 'Keep the draft locally')
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('remote_cache_put', expect.objectContaining({ key: 'draft:question:phone:worker-question', value: { decision: ['Keep the draft locally'] } })))
    await user.click(within(workerQuestion).getByRole('button', { name: 'Next' }))
    expect(within(workerQuestion).getByText('Why this decision?')).toBeInTheDocument()
    await user.click(within(workerQuestion).getByRole('button', { name: 'Back' }))
    await user.click(within(workerQuestion).getByRole('button', { name: 'Next' }))
    expect(within(workerQuestion).getByRole('button', { name: 'Submit answers' })).toBeDisabled()
    expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string; targetNodeId?: string } })?.request?.channel === 'questions' && (args as { request?: { method?: string } })?.request?.method === 'draft' && (args as { request?: { targetNodeId?: string } })?.request?.targetNodeId === 'worker')).toBe(false)

    const workerApproval = screen.getByText('echo worker-approval').closest<HTMLElement>('.approval-card')!
    const primaryApproval = screen.getByText('echo primary-approval').closest<HTMLElement>('.approval-card')!
    expect(within(workerApproval).getByRole('button', { name: 'Accept' })).toBeDisabled()
    expect(within(workerApproval).getByRole('button', { name: 'Decline' })).toBeDisabled()
    expect(within(primaryApproval).getByRole('button', { name: 'Accept' })).toBeEnabled()
  })

  it('disables live chat controls when the conversation owner is offline', async () => {
    const thread = { id: 'offline-thread', title: 'Offline owner thread', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', turns: [{ id: 'turn-1', status: 'inProgress', items: [] }] }
    const snapshot = baseSnapshot({ nodes: [{ nodeId: 'primary', name: 'Primary', primary: true, online: true, epoch: 'primary-epoch-2' }, { ...worker, online: false }], threads: [thread] })
    const { invoke, user } = renderConnected(snapshot, request => request.method === 'read' ? thread : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Offline owner thread/ }))

    expect(await screen.findByRole('button', { name: 'Stop run' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Steer this turn' })).toBeDisabled()
    await user.click(screen.getByRole('button', { name: 'More conversation actions' }))
    expect(screen.getByRole('button', { name: 'Rename conversation' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Archive conversation' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Delete conversation' })).toBeDisabled()

    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Continue the task')
    expect(screen.getByRole('button', { name: 'Save draft' })).toBeEnabled()
    await user.click(screen.getByRole('button', { name: 'Save draft' }))
    expect(sent(invoke, 'send')).toBe(false)
    expect(invoke).toHaveBeenCalledWith('remote_cache_put', expect.objectContaining({ key: 'draft:chat:phone:worker:offline-thread' }))
  })

  it('does not read historical threads as live sessions and preserves read-only access as an independent mode', async () => {
    const historical = { id: 'history-1', title: 'Historical import', ownerNodeId: 'primary', historical: true, readOnly: true, turns: [] }
    const snapshot = baseSnapshot({ threads: [historical] })
    const { invoke, user } = renderConnected(snapshot)
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Historical import/ }))
    await user.click(screen.getByRole('button', { name: 'More conversation actions' }))
    expect(screen.getByRole('button', { name: 'Rename conversation' })).toBeDisabled()
    expect(sent(invoke, 'read')).toBe(false)
    expect(screen.getAllByText(/Historical conversation/).length).toBeGreaterThan(0)
  })

  it('keeps a live read-only session operable on its available owner', async () => {
    const readonly = { id: 'readonly-live', title: 'Live read-only session', ownerNodeId: 'primary', readOnly: true, turns: [] }
    const next = renderConnected(baseSnapshot({ threads: [readonly] }), request => request.method === 'read' ? readonly : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await next.user.click(await screen.findByRole('button', { name: /Live read-only session/ }))
    await next.user.type(screen.getByRole('textbox', { name: 'Message' }), 'Continue read-only work')
    await next.user.click(screen.getByRole('button', { name: 'Send message' }))
    await waitFor(() => expect(sent(next.invoke, 'send')).toBe(true))
  })

  it('treats a removed conversation owner as unavailable even while the primary is connected', async () => {
    const thread = { id: 'removed-owner-thread', title: 'Worker removed', ownerNodeId: 'worker', turns: [] }
    const snapshot = baseSnapshot({ nodes: [{ nodeId: 'primary', name: 'Primary', primary: true, online: true, epoch: 'primary-epoch-2' }], threads: [thread] })
    const { invoke, user } = renderConnected(snapshot)
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Worker removed/ }))
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Do not send')
    expect(screen.getByRole('button', { name: 'Save draft' })).toBeEnabled()
    await user.click(screen.getByRole('button', { name: 'Save draft' }))
    await user.click(screen.getByRole('button', { name: 'More conversation actions' }))
    expect(screen.getByRole('button', { name: 'Rename conversation' })).toBeDisabled()
    expect(sent(invoke, 'read')).toBe(false)
    expect(sent(invoke, 'send')).toBe(false)
  })

  it('blocks new work on offline nodes and constrains agent execution to canonical availableNodeIds', async () => {
    const snapshot = baseSnapshot({
      nodes: [{ nodeId: 'primary', name: 'Primary', primary: true, online: true, epoch: 'primary-epoch-2' }, { ...worker, online: false }],
      agents: [{ id: 'agent-1', name: 'Scoped agent', ownerNodeId: 'primary', originNodeId: 'primary', availableNodeIds: ['primary'] }],
    })
    const { user } = renderConnected(snapshot)
    await screen.findByRole('heading', { name: 'Chats' })
    const workerOption = await screen.findByRole('option', { name: /Worker/ })
    expect(workerOption).toBeDisabled()
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Scoped agent/ }))
    const executionWorker = await screen.findByRole('option', { name: /Worker/ })
    expect(executionWorker).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Edit profile' })).toBeEnabled()
  })

  it('shows setup state for Git sync and enables worker removal only from an online primary', async () => {
    const snapshot = baseSnapshot()
    const { invoke, user } = renderConnected(snapshot, request => request.channel === 'sync' ? {} : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    expect(screen.getByRole('button', { name: 'Sync now' })).toBeDisabled()
    expect(screen.getByRole('status')).toHaveTextContent(/not been set up/i)
    await user.click(await screen.findByRole('button', { name: 'Details for Worker' }))
    const workerCard = screen.getByRole('button', { name: 'Details for Worker' }).closest<HTMLElement>('.node-card')!
    expect(workerCard.textContent).toContain('Node ID · worker')
    expect(workerCard.textContent).toContain('Epoch · worker-epoch-8')
    await user.click(screen.getByRole('button', { name: 'Remove worker' }))
    await user.click(screen.getByRole('button', { name: 'Confirm removal' }))
    await waitFor(() => expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'fleet' && (args as { request?: { method?: string } })?.request?.method === 'node.remove')).toBe(true))
    const removal = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'fleet' && (args as { request?: { method?: string } })?.request?.method === 'node.remove')
    expect((removal?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ operationId: expect.any(String), params: { nodeId: 'worker' } })
  })

  it('shows unresolved owner mutations but retries only after explicit action with the saved operation ID', async () => {
    const operationId = 'saved-worker-delete'
    const pending = {
      profileId: 'phone', targetNodeId: 'worker', operationId, createdAt: 1, lastAttemptAt: 2, attempts: 1,
      request: { id: 'request-1', channel: 'fleet', method: 'node.remove', params: { nodeId: 'worker' }, operationId, targetNodeId: 'worker' },
      lastError: 'Connection reset',
    }
    const cached = {
      'remote:pending-mutation-index:phone': { operationIds: [operationId] },
      [`remote:pending-mutation:phone:${operationId}`]: pending,
    }
    const { invoke, user } = renderConnected(baseSnapshot(), () => ({ removed: true }), cached)
    await screen.findByRole('heading', { name: 'Chats' })
    expect(await screen.findByRole('button', { name: 'Retry saved action' })).toBeEnabled()
    await user.click(screen.getByRole('button', { name: 'Refresh snapshot' }))
    expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { operationId?: string } }).request?.operationId === operationId)).toBe(false)
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await user.click(screen.getByRole('button', { name: 'Retry saved action' }))
    await waitFor(() => expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { operationId?: string } }).request?.operationId === operationId)).toBe(true))
    const replay = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { operationId?: string } }).request?.operationId === operationId)
    expect(replay?.[1]).toMatchObject({ request: pending.request })
  })

  it('keeps the connection online when a new action is blocked by a pending mutation', async () => {
    const thread = { id: 'thread-pending', title: 'Existing chat', ownerNodeId: 'primary', turns: [] }
    const operationId = 'pending-on-primary'
    const pending = { profileId: 'phone', targetNodeId: 'primary', operationId, request: { id: 'old-request', channel: 'sessions', method: 'send', targetNodeId: 'primary', params: { threadId: thread.id, text: 'Earlier message' }, operationId }, createdAt: 1, lastAttemptAt: 2, attempts: 1 }
    const cached = {
      'remote:pending-mutation-index:phone': { operationIds: [operationId] },
      [`remote:pending-mutation:phone:${operationId}`]: pending,
    }
    const { invoke, user } = renderConnected(baseSnapshot({ threads: [thread] }), request => request.method === 'read' ? thread : {}, cached)
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Existing chat/ }))
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'New message')
    await user.click(screen.getByRole('button', { name: 'Send message' }))
    expect(await screen.findByText(/earlier action for this node is still unresolved/i)).toBeInTheDocument()
    expect(screen.getByText('Connected')).toBeInTheDocument()
    expect(sent(invoke, 'send')).toBe(false)
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('New message')
  })

  it('preserves composer text edited while retrying an older saved message', async () => {
    const thread = { id: 'retry-thread', title: 'Retry chat', ownerNodeId: 'primary', turns: [] }
    const operationId = 'old-message-operation'
    const request = { id: 'old-message-request', channel: 'sessions', method: 'send', targetNodeId: 'primary', params: { threadId: thread.id, text: 'Old saved message' }, operationId }
    const pending = { profileId: 'phone', targetNodeId: 'primary', operationId, request, createdAt: 1, lastAttemptAt: 2, attempts: 1 }
    const cached = {
      'remote:pending-mutation-index:phone': { operationIds: [operationId] },
      [`remote:pending-mutation:phone:${operationId}`]: pending,
      'draft:chat:phone:primary:retry-thread': { text: 'Old saved message', threadId: thread.id, ownerNodeId: 'primary', operationId },
    }
    const sendReply = { resolve: (_result: unknown) => {} }
    const { invoke, user } = renderConnected(baseSnapshot({ threads: [thread] }), incoming => {
      if (incoming.method === 'read') return thread
      if (incoming.operationId === operationId) return new Promise(resolve => { sendReply.resolve = resolve })
      return {}
    }, cached)
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Retry chat/ }))
    const composer = await screen.findByRole('textbox', { name: 'Message' })
    await waitFor(() => expect(composer).toHaveValue('Old saved message'))
    await user.click(screen.getByRole('button', { name: 'Retry saved action' }))
    await waitFor(() => expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { operationId?: string } }).request?.operationId === operationId)).toBe(true))
    await user.clear(composer)
    await user.type(composer, 'New message while retry is pending')
    sendReply.resolve({ id: 'retry-reply', result: { accepted: true } })
    await waitFor(() => expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('New message while retry is pending'))
    expect(invoke.mock.calls.filter(([command, args]) => command === 'remote_request' && (args as { request?: { operationId?: string } }).request?.operationId === operationId)).toHaveLength(1)
  })

  it('clears a matching saved definition on confirmed retry but keeps another profile draft', async () => {
    const operationId = 'task-definition-retry'
    const params = { agentId: 'agent-1', title: 'Create this task', instructions: 'Verify the checksum', acceptanceCriteria: ['Hash matches'] }
    const pending = { profileId: 'phone', targetNodeId: 'worker', operationId, request: { id: 'task-request', channel: 'agents', method: 'tasks.create', targetNodeId: 'worker', params, operationId }, createdAt: 1, lastAttemptAt: 2, attempts: 1 }
    const matching = { id: operationId, operationId, profileId: 'phone', kind: 'task', status: 'saved_offline', targetNodeId: 'worker', params }
    const otherProfile = { id: 'other-task', operationId: 'other-task', profileId: 'other-phone', kind: 'task', status: 'saved_offline', targetNodeId: 'worker', params: { ...params, title: 'Keep this other profile draft' } }
    const cached = {
      'remote:pending-mutation-index:phone': { operationIds: [operationId] },
      [`remote:pending-mutation:phone:${operationId}`]: pending,
      'pending:definitions:phone': [matching],
      'pending:definitions:other-phone': [otherProfile],
    }
    const agent = { id: 'agent-1', name: 'Planner', originNodeId: 'worker', ownerNodeId: 'worker', availableNodeIds: ['worker'] }
    const { invoke, user } = renderConnected(baseSnapshot({ agents: [agent] }), () => ({}), cached)
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    expect(await screen.findByText('Create this task')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Retry saved action' }))
    await waitFor(() => expect(screen.queryByText('Create this task')).not.toBeInTheDocument())
    const writes = invoke.mock.calls.filter(([command, args]) => command === 'remote_cache_put' && (args as { key?: string }).key === 'pending:definitions:phone')
    expect(writes.at(-1)?.[1]).toMatchObject({ key: 'pending:definitions:phone', value: [] })
    expect(cached['pending:definitions:other-phone']).toEqual([otherProfile])
  })

  it('ignores an old profile snapshot callback after unpairing and pairing another node', async () => {
    const oldProfile = profile
    const nextProfile: Profile = { ...profile, id: 'next-phone', name: 'New primary', nodeId: 'new-primary' }
    const nextSnapshot = { ...baseSnapshot(), nodeId: 'new-primary', nodes: [{ nodeId: 'new-primary', name: 'New primary', primary: true, online: true, epoch: 'new-epoch' }], epoch: 'new-epoch', cursor: { epoch: 'new-epoch', sequence: 0 } }
    const oldSnapshot = { resolve: undefined as ((snapshot: Snapshot) => void) | undefined }
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      if (command === 'remote_profiles') return [oldProfile] as T
      if (command === 'remote_snapshot') {
        if (args?.profileId === oldProfile.id) return new Promise<Snapshot>(resolve => { oldSnapshot.resolve = resolve }) as T
        if (args?.profileId === nextProfile.id) return nextSnapshot as T
      }
      if (command === 'remote_pair') return nextProfile as T
      if (command === 'remote_cache_get') return null as T
      if (command === 'remote_cache_put' || command === 'remote_subscribe' || command === 'remote_unsubscribe' || command === 'remote_forget') return undefined as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(oldSnapshot.resolve).toBeDefined())
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await user.click(screen.getByRole('button', { name: 'Forget node' }))
    await screen.findByRole('heading', { name: 'Pair your Brr node' })
    const invitation = { endpoint: 'https://new.local', code: 'one-use', fingerprint: 'a'.repeat(64), certificatePem: '-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----' }
    await user.click(screen.getByRole('textbox', { name: 'Pairing link or invitation' }))
    await user.paste(JSON.stringify(invitation))
    await user.click(screen.getByRole('button', { name: 'Pair this phone' }))
    await waitFor(() => expect(screen.getAllByText('New primary').length).toBeGreaterThan(0))
    oldSnapshot.resolve?.(baseSnapshot())
    await waitFor(() => expect(screen.getAllByText('New primary').length).toBeGreaterThan(0))
    expect(invoke.mock.calls.some(([command, args]) => command === 'remote_cache_put' && (args as { key?: string }).key === `snapshot:${oldProfile.id}`)).toBe(false)
    expect(invoke).toHaveBeenCalledWith('remote_cache_put', expect.objectContaining({ key: `snapshot:${nextProfile.id}`, value: nextSnapshot }))
  })

  it('clears the forgotten profile chat, draft, and execution-node selection before pairing another node', async () => {
    const oldProfile: Profile = { ...profile, id: 'old-phone', nodeId: 'old-primary' }
    const nextProfile: Profile = { ...profile, id: 'next-phone', name: 'New primary', nodeId: 'new-primary' }
    const oldThread = { id: 'old-thread', title: 'Forgotten conversation', ownerNodeId: 'old-primary', historical: true }
    const oldSnapshot = baseSnapshot({ nodeId: 'old-primary', threads: [oldThread], nodes: [{ nodeId: 'old-primary', name: 'Old primary', primary: true, online: true }] })
    const nextSnapshot = baseSnapshot({ nodeId: 'new-primary', threads: [], nodes: [{ nodeId: 'new-primary', name: 'New primary', primary: true, online: true }] })
    const oldDraftKey = 'draft:chat:old-phone:old-primary:old-thread'
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      if (command === 'remote_profiles') return [oldProfile] as T
      if (command === 'remote_snapshot') return (args?.profileId === nextProfile.id ? nextSnapshot : oldSnapshot) as T
      if (command === 'remote_cache_get') return (args?.key === oldDraftKey ? { text: 'Native offline draft reviewed.' } : null) as T
      if (command === 'remote_pair') return nextProfile as T
      if (command === 'remote_cache_put' || command === 'remote_subscribe' || command === 'remote_unsubscribe' || command === 'remote_forget') return undefined as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)

    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Forgotten conversation/ }))
    await waitFor(() => expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Native offline draft reviewed.'))

    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await user.click(await screen.findByRole('button', { name: 'Forget node' }))
    await screen.findByRole('heading', { name: 'Pair your Brr node' })
    const invitation = { endpoint: 'https://new.local', code: 'one-use', fingerprint: 'a'.repeat(64), certificatePem: '-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----' }
    await user.click(screen.getByRole('textbox', { name: 'Pairing link or invitation' }))
    await user.paste(JSON.stringify(invitation))
    await user.click(screen.getByRole('button', { name: 'Pair this phone' }))
    await waitFor(() => expect(screen.getAllByText('New primary').length).toBeGreaterThan(0))

    await user.click(screen.getByRole('tab', { name: /Chats/ }))
    expect(screen.queryByText('Forgotten conversation')).not.toBeInTheDocument()
    expect(screen.queryByDisplayValue('Native offline draft reviewed.')).not.toBeInTheDocument()
    expect(screen.getByLabelText('Execution node')).toHaveValue('new-primary')
    await user.click(screen.getByRole('button', { name: 'New session' }))
    expect(screen.getByText(/New session · New primary/)).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('')
  })

  it('preserves the active chat and its local draft across same-profile reconnect', async () => {
    const thread = { id: 'reconnect-thread', title: 'Keep this conversation', ownerNodeId: 'primary', historical: true }
    const snapshot = baseSnapshot({ threads: [thread] })
    const draftKey = 'draft:chat:phone:primary:reconnect-thread'
    const { user, emit } = renderConnected(snapshot, () => ({}), {
      [draftKey]: { text: 'Keep this same-profile draft.' },
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Keep this conversation/ }))
    const composer = screen.getByRole('textbox', { name: 'Message' })
    await waitFor(() => expect(composer).toHaveValue('Keep this same-profile draft.'))

    act(() => emit('remote-disconnect', { profileId: profile.id, message: 'Connection paused' }))
    await user.click(screen.getByRole('button', { name: 'Refresh snapshot' }))

    expect(await screen.findByRole('heading', { name: 'Keep this conversation' })).toBeInTheDocument()
    expect(composer).toHaveValue('Keep this same-profile draft.')
  })

  it('ignores an old profile sessions.read that resolves after Forget and a new pairing', async () => {
    const oldProfile: Profile = { ...profile, id: 'read-old-phone', nodeId: 'old-primary' }
    const nextProfile: Profile = { ...profile, id: 'read-next-phone', name: 'New primary', nodeId: 'new-primary' }
    const thread = { id: 'late-old-thread', title: 'Late old conversation', ownerNodeId: 'old-primary', turns: [] }
    const oldSnapshot = baseSnapshot({ nodeId: 'old-primary', threads: [thread], nodes: [{ nodeId: 'old-primary', name: 'Old primary', primary: true, online: true }] })
    const nextSnapshot = baseSnapshot({ nodeId: 'new-primary', nodes: [{ nodeId: 'new-primary', name: 'New primary', primary: true, online: true }] })
    let finishRead!: (reply: unknown) => void
    let readStarted!: () => void
    let oldReadRequestId = ''
    const oldReadStarted = new Promise<void>(resolve => { readStarted = resolve })
    const oldRead = new Promise<unknown>(resolve => { finishRead = resolve })
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      if (command === 'remote_profiles') return [oldProfile] as T
      if (command === 'remote_snapshot') return (args?.profileId === nextProfile.id ? nextSnapshot : oldSnapshot) as T
      if (command === 'remote_pair') return nextProfile as T
      if (command === 'remote_request' && (args?.request as { method?: string })?.method === 'read') {
        oldReadRequestId = (args?.request as { id?: string }).id ?? ''
        readStarted()
        return oldRead as T
      }
      if (command === 'remote_cache_get') return null as T
      if (command === 'remote_cache_put' || command === 'remote_subscribe' || command === 'remote_unsubscribe' || command === 'remote_forget') return undefined as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)

    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Late old conversation/ }))
    await oldReadStarted
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await user.click(await screen.findByRole('button', { name: 'Forget node' }))
    await screen.findByRole('heading', { name: 'Pair your Brr node' })
    const invitation = { endpoint: 'https://new.local', code: 'one-use', fingerprint: 'a'.repeat(64), certificatePem: '-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----' }
    await user.click(screen.getByRole('textbox', { name: 'Pairing link or invitation' }))
    await user.paste(JSON.stringify(invitation))
    await user.click(screen.getByRole('button', { name: 'Pair this phone' }))
    await waitFor(() => expect(screen.getAllByText('New primary').length).toBeGreaterThan(0))
    await user.click(screen.getByRole('tab', { name: /Chats/ }))

    await act(async () => {
      finishRead({ id: oldReadRequestId, result: thread })
      await oldRead
      await new Promise(resolve => setTimeout(resolve, 0))
    })
    expect(screen.queryByText('Late old conversation')).not.toBeInTheDocument()
    expect(screen.getByText('No conversations yet')).toBeInTheDocument()
    expect(screen.getByLabelText('Execution node')).toHaveValue('new-primary')
  })

  it('ignores a late sessions.read after the user backs out and selects another conversation', async () => {
    const first = { id: 'first-thread', title: 'First conversation', ownerNodeId: 'primary', turns: [] }
    const second = { id: 'second-thread', title: 'Second conversation', ownerNodeId: 'primary', turns: [] }
    const snapshot = baseSnapshot({ threads: [first, second] })
    let finishFirstRead!: (reply: unknown) => void
    let firstReadStarted!: () => void
    let firstRequestId = ''
    const firstReadStartedPromise = new Promise<void>(resolve => { firstReadStarted = resolve })
    const firstRead = new Promise<unknown>(resolve => { finishFirstRead = resolve })
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      if (command === 'remote_profiles') return [profile] as T
      if (command === 'remote_snapshot') return snapshot as T
      if (command === 'remote_request' && (args?.request as { method?: string })?.method === 'read') {
        const request = args?.request as { id?: string; params?: { threadId?: string } }
        if (request.params?.threadId === first.id) {
          firstRequestId = request.id ?? ''
          firstReadStarted()
          return firstRead as T
        }
        return { id: request.id, result: second } as T
      }
      if (command === 'remote_cache_get') return null as T
      if (command === 'remote_cache_put' || command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)

    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /First conversation/ }))
    await firstReadStartedPromise
    await user.click(screen.getByRole('button', { name: 'Back to chats' }))
    await user.click(await screen.findByRole('button', { name: /Second conversation/ }))
    expect(await screen.findByRole('heading', { name: 'Second conversation' })).toBeInTheDocument()

    await act(async () => {
      finishFirstRead({ id: firstRequestId, result: first })
      await firstRead
      await new Promise(resolve => setTimeout(resolve, 0))
    })
    expect(screen.getByRole('heading', { name: 'Second conversation' })).toBeInTheDocument()
  })

  it('ignores an old profile sessions.start completion after Forget and a new pairing', async () => {
    const oldProfile: Profile = { ...profile, id: 'send-old-phone', nodeId: 'old-primary' }
    const nextProfile: Profile = { ...profile, id: 'send-next-phone', name: 'New primary', nodeId: 'new-primary' }
    const oldSnapshot = baseSnapshot({ nodeId: 'old-primary', nodes: [{ nodeId: 'old-primary', name: 'Old primary', primary: true, online: true }] })
    const nextSnapshot = baseSnapshot({ nodeId: 'new-primary', nodes: [{ nodeId: 'new-primary', name: 'New primary', primary: true, online: true }] })
    const thread = { id: 'late-start-thread', title: 'Late old started session', ownerNodeId: 'old-primary', turns: [] }
    let finishStart!: (reply: unknown) => void
    let startStarted!: () => void
    let oldStartRequestId = ''
    const oldStartStarted = new Promise<void>(resolve => { startStarted = resolve })
    const oldStart = new Promise<unknown>(resolve => { finishStart = resolve })
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      if (command === 'remote_profiles') return [oldProfile] as T
      if (command === 'remote_snapshot') return (args?.profileId === nextProfile.id ? nextSnapshot : oldSnapshot) as T
      if (command === 'remote_pair') return nextProfile as T
      if (command === 'remote_request' && (args?.request as { method?: string })?.method === 'start') {
        oldStartRequestId = (args?.request as { id?: string }).id ?? ''
        startStarted()
        return oldStart as T
      }
      if (command === 'remote_cache_get') return null as T
      if (command === 'remote_cache_put' || command === 'remote_subscribe' || command === 'remote_unsubscribe' || command === 'remote_forget') return undefined as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)

    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: 'New session' }))
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Start on the old profile')
    await user.click(screen.getByRole('button', { name: 'Start session' }))
    await oldStartStarted
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await user.click(await screen.findByRole('button', { name: 'Forget node' }))
    await screen.findByRole('heading', { name: 'Pair your Brr node' })
    const invitation = { endpoint: 'https://new.local', code: 'one-use', fingerprint: 'a'.repeat(64), certificatePem: '-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----' }
    await user.click(screen.getByRole('textbox', { name: 'Pairing link or invitation' }))
    await user.paste(JSON.stringify(invitation))
    await user.click(screen.getByRole('button', { name: 'Pair this phone' }))
    await waitFor(() => expect(screen.getAllByText('New primary').length).toBeGreaterThan(0))
    await user.click(screen.getByRole('tab', { name: /Chats/ }))

    await act(async () => {
      finishStart({ id: oldStartRequestId, result: { thread } })
      await oldStart
      await new Promise(resolve => setTimeout(resolve, 0))
    })
    expect(screen.queryByText('Late old started session')).not.toBeInTheDocument()
    expect(screen.getByText('No conversations yet')).toBeInTheDocument()
    expect(screen.getByLabelText('Execution node')).toHaveValue('new-primary')
  })

  it('does not offer worker removal to a non-primary paired node', async () => {
    const snapshot = baseSnapshot({ nodeId: 'worker', nodes: [{ nodeId: 'primary', name: 'Primary', primary: true, online: true }, { ...worker, primary: false }] })
    const { user } = renderConnected(snapshot)
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await user.click(await screen.findByRole('button', { name: 'Details for Worker' }))
    expect(screen.queryByRole('button', { name: 'Remove worker' })).not.toBeInTheDocument()
  })

  it('starts a new chat on the execution node selected in Chats', async () => {
    const snapshot = baseSnapshot({ models: [{ id: 'model-x', model: 'model-x', name: 'Model X', supportedReasoningEfforts: [{ reasoningEffort: 'low' }, { reasoningEffort: 'high' }] }, { id: 'model-y', model: 'model-y', name: 'Model Y' }] })
    const { invoke, user } = renderConnected(snapshot, request => request.channel === 'system' && request.method === 'models' ? { data: snapshot.models }
      : request.method === 'start' ? { thread: { id: 'new-thread', ownerNodeId: 'worker' } } : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await screen.findByRole('option', { name: /Worker/ })
    await user.selectOptions(screen.getByLabelText('Execution node'), 'worker')
    await user.click(screen.getByRole('button', { name: 'New session' }))
    expect(screen.getByText('New session · Worker')).toBeVisible()
    await user.selectOptions(screen.getByLabelText('Model'), 'model-y')
    await user.selectOptions(screen.getByLabelText('Reasoning'), 'high')
    await user.selectOptions(screen.getByLabelText('Mode'), 'plan')
    await user.selectOptions(screen.getByLabelText('Access'), 'read-only')
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Check the build status')
    await user.click(screen.getByRole('button', { name: 'Start session' }))
    await waitFor(() => expect(sent(invoke, 'start')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'start')
    expect((call?.[1] as { request: { targetNodeId: string; params: Record<string, unknown> } }).request).toMatchObject({ targetNodeId: 'worker', params: { text: 'Check the build status', nodeId: 'worker', model: 'model-y', effort: 'high', mode: 'plan', readOnly: true } })
  })

  it('loads models from the selected owner and starts in an available canonical project', async () => {
    const workerProject = { id: 'fleet_project_worker', name: 'Worker notes', originNodeId: 'primary', availableNodeIds: ['worker'] }
    const primaryProject = { id: 'fleet_project_primary', name: 'Primary notes', originNodeId: 'primary', availableNodeIds: ['primary'] }
    const snapshot = baseSnapshot({
      models: [{ id: 'primary-model', model: 'primary-model', name: 'Primary model' }],
      projects: [primaryProject, workerProject, { id: 'local-only-project', name: 'Local only', availableNodeIds: ['worker'] }],
    })
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.channel === 'system' && request.method === 'models' && request.targetNodeId === 'worker') {
        return { data: [{ id: 'worker-model', model: 'worker-model', name: 'Worker model' }] }
      }
      if (request.channel === 'sessions' && request.method === 'start') {
        return { thread: { id: 'worker-session', ownerNodeId: 'worker' } }
      }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await screen.findByRole('option', { name: /Worker/ })
    await user.selectOptions(screen.getByLabelText('Execution node'), 'worker')
    await user.click(screen.getByRole('button', { name: 'New session' }))
    expect(await screen.findByRole('option', { name: 'Worker model' })).toBeInTheDocument()
    expect(screen.queryByRole('option', { name: 'Primary model' })).not.toBeInTheDocument()
    expect(screen.queryByRole('option', { name: 'Primary notes' })).not.toBeInTheDocument()
    expect(screen.queryByRole('option', { name: 'Local only' })).not.toBeInTheDocument()
    await user.selectOptions(screen.getByLabelText('Project'), 'fleet_project_worker')
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Continue in the worker project')
    await user.click(screen.getByRole('button', { name: 'Start session' }))
    await waitFor(() => expect(sent(invoke, 'start')).toBe(true))
    const requests = invoke.mock.calls
      .filter(([command]) => command === 'remote_request')
      .map(([, args]) => (args as { request: Record<string, unknown> }).request)
    expect(requests).toContainEqual(expect.objectContaining({ channel: 'system', method: 'models', targetNodeId: 'worker' }))
    expect(requests).toContainEqual(expect.objectContaining({
      channel: 'sessions', method: 'start', targetNodeId: 'worker',
      params: expect.objectContaining({ model: 'worker-model', projectId: 'fleet_project_worker', nodeId: 'worker' }),
    }))
  })

  it('keeps the connection online and offers runtime default when the selected owner model list fails', async () => {
    const snapshot = baseSnapshot({ models: [{ id: 'primary-model', model: 'primary-model', name: 'Primary model' }] })
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.channel === 'system' && request.method === 'models' && request.targetNodeId === 'worker') {
        throw new Error('Worker model service unavailable')
      }
      if (request.channel === 'sessions' && request.method === 'start') return { thread: { id: 'worker-default-session', ownerNodeId: 'worker' } }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await screen.findByRole('option', { name: /Worker/ })
    await user.selectOptions(screen.getByLabelText('Execution node'), 'worker')
    await user.click(screen.getByRole('button', { name: 'New session' }))
    expect(await screen.findByText(/Model list unavailable/)).toBeInTheDocument()
    expect(screen.getByText('Connected')).toBeInTheDocument()
    const model = screen.getByLabelText('Model') as HTMLSelectElement
    expect(model).toHaveValue('')
    expect(screen.queryByRole('option', { name: 'Primary model' })).not.toBeInTheDocument()
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Use the worker default')
    await user.click(screen.getByRole('button', { name: 'Start session' }))
    await waitFor(() => expect(sent(invoke, 'start')).toBe(true))
    const start = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } }).request?.method === 'start')
    expect((start?.[1] as { request: { params: Record<string, unknown>; targetNodeId: string } }).request).toMatchObject({ targetNodeId: 'worker', params: { model: '', nodeId: 'worker' } })
  })

  it('ignores a stale model response after changing the selected owner', async () => {
    let resolveWorker: ((value: unknown) => void) | undefined
    const snapshot = baseSnapshot({ models: [{ id: 'primary-model', model: 'primary-model', name: 'Primary model' }] })
    const { user } = renderConnected(snapshot, request => {
      if (request.channel === 'system' && request.method === 'models' && request.targetNodeId === 'worker') {
        return new Promise(resolve => { resolveWorker = resolve })
      }
      if (request.channel === 'system' && request.method === 'models' && request.targetNodeId === 'primary') {
        return { data: [{ id: 'primary-live', model: 'primary-live', name: 'Primary live model' }] }
      }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await screen.findByRole('option', { name: /Worker/ })
    await user.selectOptions(screen.getByLabelText('Execution node'), 'worker')
    await user.click(screen.getByRole('button', { name: 'New session' }))
    await waitFor(() => expect(resolveWorker).toBeDefined())
    await user.click(screen.getByRole('button', { name: 'Back to chats' }))
    await user.selectOptions(screen.getByLabelText('Execution node'), 'primary')
    await user.click(screen.getByRole('button', { name: 'New session' }))
    expect(await screen.findByRole('option', { name: 'Primary live model' })).toBeInTheDocument()
    resolveWorker?.({ result: { data: [{ id: 'worker-late', model: 'worker-late', name: 'Late worker model' }] } })
    await waitFor(() => expect(screen.queryByRole('option', { name: 'Late worker model' })).not.toBeInTheDocument())
    expect(screen.getByRole('combobox', { name: 'Model' })).toHaveValue('primary-live')
  })

  it('keeps an existing thread model selected while loading choices from that thread owner', async () => {
    const thread = { id: 'saved-model-thread', title: 'Saved model thread', ownerNodeId: 'worker', model: 'saved-worker-model', turns: [] }
    const snapshot = baseSnapshot({ threads: [thread], models: [{ id: 'primary-model', model: 'primary-model', name: 'Primary model' }] })
    const { user } = renderConnected(snapshot, request => {
      if (request.channel === 'system' && request.method === 'models' && request.targetNodeId === 'worker') {
        return { data: [{ id: 'other-worker-model', model: 'other-worker-model', name: 'Other worker model' }] }
      }
      return request.method === 'read' ? thread : {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Saved model thread/ }))
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Model' })).toHaveValue('saved-worker-model'))
    expect(screen.getByRole('option', { name: 'Saved model · saved-worker-model' })).toBeInTheDocument()
    expect(screen.queryByRole('option', { name: 'Primary model' })).not.toBeInTheDocument()
    await user.selectOptions(screen.getByRole('combobox', { name: 'Model' }), '')
    expect(screen.getByRole('combobox', { name: 'Model' })).toHaveValue('')
  })

  it('starts a new-agent interview on its explicitly selected execution node', async () => {
    const snapshot = baseSnapshot()
    const { invoke, user } = renderConnected(snapshot, request => request.method === 'agents.interview' ? { thread: { id: 'interview-worker', ownerNodeId: 'worker' } } : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.selectOptions(screen.getByLabelText('New agent execution node'), 'worker')
    await user.click(screen.getByRole('button', { name: 'New agent' }))
    await waitFor(() => expect(sent(invoke, 'agents.interview')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'agents.interview')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker' })
  })

  it('answers a blocking question using its option and edited free text on the owning epoch', async () => {
    const snapshot = baseSnapshot({ questions: [{ id: 'q-1', threadId: 't-1', turnId: 'turn-1', itemId: 'item-1', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', kind: 'blocking', resolution: 'pending', delivery: 'none', draft: {}, questions: [{ id: 'scope', header: 'Scope', question: 'Which scope?', options: [{ label: 'Project', description: 'One project' }] }, { id: 'purpose', header: 'Purpose', question: 'What should be included?' }] }] })
    const { invoke, user } = renderConnected(snapshot, attentionRoute(snapshot))
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))
    await user.click(await screen.findByRole('radio', { name: /Project/ }))
    await user.click(screen.getByRole('button', { name: 'Next' }))
    await user.type(screen.getByRole('textbox', { name: /What should be included/ }), 'Only verified changes')
    await user.click(screen.getByRole('button', { name: 'Submit answers' }))
    await waitFor(() => expect(sent(invoke, 'answer')).toBe(true))
    const calls = invoke.mock.calls.filter(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'questions' && (args as { request?: { method?: string } })?.request?.method === 'answer')
    expect((calls[0]?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedEpoch: 'worker-epoch-8', params: { requestId: 'q-1', answers: { scope: ['Project'], purpose: ['Only verified changes'] } } })
    const drafts = invoke.mock.calls.filter(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'draft')
    expect((drafts.at(-1)?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ params: { requestId: 'q-1', answers: { scope: ['Project'], purpose: ['Only verified changes'] } } })
    expect(invoke).toHaveBeenCalledWith('remote_cache_put', expect.objectContaining({ key: 'draft:question:phone:q-1', value: { scope: ['Project'], purpose: ['Only verified changes'] } }))
  })

  it('restores a saved question draft from the phone cache', async () => {
    const question = { id: 'q-restore', threadId: 't-1', turnId: 'turn-1', itemId: 'item-1', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', kind: 'blocking', resolution: 'pending', delivery: 'none', draft: {}, questions: [{ id: 'scope', header: 'Scope', question: 'Which scope?', options: [{ label: 'Project', description: 'One project' }, { label: 'Workspace', description: 'All files' }] }] }
    const snapshot = baseSnapshot({ questions: [question] })
    const { user } = renderConnected(snapshot, attentionRoute(snapshot), { 'draft:question:phone:q-restore': { scope: ['Workspace'] } })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))
    expect(await screen.findByRole('radio', { name: /Workspace/ })).toBeChecked()
  })

  it('accepts the reviewed command with the original execution owner and runtime epoch', async () => {
    const snapshot = baseSnapshot({ approvals: [{ id: 'approval-8', requestId: 8, ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', status: 'pending', method: 'item/commandExecution/requestApproval', params: { threadId: 't-2', turnId: 'turn-9', command: 'pnpm test', cwd: '/workspace/brr', reason: 'Run tests before review' } }] })
    const { invoke, user } = renderConnected(snapshot, attentionRoute(snapshot))
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))
    expect(await screen.findByText('pnpm test')).toBeInTheDocument()
    expect(screen.getByText('Working directory · /workspace/brr')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Accept' }))
    await waitFor(() => expect(sent(invoke, 'answer')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'approvals' && (args as { request?: { method?: string } })?.request?.method === 'answer')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedEpoch: 'worker-epoch-8', params: { requestId: 8, result: { decision: 'accept' }, threadId: 't-2', turnId: 'turn-9' } })
  })

  it('preserves numeric approval request IDs when answering the original runtime request', async () => {
    const snapshot = baseSnapshot({ approvals: [{ id: 'approval-number-123', requestId: 123, ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', status: 'pending', method: 'item/fileChange/requestApproval', params: { threadId: 't-2', turnId: 'turn-9', itemId: 'change-1', reason: 'Write the reviewed file' } }] })
    const { invoke, user } = renderConnected(snapshot, attentionRoute(snapshot))
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))
    await user.click(await screen.findByRole('button', { name: 'Accept' }))
    await waitFor(() => expect(sent(invoke, 'answer')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'approvals' && (args as { request?: { method?: string } })?.request?.method === 'answer')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedEpoch: 'worker-epoch-8', params: { requestId: 123, result: { decision: 'accept' }, threadId: 't-2', turnId: 'turn-9' } })
  })

  it('keeps fleet approvals from both owners visible and routes same-ID decisions to the exact owner epoch', async () => {
    const primaryApproval = { id: 'primary-approval', requestId: 901, ownerNodeId: 'primary', ownerEpoch: 'primary-epoch-2', status: 'pending', method: 'item/commandExecution/requestApproval', params: { threadId: 'primary-thread', turnId: 'primary-turn', command: 'primary check', reason: 'Primary decision remains open' } }
    const workerApproval = { id: 'worker-approval', requestId: 901, ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', status: 'pending', method: 'item/commandExecution/requestApproval', params: { threadId: 'worker-thread', turnId: 'worker-turn', command: 'worker check', reason: 'Worker decision needs review' } }
    let currentSnapshot = baseSnapshot({ approvals: [primaryApproval, workerApproval] })
    let answeredRequest: Record<string, unknown> | undefined
    const { user } = renderConnected(currentSnapshot, request => {
      if (request.channel === 'approvals' && request.method === 'list') return { data: [primaryApproval] }
      if (request.channel === 'approvals' && request.method === 'answer') {
        answeredRequest = request
        currentSnapshot = { ...currentSnapshot, approvals: [primaryApproval] }
        return { accepted: true }
      }
      return {}
    }, {}, () => currentSnapshot)

    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))
    expect(await screen.findByText('Primary decision remains open')).toBeInTheDocument()
    expect(await screen.findByText('Worker decision needs review')).toBeInTheDocument()

    const workerCard = screen.getByText('Worker decision needs review').closest('article')
    expect(workerCard).not.toBeNull()
    await user.click(within(workerCard as HTMLElement).getByRole('button', { name: 'Decline' }))
    await waitFor(() => expect(answeredRequest).toMatchObject({
      targetNodeId: 'worker', expectedEpoch: 'worker-epoch-8',
      params: { requestId: 901, result: { decision: 'decline' }, threadId: 'worker-thread', turnId: 'worker-turn' },
    }))
    expect(await screen.findByText('Primary decision remains open')).toBeInTheDocument()
    expect(screen.queryByText('Worker decision needs review')).not.toBeInTheDocument()
  })

  it('grants only the exact requested permissions for the current turn', async () => {
    const permissions = { fileSystem: { write: ['/workspace/release-notes'] }, network: { enabled: true } }
    const snapshot = baseSnapshot({ approvals: [{ id: 'approval-permission', requestId: 77, ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', status: 'pending', method: 'item/permissions/requestApproval', params: { threadId: 't-3', turnId: 'turn-10', reason: 'Write the reviewed release note', permissions } }] })
    const { invoke, user } = renderConnected(snapshot, attentionRoute(snapshot))
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))
    expect(await screen.findByText('Write the reviewed release note')).toBeInTheDocument()
    expect(screen.getByLabelText('Requested permissions')).toHaveTextContent('/workspace/release-notes')
    await user.click(screen.getByRole('button', { name: 'Allow for this turn' }))
    await waitFor(() => expect(sent(invoke, 'answer')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'approvals' && (args as { request?: { method?: string } })?.request?.method === 'answer')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedEpoch: 'worker-epoch-8', params: { requestId: 77, threadId: 't-3', turnId: 'turn-10', result: { permissions, scope: 'turn' } } })
  })

  it('collects schema fields before accepting an MCP elicitation', async () => {
    const snapshot = baseSnapshot({ approvals: [{ id: 'mcp-approval', requestId: 'mcp-runtime-1', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', status: 'pending', method: 'mcpServer/elicitation/request', params: { serverName: 'Release tracker', message: 'Choose a release channel', mode: 'form', requestedSchema: { type: 'object', required: ['channel'], properties: { channel: { type: 'string', title: 'Channel', enum: ['stable', 'beta'] }, notify: { type: 'boolean', title: 'Notify team', default: false } } } } }] })
    const { invoke, user } = renderConnected(snapshot, attentionRoute(snapshot))
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))
    await user.selectOptions(await screen.findByLabelText('Channel'), 'beta')
    await user.click(screen.getByLabelText('Notify team'))
    await user.click(screen.getByRole('button', { name: 'Submit response' }))
    await waitFor(() => expect(sent(invoke, 'answer')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'approvals' && (args as { request?: { method?: string } })?.request?.method === 'answer')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedEpoch: 'worker-epoch-8', params: { requestId: 'mcp-runtime-1', result: { action: 'accept', content: { channel: 'beta', notify: true }, _meta: null } } })
  })

  it('does not show grant actions for unsupported runtime approval methods', async () => {
    const snapshot = baseSnapshot({ approvals: [{ id: 'unsupported', requestId: 41, ownerNodeId: 'worker', status: 'pending', method: 'workspace/write', params: { command: 'not a supported runtime approval' } }] })
    const { invoke, user } = renderConnected(snapshot, attentionRoute(snapshot))
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Attention/ }))
    expect(await screen.findByRole('heading', { name: 'Unsupported runtime request' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /allow|accept|decline/i })).not.toBeInTheDocument()
    expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'approvals' && (args as { request?: { method?: string } })?.request?.method === 'answer')).toBe(false)
  })

  it('approves a reviewed profile proposal on the agent owning node', async () => {
    const agent = { id: 'agent-1', name: 'Planner', ownerNodeId: 'worker', revision: 'agent-rev-3' }
    const draft = { id: 'draft-1', agentId: 'agent-1', hash: 'hash-1', status: 'proposed', ownerNodeId: 'worker', candidate: { name: 'Planner v2', instructions: 'Plan carefully' } }
    const snapshot = baseSnapshot({ agents: [agent] })
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.method === 'agents.read') return agent
      if (request.method === 'agents.drafts') return { data: [draft] }
      if (request.method === 'agents.approve') return { agent }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Profile' }))
    await user.click(await screen.findByRole('button', { name: 'Approve profile' }))
    await waitFor(() => expect(sent(invoke, 'agents.approve')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'agents.approve')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedRevision: 'hash-1', params: { draftId: 'draft-1', hash: 'hash-1' } })
  })

  it('formats Rust epoch-second timestamps in the conversation list', async () => {
    const timestamp = 1_700_000_000
    const expected = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' }).format(new Date(timestamp * 1000))
    const snapshot = baseSnapshot({ threads: [{ id: 'epoch-thread', title: 'Epoch thread', updatedAt: timestamp }] })
    renderConnected(snapshot)
    await screen.findByRole('heading', { name: 'Chats' })
    expect(await screen.findByText(expected)).toBeInTheDocument()
  })

  it('reviews a new profile draft from the agents roster on its interview node', async () => {
    const draft = { id: 'fresh-draft', threadId: 'interview-thread', agentId: null, status: 'proposed', hash: 'fresh-hash', candidate: { name: 'First agent', description: 'A new profile', instructions: 'Plan releases', soul: 'Be careful', model: 'model-x', effort: 'medium' } }
    const snapshot = baseSnapshot()
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.method === 'agents.drafts') return { data: [draft] }
      if (request.method === 'agents.approve') return { agent: { id: 'first-agent', name: 'First agent' } }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.selectOptions(await screen.findByLabelText('New agent execution node'), 'worker')
    expect(await screen.findByRole('heading', { name: 'First agent' })).toBeInTheDocument()
    expect(screen.getByText('fresh-hash')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Approve and create' }))
    await waitFor(() => expect(sent(invoke, 'agents.approve')).toBe(true))
    const approval = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'agents.approve')
    expect((approval?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedRevision: 'fresh-hash', params: { draftId: 'fresh-draft', hash: 'fresh-hash' } })
  })

  it('shows actions only for exact-hash proposed or approved profile drafts', async () => {
    const agent = { id: 'agent-review', name: 'Reviewer', ownerNodeId: 'worker' }
    const drafts = [
      { id: 'proposed-1', agentId: 'agent-review', status: 'proposed', hash: 'hash-p', candidate: { name: 'Proposed candidate' } },
      { id: 'approved-1', agentId: 'agent-review', status: 'approved', hash: 'hash-a', candidate: { name: 'Approved candidate' } },
      { id: 'applied-1', agentId: 'agent-review', status: 'applied', hash: 'hash-x', candidate: { name: 'Applied candidate' } },
      { id: 'rejected-1', agentId: 'agent-review', status: 'rejected', hash: 'hash-r', candidate: { name: 'Rejected candidate' } },
      { id: 'missing-hash', agentId: 'agent-review', status: 'proposed', candidate: { name: 'Unverifiable candidate' } },
    ]
    const snapshot = baseSnapshot({ agents: [agent] })
    const { user } = renderConnected(snapshot, request => request.method === 'agents.read' ? agent : request.method === 'agents.drafts' ? { data: drafts } : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Reviewer/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(within(screen.getByRole('tablist', { name: 'Agent sections' })).getByRole('tab', { name: 'Profile' }))
    expect(await screen.findByText('Proposed candidate')).toBeInTheDocument()
    expect(await screen.findByText('Approved candidate')).toBeInTheDocument()
    expect(screen.queryByText('Applied candidate')).not.toBeInTheDocument()
    expect(screen.queryByText('Rejected candidate')).not.toBeInTheDocument()
    expect(screen.queryByText('Unverifiable candidate')).not.toBeInTheDocument()
    expect(screen.getAllByRole('button', { name: 'Approve profile' })).toHaveLength(2)
  })

  it('opens an agent conversation on the explicitly selected node, separate from its origin', async () => {
    const agent = { id: 'shared-agent', name: 'Planner', originNodeId: 'primary', ownerNodeId: 'primary', availableNodeIds: ['primary', 'worker'], revision: 'agent-r1' }
    const snapshot = baseSnapshot({ agents: [agent] })
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.method === 'agents.read') return agent
      if (request.method === 'agents.open') return { thread: { id: 'thread-worker', title: 'Planner on worker', ownerNodeId: 'worker' } }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(screen.getAllByRole('tab', { name: 'Chats' })[0])
    await user.click(await screen.findByRole('button', { name: 'Open chat' }))
    await waitFor(() => expect(sent(invoke, 'agents.open')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'agents.open')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { agentId: 'shared-agent' } })
  })

  it('opens the selected saved agent conversation by thread id and owner', async () => {
    const agent = { id: 'shared-agent', name: 'Planner', ownerNodeId: 'primary', revision: 'agent-r1' }
    const savedThread = { id: 'saved-thread-7', title: 'Earlier planning', ownerNodeId: 'worker', brr: { agentId: 'shared-agent', kind: 'agent' } }
    const snapshot = baseSnapshot({ agents: [agent] })
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.method === 'agents.read') return agent
      if (request.method === 'agents.sessions') return { data: [savedThread] }
      if (request.channel === 'sessions' && request.method === 'read') return { ...savedThread, turns: [] }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(within(screen.getByRole('tablist', { name: 'Agent sections' })).getByRole('tab', { name: 'Chats' }))
    await user.click(await screen.findByRole('button', { name: /Earlier planning/ }))
    await waitFor(() => expect(sent(invoke, 'read')).toBe(true))
    const read = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'sessions' && (args as { request?: { method?: string } })?.request?.method === 'read')
    expect((read?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { threadId: 'saved-thread-7' } })
    expect(sent(invoke, 'agents.open')).toBe(false)
  })

  it('creates new agent tasks on the selected execution node', async () => {
    const agent = { id: 'shared-agent', name: 'Planner', originNodeId: 'primary', ownerNodeId: 'primary', availableNodeIds: ['primary', 'worker'], revision: 'agent-r4' }
    const snapshot = baseSnapshot({ agents: [agent] })
    const { invoke, user } = renderConnected(snapshot, request => request.method === 'agents.read' ? agent : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.type(screen.getByLabelText('Title'), 'Verify release')
    await user.type(screen.getAllByLabelText('Instructions')[0], 'Check the release evidence')
    await user.type(screen.getByLabelText(/Acceptance criteria/), 'Every result is linked')
    await user.click(screen.getByRole('button', { name: 'Create task' }))
    await waitFor(() => expect(sent(invoke, 'tasks.create')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'tasks.create')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedRevision: 'agent-r4', params: { agentId: 'shared-agent', title: 'Verify release' } })
  })

  it('keeps existing task controls pinned to their owner after changing the agent execution node', async () => {
    const agent = { id: 'shared-agent', name: 'Planner', originNodeId: 'primary', ownerNodeId: 'primary', revision: 'agent-r4' }
    const existingTask = { id: 'task-on-primary', title: 'Existing task', status: 'running', ownerNodeId: 'primary', ownerEpoch: 'primary-epoch-2' }
    const snapshot = baseSnapshot({ agents: [agent] })
    const { invoke, user } = renderConnected(snapshot, request => request.method === 'agents.read' ? agent : request.method === 'tasks.list' ? { data: [existingTask] } : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.selectOptions(await screen.findByLabelText('Actions for Existing task'), 'pause')
    await waitFor(() => expect(sent(invoke, 'tasks.control')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'tasks.control')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'primary', expectedEpoch: 'primary-epoch-2', params: { taskId: 'task-on-primary', action: 'pause' } })
    expect((call?.[1] as { request: Record<string, unknown> }).request).not.toHaveProperty('expectedRevision')
  })

  it('renames and archives a conversation through its saved owner', async () => {
    const thread = { id: 'thread-controls', title: 'Rename me', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', turns: [] }
    const snapshot = baseSnapshot({ threads: [thread] })
    let latest = snapshot
    const { invoke, user, emit } = renderConnected(snapshot, request => request.channel === 'sessions' && request.method === 'read' ? thread : {}, {}, () => latest)
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Rename me/ }))
    await user.click(screen.getByRole('button', { name: 'More conversation actions' }))
    await user.click(screen.getByRole('button', { name: 'Rename conversation' }))
    const name = screen.getByRole('textbox', { name: 'Conversation name' })
    await user.clear(name)
    await user.type(name, 'Release review')
    await user.click(screen.getByRole('button', { name: 'Save name' }))
    await waitFor(() => expect(sent(invoke, 'rename')).toBe(true))
    const rename = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'rename')
    expect((rename?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { threadId: 'thread-controls', name: 'Release review' } })
    expect(await screen.findByRole('heading', { name: 'Release review' })).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'More conversation actions' }))
    await user.click(screen.getByRole('button', { name: 'Archive conversation' }))
    await waitFor(() => expect(sent(invoke, 'archive')).toBe(true))
    const archive = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'archive')
    expect((archive?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { threadId: 'thread-controls' } })
    expect(await screen.findByText('Conversation archived.')).toBeInTheDocument()

    latest = { ...snapshot, threads: [], cursor: { epoch: snapshot.epoch, sequence: 1 } }
    emit('remote-event', {
      epoch: snapshot.epoch, sequence: 1, nodeId: 'worker', ownerEpoch: thread.ownerEpoch,
      message: { method: 'thread/archived', params: { threadId: thread.id } },
    })
    await waitFor(() => expect(screen.queryByRole('button', { name: /Rename me/ })).not.toBeInTheDocument())
    expect(screen.getByText('Conversation archived.')).toBeInTheDocument()
  })

  it('requires explicit confirmation before deleting an owner-scoped conversation', async () => {
    const thread = { id: 'thread-delete', title: 'Delete me', ownerNodeId: 'worker', turns: [] }
    const snapshot = baseSnapshot({ threads: [thread] })
    const { invoke, user } = renderConnected(snapshot, request => request.channel === 'sessions' && request.method === 'read' ? thread : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Delete me/ }))
    await user.click(screen.getByRole('button', { name: 'More conversation actions' }))
    await user.click(screen.getByRole('button', { name: 'Delete conversation' }))
    expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'delete')).toBe(false)
    await user.click(screen.getByRole('button', { name: 'Delete permanently' }))
    await waitFor(() => expect(sent(invoke, 'delete')).toBe(true))
    const deletion = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'delete')
    expect((deletion?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { threadId: 'thread-delete' } })
  })

  it('loads task pages by owner and deduplicates overlapping task records', async () => {
    const agent = { id: 'agent-pages', name: 'Planner', ownerNodeId: 'worker' }
    const first = { id: 'task-1', agentId: 'agent-pages', title: 'First page task', status: 'queued' }
    const second = { id: 'task-2', agentId: 'agent-pages', title: 'Second page task', status: 'queued' }
    const snapshot = baseSnapshot({ agents: [agent] })
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.method === 'agents.read') return agent
      if (request.method === 'tasks.list' && !(request.params as Record<string, unknown>).after) return { data: [first], nextCursor: 'task-1' }
      if (request.method === 'tasks.list' && (request.params as Record<string, unknown>).after === 'task-1') return { data: [first, second], nextCursor: null }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(within(screen.getByRole('tablist', { name: 'Agent sections' })).getByRole('tab', { name: 'Tasks & schedules' }))
    expect(await screen.findByText('First page task')).toBeInTheDocument()
    await user.click(await screen.findByRole('button', { name: 'Load more tasks' }))
    expect(await screen.findByText('Second page task')).toBeInTheDocument()
    expect(screen.getAllByText('First page task')).toHaveLength(1)
    const pageRequest = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'tasks.list' && (args as { request?: { params?: { after?: string } } })?.request?.params?.after === 'task-1')
    expect((pageRequest?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { agentId: 'agent-pages', after: 'task-1', limit: 100 } })
  })

  it('stops task pagination when the node repeats a cursor', async () => {
    const agent = { id: 'agent-repeat', name: 'Planner', ownerNodeId: 'worker' }
    const task = { id: 'task-cursor', title: 'Cursor task', status: 'queued' }
    const snapshot = baseSnapshot({ agents: [agent] })
    const { user } = renderConnected(snapshot, request => {
      if (request.method === 'agents.read') return agent
      if (request.method === 'tasks.list') return { data: [task], nextCursor: 'task-cursor' }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(within(screen.getByRole('tablist', { name: 'Agent sections' })).getByRole('tab', { name: 'Tasks & schedules' }))
    await user.click(await screen.findByRole('button', { name: 'Load more tasks' }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/repeated a task cursor/i)
    expect(screen.queryByRole('button', { name: 'Load more tasks' })).not.toBeInTheDocument()
  })

  it('hydrates real thread turns and streams owner-scoped text before steer and stop', async () => {
    const thread = { id: 'thread-1', title: 'Hydrated thread', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', turns: [{ id: 'turn-1', status: 'inProgress', items: [{ id: 'message-1', type: 'agentMessage', text: 'Earlier response' }] }] }
    const snapshot = baseSnapshot({ threads: [thread] })
    const { invoke, user, emit, listen } = renderConnected(snapshot, request => request.method === 'read' ? thread : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Hydrated thread/ }))
    expect(await screen.findByText('Earlier response')).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'Stop run' })).toBeInTheDocument()
    await waitFor(() => expect(listen).toHaveBeenCalled())
    emit('remote-event', { nodeId: 'worker', ownerEpoch: 'worker-epoch-8', epoch: 'primary-epoch-2', sequence: 1, message: { method: 'item/agentMessage/delta', params: { threadId: 'thread-1', turnId: 'turn-1', itemId: 'message-1', delta: ' with live output' } } })
    expect(await screen.findByText('Earlier response with live output')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Steer this turn' }))
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Check the failing assertion')
    await user.click(screen.getByRole('button', { name: 'Send message' }))
    await waitFor(() => expect(sent(invoke, 'steer')).toBe(true))
    const steer = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'steer')
    expect((steer?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedEpoch: 'worker-epoch-8', params: { threadId: 'thread-1', expectedTurnId: 'turn-1', text: 'Check the failing assertion' } })
    await user.click(await screen.findByRole('button', { name: 'Stop run' }))
    await waitFor(() => expect(sent(invoke, 'stop')).toBe(true))
    const stop = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'stop')
    expect((stop?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedEpoch: 'worker-epoch-8', params: { threadId: 'thread-1', turnId: 'turn-1' } })
  })

  it('renders runtime userMessage content from sessions.read and keeps it once across events and reopen', async () => {
    const thread = { id: 'wire-thread', title: 'Wire transcript', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', historyMode: 'full' }
    const readResult = {
      ...thread,
      sessionId: 'wire-thread',
      status: { type: 'idle' },
      turns: [{ id: 'wire-turn', status: 'completed', items: [
        { type: 'userMessage', id: 'wire-user', clientId: 'wire-client', content: [{ type: 'text', text: 'Please review the focused transcript behavior.', text_elements: [] }, { type: 'image', url: 'private-image-payload' }], startedAtMs: 123, completedAtMs: 124 },
        { type: 'agentMessage', id: 'wire-agent', text: 'I will inspect the transcript rendering.', phase: 'final_answer', startedAtMs: 125, completedAtMs: 126 },
      ] }],
    }
    const snapshot = baseSnapshot({ threads: [thread] })
    const { user, emit } = renderConnected(snapshot, request => request.channel === 'sessions' && request.method === 'read' ? readResult : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Wire transcript.* · Worker/ }))
    const transcript = screen.getByLabelText('Conversation transcript')
    expect(within(transcript).getByText('Please review the focused transcript behavior.')).toBeInTheDocument()
    expect(within(transcript).getByText('I will inspect the transcript rendering.')).toBeInTheDocument()
    expect(within(transcript).getAllByText('Please review the focused transcript behavior.')).toHaveLength(1)
    expect(within(transcript).queryByText('private-image-payload')).not.toBeInTheDocument()
    expect(Array.from(transcript.querySelectorAll('.transcript-item')).map(item => item.querySelector('.transcript-speaker')?.textContent)).toEqual(['You', 'Agent'])

    emit('remote-event', { nodeId: 'worker', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', epoch: 'primary-epoch-2', sequence: 1, message: { method: 'item/agentMessage/delta', params: { threadId: 'wire-thread', turnId: 'wire-turn', itemId: 'wire-agent', delta: ' Checked.' } } })
    expect(await within(transcript).findByText('I will inspect the transcript rendering. Checked.')).toBeInTheDocument()
    expect(within(transcript).getAllByText('Please review the focused transcript behavior.')).toHaveLength(1)

    await user.click(screen.getByRole('button', { name: 'Back to chats' }))
    await user.click(await screen.findByRole('button', { name: /Wire transcript/ }))
    const reopenedTranscript = screen.getByLabelText('Conversation transcript')
    expect(await within(reopenedTranscript).findByText('Please review the focused transcript behavior.')).toBeInTheDocument()
    expect(within(reopenedTranscript).getAllByText('Please review the focused transcript behavior.')).toHaveLength(1)
  })

  it('pages older paginated thread history on the owning node in chronological order', async () => {
    const thread = { id: 'paged-thread', title: 'Paged conversation', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', historyMode: 'paginated', turns: [] }
    const snapshot = baseSnapshot({ threads: [thread] })
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.channel === 'sessions' && request.method === 'read') return thread
      if (request.channel === 'sessions' && request.method === 'turns' && !(request.params as Record<string, unknown>).cursor) return { data: [{ id: 'turn-new', status: 'completed', items: [{ id: 'new-item', type: 'agentMessage', text: 'Recent response' }] }], nextCursor: 'older-page' }
      if (request.channel === 'sessions' && request.method === 'turns' && (request.params as Record<string, unknown>).cursor === 'older-page') return { data: [{ id: 'turn-old', status: 'completed', items: [{ id: 'old-item', type: 'agentMessage', text: 'Earlier response' }] }], nextCursor: null }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Paged conversation/ }))
    expect(await screen.findByText('Recent response')).toBeInTheDocument()
    const firstPage = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'sessions' && (args as { request?: { method?: string } })?.request?.method === 'turns')
    expect((firstPage?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { threadId: 'paged-thread' } })
    await user.click(screen.getByRole('button', { name: 'Load older history' }))
    expect(await screen.findByText('Earlier response')).toBeInTheDocument()
    const transcript = screen.getByLabelText('Conversation transcript')
    const messages = Array.from(transcript.querySelectorAll('.transcript-copy p:last-child')).map(node => node.textContent)
    expect(messages).toEqual(['Earlier response', 'Recent response'])
  })

  it('stops paginated thread history when the runtime repeats a cursor', async () => {
    const thread = { id: 'repeat-thread', title: 'Repeat cursor', ownerNodeId: 'worker', historyMode: 'paginated', turns: [] }
    const snapshot = baseSnapshot({ threads: [thread] })
    const { user } = renderConnected(snapshot, request => {
      if (request.channel === 'sessions' && request.method === 'read') return thread
      if (request.channel === 'sessions' && request.method === 'turns') return { data: [], nextCursor: 'same-cursor' }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Repeat cursor/ }))
    await user.click(await screen.findByRole('button', { name: 'Load older history' }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/repeated a thread history cursor/i)
    expect(screen.queryByRole('button', { name: 'Load older history' })).not.toBeInTheDocument()
  })

  it('reduces ordered owner events from a replay into the visible conversation', async () => {
    const thread = { id: 'thread-replay', title: 'Replay thread', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', turns: [{ id: 'turn-replay', status: 'inProgress', items: [{ id: 'message-replay', type: 'agentMessage', text: 'Starting point' }] }] }
    const snapshot = baseSnapshot({ threads: [thread] })
    const { user, emit } = renderConnected(snapshot, request => request.method === 'read' ? thread : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Replay thread/ }))
    expect(await screen.findByText('Starting point')).toBeInTheDocument()
    emit('remote-replay', { epoch: 'primary-epoch-2', latest: 4, requiresSnapshot: false, events: [
      { nodeId: 'worker', ownerEpoch: 'worker-epoch-8', epoch: 'primary-epoch-2', sequence: 3, message: { method: 'item/agentMessage/delta', params: { threadId: 'thread-replay', turnId: 'turn-replay', itemId: 'message-replay', delta: ' · first' } } },
      { nodeId: 'worker', ownerEpoch: 'worker-epoch-8', epoch: 'primary-epoch-2', sequence: 4, message: { method: 'item/agentMessage/delta', params: { threadId: 'thread-replay', turnId: 'turn-replay', itemId: 'message-replay', delta: ' · second' } } },
    ] })
    expect(await screen.findByText('Starting point · first · second')).toBeInTheDocument()
  })

  it('coalesces semantic snapshot refreshes and skips one refresh per streamed token', async () => {
    const { invoke, emit } = renderConnected(baseSnapshot())
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(invoke.mock.calls.some(([command]) => command === 'remote_snapshot')).toBe(true))
    const snapshotCalls = () => invoke.mock.calls.filter(([command]) => command === 'remote_snapshot').length
    const before = snapshotCalls()
    for (const delta of ['a', 'b', 'c', 'd']) emit('remote-event', { nodeId: 'primary', epoch: 'primary-epoch-2', sequence: before + 1, message: { method: 'item/agentMessage/delta', params: { threadId: 't', itemId: 'm', turnId: 'turn', delta } } })
    await new Promise(resolve => setTimeout(resolve, 180))
    expect(snapshotCalls()).toBe(before)
    emit('remote-event', { nodeId: 'primary', epoch: 'primary-epoch-2', sequence: before + 2, message: { method: 'turn/completed', params: { threadId: 't', turn: { id: 'turn', status: 'completed' } } } })
    emit('remote-event', { nodeId: 'primary', epoch: 'primary-epoch-2', sequence: before + 3, message: { method: 'brr/questions/changed', params: {} } })
    await waitFor(() => expect(snapshotCalls()).toBe(before + 1))
  })

  it('does not let a pre-archive snapshot overwrite the acknowledged archive result', async () => {
    const thread = { id: 'archive-race', title: 'Archive race', ownerNodeId: 'primary', turns: [] }
    const initial = baseSnapshot({ threads: [thread] })
    const archived = baseSnapshot({ threads: [], cursor: { epoch: 'primary-epoch-2', sequence: 8 } })
    const snapshots: Array<(value: Snapshot) => void> = []
    let call = 0
    const { invoke, user, emit } = renderConnected(initial, request => request.method === 'archive' ? {} : {}, { 'snapshot:phone': initial }, () => {
      call += 1
      if (call === 1) return initial
      return new Promise<Snapshot>(resolve => snapshots.push(resolve)) as unknown as Snapshot
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(call).toBe(1))

    emit('remote-event', { nodeId: 'primary', epoch: 'primary-epoch-2', sequence: 7, message: { method: 'thread/updated', params: { threadId: thread.id } } })
    await waitFor(() => expect(snapshots).toHaveLength(1))
    await user.click(await screen.findByRole('button', { name: /Archive race/ }))
    await user.click(screen.getByRole('button', { name: 'More conversation actions' }))
    await user.click(screen.getByRole('button', { name: 'Archive conversation' }))
    await waitFor(() => expect(snapshots).toHaveLength(2))

    snapshots[1](archived)
    expect(await screen.findByText('Conversation archived.')).toBeInTheDocument()
    snapshots[0](initial)
    await waitFor(() => expect(screen.queryByRole('button', { name: /Archive race/ })).not.toBeInTheDocument())
    const cachedSnapshots = invoke.mock.calls.filter(([command, args]) => command === 'remote_cache_put' && (args as { key?: string })?.key === 'snapshot:phone')
    expect((cachedSnapshots.at(-1)?.[1] as { value: Snapshot }).value.threads).toEqual([])
  })

  it('uses the refreshed cursor when toolbar refresh reconnects the event stream', async () => {
    const initial = baseSnapshot({ cursor: { epoch: 'primary-epoch-2', sequence: 3 } })
    const refreshed = baseSnapshot({ cursor: { epoch: 'primary-epoch-2', sequence: 21 } })
    let snapshots = 0
    const { invoke, user, emit } = renderConnected(initial, () => ({}), {}, () => ++snapshots === 1 ? initial : refreshed)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(invoke.mock.calls.some(([command]) => command === 'remote_subscribe')).toBe(true))
    const subscriptionsBefore = invoke.mock.calls.filter(([command]) => command === 'remote_subscribe').length

    emit('remote-disconnect', { profileId: profile.id, message: 'Stream ended' })
    expect(await screen.findByText('Stream ended')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Refresh snapshot' }))

    await waitFor(() => {
      const subscriptions = invoke.mock.calls.filter(([command]) => command === 'remote_subscribe')
      expect(subscriptions).toHaveLength(subscriptionsBefore + 1)
      expect(subscriptions.at(-1)?.[1]).toMatchObject({ profileId: profile.id, cursor: refreshed.cursor })
    })
    expect(screen.getByText('Connected')).toBeInTheDocument()
  })

  it('re-hydrates the active thread and owner epoch after a replay gap', async () => {
    const oldThread = { id: 'thread-gap', title: 'Gap thread', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8', turns: [{ id: 'turn-old', status: 'inProgress', items: [{ id: 'message-old', type: 'agentMessage', text: 'Before reconnect' }] }] }
    const initial = baseSnapshot({ threads: [oldThread] })
    let currentSnapshot = initial
    const latestThread = { id: 'thread-gap', title: 'Gap thread', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-9', turns: [{ id: 'turn-new', status: 'inProgress', items: [{ id: 'message-new', type: 'agentMessage', text: 'Fresh after reconnect' }] }] }
    let currentThread = oldThread
    const { invoke, user, emit } = renderConnected(initial, request => request.method === 'read' ? currentThread : {}, {}, () => currentSnapshot)
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(await screen.findByRole('button', { name: /Gap thread/ }))
    expect(await screen.findByText('Before reconnect')).toBeInTheDocument()
    currentThread = latestThread
    currentSnapshot = baseSnapshot({ nodes: [{ nodeId: 'primary', name: 'Primary', primary: true, online: true, epoch: 'primary-epoch-2' }, { ...worker, epoch: 'worker-epoch-9' }], threads: [latestThread] })
    emit('remote-event', { nodeId: 'worker', ownerEpoch: 'worker-epoch-9', epoch: 'primary-epoch-2', sequence: 11, message: { method: 'item/agentMessage/delta', params: { threadId: 'thread-gap', turnId: 'turn-new', itemId: 'message-new', delta: 'ignored until refreshed' } } })
    expect(await screen.findByText('Fresh after reconnect')).toBeInTheDocument()
    expect(screen.queryByText('Fresh after reconnectignored until refreshed')).not.toBeInTheDocument()
    emit('remote-replay', { epoch: 'primary-epoch-2', latest: 12, requiresSnapshot: true, events: [] })
    expect(await screen.findByText('Fresh after reconnect')).toBeInTheDocument()
    await user.click(await screen.findByRole('button', { name: 'Stop run' }))
    await waitFor(() => expect(sent(invoke, 'stop')).toBe(true))
    const stop = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'stop')
    expect((stop?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedEpoch: 'worker-epoch-9', params: { threadId: 'thread-gap', turnId: 'turn-new' } })
  })

  it('approves a task proposal and creates then pauses a schedule with the owning agent', async () => {
    const agent = { id: 'agent-1', name: 'Planner', ownerNodeId: 'worker', revision: 'agent-rev-3' }
    const proposal = { id: 'proposal-1', parentTaskId: 'parent-1', hash: 'proposal-hash', status: 'proposed', tasks: [{ key: 'verify', task: { title: 'Check deployments', instructions: 'Inspect each node', acceptanceCriteria: ['No node is missed'] }, dependsOn: [] }], createdAt: 1_700_000_000, ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8' }
    const workflow = { id: 'workflow-1', name: 'Weekly check', expression: '0 9 * * 1', timezone: 'Australia/Brisbane', paused: false, revision: 'flow-r1', ownerNodeId: 'worker', ownerEpoch: 'worker-epoch-8' }
    const snapshot = baseSnapshot({ agents: [agent] })
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.method === 'agents.read') return agent
      if (request.method === 'tasks.proposals') return { data: [proposal] }
      if (request.method === 'workflows.list') return { data: [workflow] }
      if (request.method === 'tasks.list') return { data: [] }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    expect(await screen.findByText('proposal-hash')).toBeInTheDocument()
    expect(screen.getByText('Inspect each node')).toBeInTheDocument()
    await user.click(await screen.findByRole('button', { name: 'Approve proposal' }))
    await waitFor(() => expect(sent(invoke, 'tasks.approve')).toBe(true))
    const approval = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'tasks.approve')
    expect((approval?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedEpoch: 'worker-epoch-8', params: { proposalId: 'proposal-1', hash: 'proposal-hash' } })

    await user.type(screen.getByLabelText('Schedule name'), 'Weekly check')
    await user.type(screen.getByLabelText('Task title'), 'Check deployments')
    await user.type(screen.getAllByLabelText('Instructions')[1], 'Review service health and report failures')
    await user.type(screen.getByLabelText(/Schedule acceptance criteria/), 'Report each failed service')
    await user.click(screen.getByRole('button', { name: 'Save schedule' }))
    await waitFor(() => expect(sent(invoke, 'workflows.create')).toBe(true))
    const schedule = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'workflows.create')
    expect((schedule?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { agentId: 'agent-1', name: 'Weekly check', title: 'Check deployments', acceptanceCriteria: ['Report each failed service'], expression: '0 9 * * 1' } })

    await user.click(await screen.findByRole('button', { name: 'Pause' }))
    await waitFor(() => expect(sent(invoke, 'workflows.control')).toBe(true))
    const control = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'workflows.control')
    expect((control?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { workflowId: 'workflow-1', paused: true } })
  })

  it('confirms memory forgetting, supports undo, and revokes a learned skill', async () => {
    const agent = { id: 'agent-1', name: 'Researcher', ownerNodeId: 'worker', revision: 'agent-rev-1' }
    const memory = { id: 'memory-1', scope: 'agent', scopeId: 'agent-1', kind: 'fact', content: 'Use the staging tenant', revision: 4, sourceIds: ['source-1'] }
    const memorySource = { sourceId: 'source-1', revision: 2, threadId: 'thread-source', agentId: 'agent-1', projectId: null, content: 'Verified source excerpt', active: true }
    const skill = { id: 'skill-1', name: 'Web research', description: 'Check sources', revision: 'skill-r2' }
    let forgotten = false
    const snapshot = baseSnapshot({ agents: [agent] })
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.method === 'agents.read') return agent
      if (request.method === 'memory.list') return { data: forgotten ? [] : [memory], sources: [memorySource] }
      if (request.method === 'memory.forget') { forgotten = true; return { eventId: 123 } }
      if (request.method === 'memory.undo') { forgotten = false; return {} }
      if (request.method === 'skills.list') return { data: [skill] }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Researcher/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Memory' }))
    expect(await screen.findByText('Use the staging tenant')).toBeInTheDocument()
    expect(await screen.findByText('Source · thread-source')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Forget memory' }))
    await user.click(screen.getByRole('button', { name: 'Confirm forget' }))
    await waitFor(() => expect(sent(invoke, 'memory.forget')).toBe(true))
    await user.click(await screen.findByRole('button', { name: 'Undo last forget' }))
    await waitFor(() => expect(sent(invoke, 'memory.undo')).toBe(true))
    const undo = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'memory.undo')
    expect((undo?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', params: { eventId: 123, agentId: 'agent-1' } })
    await user.click(await screen.findByRole('tab', { name: 'Skills' }))
    await user.click(await screen.findByRole('button', { name: 'Revoke Web research' }))
    await waitFor(() => expect(sent(invoke, 'skills.revoke')).toBe(true))
    const revoke = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } })?.request?.method === 'skills.revoke')
    expect((revoke?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ targetNodeId: 'worker', expectedRevision: 'skill-r2', params: { id: 'skill-1', agentId: 'agent-1' } })
  })

  it('clears memory undo when changing its owning management node', async () => {
    const agent = { id: 'shared-agent', name: 'Planner', ownerNodeId: 'primary', availableNodeIds: ['primary', 'worker'], revision: 'agent-r4' }
    const memory = { id: 'memory-1', kind: 'fact', content: 'Keep the release scoped', revision: 5 }
    const snapshot = baseSnapshot({ agents: [agent] })
    const { user } = renderConnected(snapshot, request => request.method === 'agents.read' ? agent : request.method === 'memory.list' ? { data: [memory] } : request.method === 'memory.forget' ? { eventId: 456 } : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.click(await screen.findByRole('tab', { name: 'Memory' }))
    await user.click(screen.getByRole('button', { name: 'Forget memory' }))
    await user.click(screen.getByRole('button', { name: 'Confirm forget' }))
    expect(await screen.findByRole('button', { name: 'Undo last forget' })).toBeInTheDocument()
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Undo last forget' })).not.toBeInTheDocument())
  })

  it('keeps an offline task draft local until explicit review and publish', async () => {
    const agent = { id: 'agent-1', name: 'Planner', ownerNodeId: 'worker', revision: 'agent-rev-3' }
    const snapshot = baseSnapshot({ agents: [agent] })
    let snapshotCalls = 0
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      if (command === 'remote_profiles') return [profile] as T
      if (command === 'remote_snapshot') {
        snapshotCalls += 1
        if (snapshotCalls === 1) throw new Error('offline for test')
        return snapshot as T
      }
      if (command === 'remote_cache_get') return String(args?.key ?? '').startsWith('snapshot:') ? snapshot as T : null as T
      if (command === 'remote_cache_put' || command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
      if (command === 'remote_request') return { id: (args?.request as { id?: string })?.id, result: agent } as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(screen.getByText('Offline')).toBeInTheDocument())
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.type(screen.getByLabelText('Title'), 'Prepare a release note')
    await user.type(screen.getAllByLabelText('Instructions')[0], 'Summarize verified changes')
    await user.type(screen.getByLabelText(/Acceptance criteria/), 'Every claim links to evidence')
    await user.click(screen.getByRole('button', { name: 'Save task draft' }))
    expect(await screen.findByText(/Task definition saved on this device/)).toBeInTheDocument()
    expect(invoke).not.toHaveBeenCalledWith('remote_request', expect.objectContaining({ request: expect.objectContaining({ method: 'tasks.create' }) }))
    await user.click(screen.getByRole('button', { name: 'Refresh snapshot' }))
    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument())
    await user.click(await screen.findByRole('button', { name: 'Review & create' }))
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('remote_request', expect.objectContaining({ request: expect.objectContaining({ method: 'tasks.create', operationId: expect.any(String), expectedRevision: 'agent-rev-3', targetNodeId: 'worker' }) })))
  })

  it('keeps an offline schedule form when native draft persistence fails', async () => {
    const agent = { id: 'agent-1', name: 'Planner', ownerNodeId: 'worker', revision: 'agent-rev-3' }
    const snapshot = baseSnapshot({ agents: [agent] })
    let snapshotCalls = 0
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      if (command === 'remote_profiles') return [profile] as T
      if (command === 'remote_snapshot') {
        snapshotCalls += 1
        if (snapshotCalls === 1) throw new Error('offline for test')
        return snapshot as T
      }
      if (command === 'remote_cache_get') return String(args?.key ?? '').startsWith('snapshot:') ? snapshot as T : null as T
      if (command === 'remote_cache_put' && String(args?.key ?? '').startsWith('pending:definitions:')) throw new Error('Native cache unavailable')
      if (command === 'remote_cache_put' || command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
      if (command === 'remote_request') return { id: (args?.request as { id?: string })?.id, result: agent } as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(screen.getByText('Offline')).toBeInTheDocument())
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.type(screen.getByLabelText('Schedule name'), 'Weekly deployment review')
    await user.type(screen.getByLabelText('Task title'), 'Review deployments')
    await user.type(screen.getAllByLabelText('Instructions')[1], 'Check every service')
    await user.type(screen.getByLabelText(/Schedule acceptance criteria/), 'Report failed services')
    await user.click(screen.getByRole('button', { name: 'Save schedule draft' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('Native cache unavailable')
    expect(screen.getByLabelText('Schedule name')).toHaveValue('Weekly deployment review')
    expect(screen.getByLabelText('Task title')).toHaveValue('Review deployments')
    expect(screen.queryByText('Weekly deployment review · workflow · Review before creating')).not.toBeInTheDocument()
    expect(sent(invoke, 'workflows.create')).toBe(false)
  })

  it('reuses one offline definition ID when native cache commits but rejects its reply', async () => {
    const agent = { id: 'agent-1', name: 'Planner', ownerNodeId: 'worker', revision: 'agent-rev-3' }
    const snapshot = baseSnapshot({ agents: [agent] })
    const cache = new Map<string, unknown>()
    let snapshotCalls = 0
    let rejectAfterCommit = true
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      const key = String(args?.key ?? '')
      if (command === 'remote_profiles') return [profile] as T
      if (command === 'remote_snapshot') {
        snapshotCalls += 1
        if (snapshotCalls === 1) throw new Error('offline for test')
        return snapshot as T
      }
      if (command === 'remote_cache_get') return (key.startsWith('snapshot:') ? snapshot : cache.get(key) ?? null) as T
      if (command === 'remote_cache_put') {
        cache.set(key, args?.value)
        if (key.startsWith('pending:definitions:') && rejectAfterCommit) {
          rejectAfterCommit = false
          throw new Error('Cache reply was lost')
        }
        return undefined as T
      }
      if (command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
      if (command === 'remote_request') return { id: (args?.request as { id?: string })?.id, result: agent } as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(screen.getByText('Offline')).toBeInTheDocument())
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.type(screen.getByLabelText('Schedule name'), 'Weekly review')
    await user.type(screen.getByLabelText('Task title'), 'Check services')
    await user.type(screen.getAllByLabelText('Instructions')[1], 'Inspect each service')
    await user.type(screen.getByLabelText(/Schedule acceptance criteria/), 'List failures')

    await user.click(screen.getByRole('button', { name: 'Save schedule draft' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Cache reply was lost')
    const key = 'pending:definitions:phone'
    const first = cache.get(key) as Array<{ id: string; operationId: string }>
    expect(first).toHaveLength(1)
    const firstId = first[0].id
    await user.click(screen.getByRole('button', { name: 'Save schedule draft' }))
    expect(await screen.findByText(/Schedule definition saved on this device/)).toBeInTheDocument()
    const retried = cache.get(key) as Array<{ id: string; operationId: string }>
    expect(retried).toHaveLength(1)
    expect(retried[0]).toMatchObject({ id: firstId, operationId: firstId })
    await user.clear(screen.getByLabelText('Schedule name'))
    await user.type(screen.getByLabelText('Schedule name'), 'Monthly review')
    await user.click(screen.getByRole('button', { name: 'Save schedule draft' }))
    expect(await screen.findByText(/Schedule definition saved on this device/)).toBeInTheDocument()
    const changedPayload = cache.get(key) as Array<{ id: string; operationId: string; params: { name: string } }>
    expect(changedPayload).toHaveLength(2)
    expect(changedPayload[1].params.name).toBe('Monthly review')
    expect(changedPayload[1].operationId).not.toBe(firstId)
  })

  it('serializes offline definition writes so concurrent form saves do not replace each other', async () => {
    const agent = { id: 'agent-1', name: 'Planner', ownerNodeId: 'worker', revision: 'agent-rev-3' }
    const snapshot = baseSnapshot({ agents: [agent] })
    const cache = new Map<string, unknown>()
    let snapshotCalls = 0
    let definitionWrites = 0
    let releaseFirstWrite: (() => void) | undefined
    const firstWritePending = new Promise<void>(resolve => { releaseFirstWrite = resolve })
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      const key = String(args?.key ?? '')
      if (command === 'remote_profiles') return [profile] as T
      if (command === 'remote_snapshot') {
        snapshotCalls += 1
        if (snapshotCalls === 1) throw new Error('offline for test')
        return snapshot as T
      }
      if (command === 'remote_cache_get') return (key.startsWith('snapshot:') ? snapshot : cache.get(key) ?? null) as T
      if (command === 'remote_cache_put') {
        if (key.startsWith('pending:definitions:')) {
          definitionWrites += 1
          if (definitionWrites === 1) await firstWritePending
        }
        cache.set(key, args?.value)
        return undefined as T
      }
      if (command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
      if (command === 'remote_request') return { id: (args?.request as { id?: string })?.id, result: agent } as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(screen.getByText('Offline')).toBeInTheDocument())
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.type(screen.getByLabelText('Schedule name'), 'Weekly review')
    await user.type(screen.getByLabelText('Task title'), 'Check services')
    await user.type(screen.getAllByLabelText('Instructions')[1], 'Inspect every service')
    await user.type(screen.getByLabelText(/Schedule acceptance criteria/), 'List failures')
    await user.type(screen.getByLabelText('Title'), 'Write release note')
    await user.type(screen.getAllByLabelText('Instructions')[0], 'Summarize changes')
    await user.type(screen.getByLabelText(/^Acceptance criteria/), 'Link evidence')

    await user.click(screen.getByRole('button', { name: 'Save schedule draft' }))
    await waitFor(() => expect(definitionWrites).toBe(1))
    await user.click(screen.getByRole('button', { name: 'Save task draft' }))
    await waitFor(() => expect(definitionWrites).toBe(1))
    await user.type(screen.getByLabelText('Title'), ' for next run')
    releaseFirstWrite?.()

    await waitFor(() => expect(definitionWrites).toBe(2))
    const saved = cache.get('pending:definitions:phone') as Array<{ kind: string }>
    expect(saved.map(draft => draft.kind).sort()).toEqual(['task', 'workflow'])
    expect(screen.getByLabelText('Title')).toHaveValue('Write release note for next run')
    expect(await screen.findByText(/Task definition saved on this device/)).toBeInTheDocument()
  })

  it('removes a published definition from the latest list when another offline save races it', async () => {
    const agent = { id: 'agent-1', name: 'Planner', ownerNodeId: 'worker', revision: 'agent-rev-3' }
    const initial = baseSnapshot({ agents: [agent] })
    let snapshotCalls = 0
    let publishStarted = false
    let releasePublish: (() => void) | undefined
    const publishPending = new Promise<void>(resolve => { releasePublish = resolve })
    let taskWriteStarted = false
    let releaseTaskWrite: (() => void) | undefined
    const taskWritePending = new Promise<void>(resolve => { releaseTaskWrite = resolve })
    const cache = new Map<string, unknown>()
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      const key = String(args?.key ?? '')
      if (command === 'remote_profiles') return [profile] as T
      if (command === 'remote_snapshot') {
        snapshotCalls += 1
        if (snapshotCalls === 1) throw new Error('offline for test')
        return initial as T
      }
      if (command === 'remote_cache_get') return (key.startsWith('snapshot:') ? initial : cache.get(key) ?? null) as T
      if (command === 'remote_cache_put') {
        if (key.startsWith('pending:definitions:') && Array.isArray(args?.value) && (args.value as Array<{ kind?: string }>).some(draft => draft.kind === 'task')) {
          taskWriteStarted = true
          await taskWritePending
        }
        cache.set(key, args?.value)
        return undefined as T
      }
      if (command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
      if (command === 'remote_request') {
        const request = args?.request as { id?: string; method?: string }
        if (request.method === 'workflows.create') {
          publishStarted = true
          await publishPending
        }
        return { id: request.id, result: agent } as T
      }
      return undefined as T
    })
    const listeners = new Map<string, (payload: unknown) => void>()
    const client = new RemoteClient({ invoke, listen: vi.fn(async (event: string, handler: (payload: unknown) => void) => { listeners.set(event, handler); return () => listeners.delete(event) }) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(screen.getByText('Offline')).toBeInTheDocument())
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.type(screen.getByLabelText('Schedule name'), 'Publish this')
    await user.type(screen.getByLabelText('Task title'), 'First task')
    await user.type(screen.getAllByLabelText('Instructions')[1], 'Inspect services')
    await user.type(screen.getByLabelText(/Schedule acceptance criteria/), 'Report failures')
    await user.click(screen.getByRole('button', { name: 'Save schedule draft' }))
    expect(await screen.findByText(/Schedule definition saved/)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Refresh snapshot' }))
    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument())
    await user.click(screen.getByRole('button', { name: 'Review & create' }))
    await waitFor(() => expect(publishStarted).toBe(true))
    await waitFor(() => expect(listeners.has('remote-disconnect')).toBe(true))
    listeners.get('remote-disconnect')?.({ profileId: 'phone', message: 'Offline during publish' })
    await waitFor(() => expect(screen.getByText('Offline')).toBeInTheDocument())

    await user.clear(screen.getByLabelText('Title'))
    await user.type(screen.getByLabelText('Title'), 'Concurrent task')
    await user.clear(screen.getAllByLabelText('Instructions')[0])
    await user.type(screen.getAllByLabelText('Instructions')[0], 'Inspect every node')
    await user.clear(screen.getByLabelText(/^Acceptance criteria/))
    await user.type(screen.getByLabelText(/^Acceptance criteria/), 'Include evidence')
    await user.click(screen.getByRole('button', { name: 'Save task draft' }))
    await waitFor(() => expect(taskWriteStarted).toBe(true))
    releasePublish?.()
    await new Promise(resolve => setTimeout(resolve, 0))
    releaseTaskWrite?.()

    expect(await screen.findByText(/Saved definition created on its owning node/)).toBeInTheDocument()
    const saved = cache.get('pending:definitions:phone') as Array<{ kind: string; params: { title?: string } }>
    expect(saved).toHaveLength(1)
    expect(saved[0]).toMatchObject({ kind: 'task', params: { title: 'Concurrent task' } })
  })

  it('waits for definition writes before forgetting and clears only that profile definitions key', async () => {
    const agent = { id: 'agent-1', name: 'Planner', ownerNodeId: 'worker', revision: 'agent-rev-3' }
    const snapshot = baseSnapshot({ agents: [agent] })
    let snapshotCalls = 0
    let definitionWrites = 0
    let releaseDefinitionWrite: (() => void) | undefined
    const definitionWritePending = new Promise<void>(resolve => { releaseDefinitionWrite = resolve })
    const cache = new Map<string, unknown>()
    const order: string[] = []
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      const key = String(args?.key ?? '')
      if (command === 'remote_profiles') return [profile] as T
      if (command === 'remote_snapshot') {
        snapshotCalls += 1
        if (snapshotCalls === 1) throw new Error('offline for test')
        return snapshot as T
      }
      if (command === 'remote_cache_get') return (key.startsWith('snapshot:') ? snapshot : cache.get(key) ?? null) as T
      if (command === 'remote_cache_put') {
        if (key === 'pending:definitions:phone' && Array.isArray(args?.value)) {
          definitionWrites += 1
          order.push('draft-write-start')
          await definitionWritePending
          order.push('draft-write-finish')
        }
        if (key === 'pending:definitions:phone' && args?.value === null) order.push('definitions-cleared')
        cache.set(key, args?.value)
        return undefined as T
      }
      if (command === 'remote_forget') { order.push('credential-forget'); return undefined as T }
      if (command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
      if (command === 'remote_request') return { id: (args?.request as { id?: string })?.id, result: agent } as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(screen.getByText('Offline')).toBeInTheDocument())
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.type(screen.getByLabelText('Schedule name'), 'Weekly review')
    await user.type(screen.getByLabelText('Task title'), 'Check services')
    await user.type(screen.getAllByLabelText('Instructions')[1], 'Inspect services')
    await user.type(screen.getByLabelText(/Schedule acceptance criteria/), 'List failures')
    await user.click(screen.getByRole('button', { name: 'Save schedule draft' }))
    await waitFor(() => expect(definitionWrites).toBe(1))

    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await user.click(await screen.findByRole('button', { name: 'Forget node' }))
    expect(invoke).not.toHaveBeenCalledWith('remote_forget', expect.anything())
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.click(screen.getByRole('button', { name: 'Save schedule draft' }))
    expect(definitionWrites).toBe(1)
    releaseDefinitionWrite?.()

    await waitFor(() => expect(invoke).toHaveBeenCalledWith('remote_forget', { id: 'phone' }))
    await waitFor(() => expect(cache.get('pending:definitions:phone')).toBeNull())
    expect(order).toEqual(['draft-write-start', 'draft-write-finish', 'credential-forget', 'definitions-cleared'])
    expect(invoke.mock.calls.filter(([command, args]) => command === 'remote_cache_put' && (args as { value?: unknown }).value === null))
      .toEqual([['remote_cache_put', expect.objectContaining({ key: 'pending:definitions:phone', value: null })]])
    expect(await screen.findByRole('heading', { name: 'Pair your Brr node' })).toBeInTheDocument()
  })

  it('defers forgetting while an offline definition publish awaits its remote receipt', async () => {
    const agent = { id: 'agent-1', name: 'Planner', ownerNodeId: 'worker', revision: 'agent-rev-3' }
    const snapshot = baseSnapshot({ agents: [agent] })
    const cache = new Map<string, unknown>()
    let snapshotCalls = 0
    let publishStarted = false
    let finishPublish: (() => void) | undefined
    const publishPending = new Promise<void>(resolve => { finishPublish = resolve })
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      const key = String(args?.key ?? '')
      if (command === 'remote_profiles') return [profile] as T
      if (command === 'remote_snapshot') {
        snapshotCalls += 1
        if (snapshotCalls === 1) throw new Error('offline for test')
        return snapshot as T
      }
      if (command === 'remote_cache_get') return (key.startsWith('snapshot:') ? snapshot : cache.get(key) ?? null) as T
      if (command === 'remote_cache_put') { cache.set(key, args?.value); return undefined as T }
      if (command === 'remote_forget') return undefined as T
      if (command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
      if (command === 'remote_request') {
        const request = args?.request as { id?: string; method?: string }
        if (request.method === 'workflows.create') { publishStarted = true; await publishPending }
        return { id: request.id, result: agent } as T
      }
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    await screen.findByRole('heading', { name: 'Chats' })
    await waitFor(() => expect(screen.getByText('Offline')).toBeInTheDocument())
    await user.click(screen.getByRole('tab', { name: /Agents/ }))
    await user.click(await screen.findByRole('button', { name: /Planner/ }))
    await user.selectOptions(await screen.findByLabelText('Agent execution node'), 'worker')
    await user.click(await screen.findByRole('tab', { name: 'Tasks & schedules' }))
    await user.type(screen.getByLabelText('Schedule name'), 'Publish this')
    await user.type(screen.getByLabelText('Task title'), 'Check services')
    await user.type(screen.getAllByLabelText('Instructions')[1], 'Inspect services')
    await user.type(screen.getByLabelText(/Schedule acceptance criteria/), 'Report failures')
    await user.click(screen.getByRole('button', { name: 'Save schedule draft' }))
    expect(await screen.findByText(/Schedule definition saved/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Refresh snapshot' }))
    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument())
    await user.click(screen.getByRole('button', { name: 'Review & create' }))
    await waitFor(() => expect(publishStarted).toBe(true))

    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await user.click(await screen.findByRole('button', { name: 'Forget node' }))
    expect(invoke).not.toHaveBeenCalledWith('remote_forget', expect.anything())
    expect(screen.getByText(/definition operation.*still completing/i)).toBeInTheDocument()
    finishPublish?.()
    expect(await screen.findByText(/Saved definition created on its owning node/)).toBeInTheDocument()
    expect(invoke).not.toHaveBeenCalledWith('remote_forget', expect.anything())

    await user.click(screen.getByRole('button', { name: 'Forget node' }))
    await waitFor(() => expect(invoke).toHaveBeenCalledWith('remote_forget', { id: 'phone' }))
  })

  it('shows an unpaired state with an error when definition cleanup fails after credential forget', async () => {
    const snapshot = baseSnapshot()
    const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
      if (command === 'remote_profiles') return [profile] as T
      if (command === 'remote_snapshot') return snapshot as T
      if (command === 'remote_cache_get') return (String(args?.key ?? '').startsWith('snapshot:') ? snapshot : null) as T
      if (command === 'remote_forget') return undefined as T
      if (command === 'remote_cache_put' && args?.key === 'pending:definitions:phone' && args.value === null) throw new Error('Definition cache unavailable')
      if (command === 'remote_cache_put' || command === 'remote_subscribe' || command === 'remote_unsubscribe') return undefined as T
      return undefined as T
    })
    const client = new RemoteClient({ invoke, listen: vi.fn(async () => () => undefined) } as NativeBridge)
    const user = userEvent.setup()
    render(<MobileApp client={client} native />)
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await user.click(await screen.findByRole('button', { name: 'Forget node' }))

    expect(await screen.findByRole('heading', { name: 'Pair your Brr node' })).toBeInTheDocument()
    expect(await screen.findByRole('alert')).toHaveTextContent('unpaired, but offline definition drafts could not be cleared: Definition cache unavailable')
    expect(invoke).toHaveBeenCalledWith('remote_forget', { id: 'phone' })
    expect(invoke).toHaveBeenCalledWith('remote_cache_put', { key: 'pending:definitions:phone', value: null })
  })

  it('commits the selected conflict version with the revision it was reviewed against', async () => {
    const snapshot = baseSnapshot({ sync: { configured: true } })
    const conflict = { id: 'conflict-1', kind: 'memory', revision: 'record-rev-7', base: { content: 'base' }, current: { content: 'current' }, incoming: { content: 'incoming' } }
    const { invoke, user } = renderConnected(snapshot, request => request.method === 'conflicts' ? { data: [conflict] } : request.method === 'history' ? { data: [] } : {})
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    await screen.findByRole('radiogroup', { name: 'Resolution version for conflict-1' })
    await user.click(screen.getByRole('radio', { name: 'Keep incoming' }))
    await user.click(screen.getByRole('button', { name: 'Commit chosen version' }))
    await waitFor(() => expect(sent(invoke, 'resolve')).toBe(true))
    const call = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { channel?: string; method?: string } })?.request?.channel === 'sync' && (args as { request?: { method?: string } })?.request?.method === 'resolve')
    expect((call?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ operationId: expect.any(String), expectedRevision: 'record-rev-7', params: { conflictId: 'conflict-1', choice: 'incoming' } })
  })

  it('requires an exact profile preview hash before applying a profile conflict', async () => {
    const snapshot = baseSnapshot({ sync: { configured: true } })
    const conflict = {
      id: 'agent-conflict', table: 'agentProfiles', kind: 'agent', revision: 'conflict-rev-4', profileReview: true,
      current: { content: { data: { candidate: { name: 'Current name', description: 'Current description', instructions: 'Current instructions', soul: 'Current soul', model: 'model-x', effort: 'medium' } } } },
      incoming: { content: { data: { candidate: { name: 'Incoming name', description: 'Incoming description', instructions: 'Incoming instructions', soul: 'Incoming soul', model: 'model-y', effort: 'high' } } } },
      base: null,
    }
    const { invoke, user } = renderConnected(snapshot, request => {
      if (request.method === 'conflicts') return { data: [conflict] }
      if (request.method === 'history') return { data: [] }
      if (request.method === 'previewProfile') return { candidate: request.params && (request.params as Record<string, unknown>).candidate, reviewHash: 'sha256:review-exact-candidate' }
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    expect((await screen.findAllByText(/Current description/)).length).toBeGreaterThan(0)
    expect(screen.getAllByText(/Incoming description/).length).toBeGreaterThan(0)
    const mergedName = screen.getByRole('textbox', { name: 'Merged profile name for agent-conflict' })
    await user.clear(mergedName)
    await user.type(mergedName, 'Reviewed merged name')
    await user.click(screen.getByRole('button', { name: 'Review exact profile' }))
    await screen.findByText(/sha256:review-exact-candidate/)
    const preview = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } }).request?.method === 'previewProfile')
    expect((preview?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ expectedRevision: 'conflict-rev-4', params: { conflictId: 'agent-conflict', candidate: { name: 'Reviewed merged name', effort: 'medium' } } })
    await user.clear(mergedName)
    await user.type(mergedName, 'Edited after review')
    expect(screen.queryByRole('button', { name: 'Apply reviewed profile' })).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Review exact profile' }))
    await screen.findByText(/sha256:review-exact-candidate/)
    await user.click(screen.getByRole('button', { name: 'Apply reviewed profile' }))
    await waitFor(() => expect(sent(invoke, 'approveProfile')).toBe(true))
    const approval = invoke.mock.calls.find(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } }).request?.method === 'approveProfile')
    expect((approval?.[1] as { request: Record<string, unknown> }).request).toMatchObject({ operationId: expect.any(String), expectedRevision: 'conflict-rev-4', params: { conflictId: 'agent-conflict', reviewHash: 'sha256:review-exact-candidate', candidate: { name: 'Edited after review' } } })
  })

  it('preserves profile conflict edits when an unrelated snapshot cursor advances', async () => {
    const snapshot = baseSnapshot({ sync: { configured: true } })
    const conflict = { id: 'cursor-conflict', table: 'agentProfiles', revision: 'cursor-rev-1', profileReview: true, current: { content: { data: { candidate: { name: 'Current', description: 'Keep local edit', instructions: 'Do', soul: '', model: 'model-x', effort: 'medium' } } } }, incoming: { content: { data: { candidate: { name: 'Other', description: 'Incoming', instructions: 'Do other', soul: '', model: 'model-y', effort: 'high' } } } } }
    let sequence = 0
    const { user, invoke } = renderConnected(snapshot, request => request.method === 'conflicts' ? { data: [conflict] } : request.method === 'history' ? { data: [] } : {}, {}, () => ({ ...snapshot, cursor: { epoch: snapshot.epoch, sequence: ++sequence } }))
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    const description = await screen.findByRole('textbox', { name: 'Merged profile description for cursor-conflict' })
    await user.clear(description)
    await user.type(description, 'Carefully merged description')
    const snapshotCalls = invoke.mock.calls.filter(([command]) => command === 'remote_snapshot').length
    await user.click(screen.getByRole('button', { name: 'Refresh snapshot' }))
    await waitFor(() => expect(invoke.mock.calls.filter(([command]) => command === 'remote_snapshot').length).toBeGreaterThan(snapshotCalls))
    expect(screen.getByRole('textbox', { name: 'Merged profile description for cursor-conflict' })).toHaveValue('Carefully merged description')
    expect(invoke.mock.calls.filter(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } }).request?.method === 'conflicts')).toHaveLength(1)
  })

  it('does not restore a profile review after its candidate changes during preview', async () => {
    const snapshot = baseSnapshot({ sync: { configured: true } })
    const conflict = { id: 'late-preview', table: 'agentProfiles', revision: 'late-rev', profileReview: true, current: { content: { data: { candidate: { name: 'Current', description: 'Description', instructions: 'Do', soul: '', model: 'model-x', effort: 'medium' } } } }, incoming: { content: { data: { candidate: { name: 'Incoming', description: 'Other', instructions: 'Other', soul: '', model: 'model-y', effort: 'high' } } } } }
    const previewReply = { resolve: (_result: unknown) => {} }
    const { user, invoke } = renderConnected(snapshot, request => {
      if (request.method === 'conflicts') return { data: [conflict] }
      if (request.method === 'history') return { data: [] }
      if (request.method === 'previewProfile') return new Promise(resolve => { previewReply.resolve = resolve })
      return {}
    })
    await screen.findByRole('heading', { name: 'Chats' })
    await user.click(screen.getByRole('tab', { name: /Fleet/ }))
    const name = await screen.findByRole('textbox', { name: 'Merged profile name for late-preview' })
    await user.click(screen.getByRole('button', { name: 'Review exact profile' }))
    await waitFor(() => expect(invoke.mock.calls.some(([command, args]) => command === 'remote_request' && (args as { request?: { method?: string } }).request?.method === 'previewProfile')).toBe(true))
    await user.clear(name)
    await user.type(name, 'Edited while review pending')
    previewReply.resolve({ candidate: { name: 'Current', description: 'Description', instructions: 'Do', soul: '', model: 'model-x', effort: 'medium' }, reviewHash: 'stale-hash' })
    await waitFor(() => expect(screen.getByRole('button', { name: 'Review exact profile' })).toBeInTheDocument())
    expect(screen.queryByRole('button', { name: 'Apply reviewed profile' })).not.toBeInTheDocument()
  })
})
