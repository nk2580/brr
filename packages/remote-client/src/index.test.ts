import { describe, expect, it, vi } from 'vitest'
import { PendingMutationError, RemoteClient, UncertainMutationError, type NativeBridge, type Request } from './index'

function harness() {
  const cache = new Map<string, unknown>()
  const sent: Request[] = []
  let failNextSend = false
  let nextError: string | undefined
  let wrongReplyId = false
  const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => {
    if (command === 'remote_cache_get') return (cache.get(String(args?.key)) ?? null) as T
    if (command === 'remote_cache_put') {
      cache.set(String(args?.key), args?.value)
      return undefined as T
    }
    if (command === 'remote_request') {
      sent.push(args?.request as Request)
      if (failNextSend) { failNextSend = false; throw new Error('TLS connection closed') }
      if (wrongReplyId) {
        wrongReplyId = false
        return { id: 'different-request', result: { accepted: true } } as T
      }
      if (nextError) {
        const code = nextError
        nextError = undefined
        return { id: (args?.request as Request).id, error: { code, message: 'Result not available' } } as T
      }
      return { id: (args?.request as Request).id, result: { accepted: true } } as T
    }
    return undefined as T
  })
  const client = new RemoteClient({ invoke, listen: vi.fn() } as unknown as NativeBridge)
  return { cache, sent, invoke, client, failSend() { failNextSend = true }, respondWithError(code: string) { nextError = code }, replyToDifferentRequest() { wrongReplyId = true } }
}

const mutation = (operationId: string, targetNodeId = 'worker') => ({
  id: `request-${operationId}`, operationId, channel: 'sessions' as const, method: 'send', targetNodeId,
  params: { threadId: 'thread-1', text: 'continue' },
})

