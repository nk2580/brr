import { describe, expect, it, vi } from 'vitest'
import { createOperationId, parseInvitation, RemoteClient, type NativeBridge, type Request } from '@brr/remote-client'

function bridge() {
  const invoke = vi.fn(async <T,>(command: string, args?: Record<string, unknown>) => (
    command === 'remote_request' ? { id: (args?.request as Request).id, result: { accepted: true } } : undefined
  ) as T)
  const listen = vi.fn(async (_event: string, _handler: (payload: unknown) => void) => () => undefined)
  return { invoke, listen, client: new RemoteClient({ invoke, listen } as NativeBridge) }
}

describe('remote client safety boundary', () => {
  it('never dispatches a remote mutation while offline', async () => {
    const { client, invoke } = bridge()
    await expect(client.request('phone', { channel: 'agents', method: 'tasks.create', params: { title: 'Review' }, operationId: createOperationId() }, 'offline'))
      .rejects.toThrow('Your draft is still saved on this device')
    expect(invoke).not.toHaveBeenCalled()
  })

  it('does not replay a mutation whose delivery state is uncertain', async () => {
    const { client, invoke } = bridge()
    await expect(client.request('phone', { channel: 'sessions', method: 'send', params: { threadId: 'thread-a', text: 'Continue' }, operationId: 'send-once' }, 'uncertain'))
      .rejects.toThrow('Check its current state before trying again')
    expect(invoke).not.toHaveBeenCalled()
  })

  it('preserves operation, runtime epoch and execution-owner scope for an approval', async () => {
    const { client, invoke } = bridge()
    await client.request('phone', {
      channel: 'approvals', method: 'answer', params: { approvalId: 'approval-original', approved: true },
      operationId: 'decision-1', expectedEpoch: 'runtime-7', targetNodeId: 'linux-node',
    })
    expect(invoke).toHaveBeenCalledWith('remote_request', {
      profileId: 'phone',
      request: expect.objectContaining({ id: expect.any(String), operationId: 'decision-1', expectedEpoch: 'runtime-7', targetNodeId: 'linux-node' }),
    })
  })

  it('accepts a pinned HTTPS invitation and rejects an HTTP endpoint', () => {
    const pem = '-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----'
    expect(parseInvitation(JSON.stringify({ endpoint: 'https://brr.local:4100', code: 'one-use', fingerprint: 'abc', certificatePem: pem })).endpoint)
      .toBe('https://brr.local:4100')
    expect(() => parseInvitation(JSON.stringify({ endpoint: 'http://brr.local', code: 'one-use', fingerprint: 'abc', certificatePem: pem })))
      .toThrow('HTTPS endpoint')
  })
})