describe('remote mutation delivery', () => {
  it('persists the exact request envelope before dispatching a mutation', async () => {
    const h = harness()
    const request = mutation('operation-stable')
    await h.client.request('phone', request)

    const cacheWrites = h.invoke.mock.calls.filter(([command]) => command === 'remote_cache_put')
    const sendIndex = h.invoke.mock.calls.findIndex(([command]) => command === 'remote_request')
    const journalIndexWrite = cacheWrites.find(([, args]) => String(args?.key).startsWith('remote:pending-mutation-index:'))
    const envelopeWrite = cacheWrites.find(([, args]) => String(args?.key).startsWith('remote:pending-mutation:'))
    expect(envelopeWrite?.[1]?.value).toMatchObject({ profileId: 'phone', targetNodeId: 'worker', operationId: 'operation-stable', request, attempts: 1 })
    expect(h.invoke.mock.invocationCallOrder[h.invoke.mock.calls.indexOf(journalIndexWrite!)]).toBeLessThan(h.invoke.mock.invocationCallOrder[h.invoke.mock.calls.indexOf(envelopeWrite!)])
    expect(h.invoke.mock.invocationCallOrder[h.invoke.mock.calls.indexOf(envelopeWrite!)]).toBeLessThan(h.invoke.mock.invocationCallOrder[sendIndex])
  })

  it('keeps an uncertain exact envelope and never silently resends it', async () => {
    const h = harness()
    h.failSend()
    const request = mutation('operation-uncertain')
    await expect(h.client.request('phone', request)).rejects.toBeInstanceOf(UncertainMutationError)
    const pending = await h.client.pendingMutations('phone', 'worker')
    expect(pending).toHaveLength(1)
    expect(pending[0].request).toEqual(request)
    await expect(h.client.request('phone', request)).rejects.toBeInstanceOf(PendingMutationError)
    expect(h.sent).toHaveLength(1)
  })

  it('retries only by explicit action with the same request and operation IDs', async () => {
    const h = harness()
    h.failSend()
    const request = mutation('operation-retry')
    await expect(h.client.request('phone', request)).rejects.toBeInstanceOf(UncertainMutationError)
    await h.client.retryMutation('phone', 'operation-retry')
    expect(h.sent).toHaveLength(2)
    expect(h.sent[1]).toEqual(request)
    expect(await h.client.pendingMutations('phone')).toEqual([])
  })

  it('does not send a saved action when an explicit retry is requested while offline or uncertain', async () => {
    const h = harness()
    h.failSend()
    await expect(h.client.request('phone', mutation('operation-offline-retry'))).rejects.toBeInstanceOf(UncertainMutationError)
    await expect(h.client.retryMutation('phone', 'operation-offline-retry', 'offline')).rejects.toThrow(/needs a connection/)
    await expect(h.client.retryMutation('phone', 'operation-offline-retry', 'uncertain')).rejects.toThrow(/already in progress/)
    expect(h.sent).toHaveLength(1)
    expect(await h.client.pendingMutations('phone')).toHaveLength(1)
  })

  it('blocks overlapping unresolved work for the same profile and node, but scopes other nodes separately', async () => {
    const h = harness()
    h.failSend()
    await expect(h.client.request('phone', mutation('operation-a'))).rejects.toBeInstanceOf(UncertainMutationError)
    await expect(h.client.request('phone', mutation('operation-b'))).rejects.toBeInstanceOf(PendingMutationError)
    await h.client.request('phone', mutation('operation-c', 'other-worker'))
    await h.client.request('other-phone', mutation('operation-d'))
    expect(h.sent.map(request => request.operationId)).toEqual(['operation-a', 'operation-c', 'operation-d'])
  })

  it('strips operation IDs from reads and does not journal them', async () => {
    const h = harness()
    await h.client.request('phone', { id: 'read-id', operationId: 'must-not-leak', channel: 'sync', method: 'previewProfile', params: { draftId: 'draft-1' } })
    expect(h.sent[0]).not.toHaveProperty('operationId')
    expect(await h.client.pendingMutations('phone')).toEqual([])
    expect(h.invoke.mock.calls.filter(([command]) => command === 'remote_cache_put')).toEqual([])
  })

  it('keeps the exact operation after an uncertain server reply or a later offline retry', async () => {
    const h = harness()
    const request = mutation('server-uncertain')
    h.respondWithError('uncertain')
    await expect(h.client.request('phone', request)).resolves.toMatchObject({ error: { code: 'uncertain' } })
    expect(await h.client.pendingMutations('phone')).toMatchObject([{ request }])
    await expect(h.client.request('phone', mutation('new-operation'))).rejects.toBeInstanceOf(PendingMutationError)

    h.respondWithError('owner_offline')
    await h.client.retryMutation('phone', request.operationId)
    expect(await h.client.pendingMutations('phone')).toMatchObject([{ request }])

    await h.client.retryMutation('phone', request.operationId)
    expect(h.sent).toEqual([request, request, request])
    expect(await h.client.pendingMutations('phone')).toEqual([])
  })

  it('does not block future mutations after a confirmed pre-dispatch rejection', async () => {
    const h = harness()
    h.respondWithError('owner_offline')
    await h.client.request('phone', mutation('offline-before-first-send'))
    expect(await h.client.pendingMutations('phone')).toEqual([])
    await h.client.request('phone', mutation('after-reconnect'))
    expect(h.sent).toHaveLength(2)
  })

  it('never clears an action on an acknowledgement for a different request', async () => {
    const h = harness()
    h.replyToDifferentRequest()
    const request = mutation('wrong-ack')
    await expect(h.client.request('phone', request)).rejects.toBeInstanceOf(UncertainMutationError)
    expect(await h.client.pendingMutations('phone')).toMatchObject([{ request }])
    await h.client.retryMutation('phone', request.operationId)
    expect(h.sent).toEqual([request, request])
  })

  it.each([
    ['system', 'models'], ['system', 'account'],
    ['agents', 'agents.list'], ['agents', 'agents.read'], ['agents', 'agents.drafts'],
    ['agents', 'agents.sessions'], ['agents', 'memory.search'],
    ['chat', 'drafts.list'], ['chat', 'queue.list'],
  ] as const)('does not leave a pending mutation for failed %s %s reads', async (channel, method) => {
    const h = harness()
    h.failSend()
    await expect(h.client.request('phone', { channel, method, params: {}, operationId: 'old-client-id' })).rejects.toThrow('TLS connection closed')
    expect(h.sent[0]).not.toHaveProperty('operationId')
    expect(await h.client.pendingMutations('phone')).toEqual([])
  })
})
