import { beforeEach, describe, expect, it, vi } from 'vitest'

const invoke = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

import { tickets, ticketLabel, ticketConflict, ticketProposalProjectSnapshot } from './tickets'

describe('tickets API', () => {
  beforeEach(() => invoke.mockReset())

  it('sends ticket requests through the native tickets bridge', async () => {
    invoke.mockResolvedValue({ data: [] })
    await tickets.list({ projectId: 'project-1' })
    expect(invoke).toHaveBeenCalledWith('tickets_request', { method: 'list', params: { projectId: 'project-1' } })
  })

  it('sends an optional registered folder with the reviewed run proposal', async () => {
    invoke.mockResolvedValue({ id: 'proposal-1' })
    await tickets.proposeRun('ticket-1', 3, 'agent-1', '/workspace/app/packages/ui', 'stable-op')
    expect(invoke).toHaveBeenCalledWith('tickets_request', { method: 'proposeRun', params: { ticketId: 'ticket-1', revision: 3, agentId: 'agent-1', folder: '/workspace/app/packages/ui', operationId: 'stable-op' } })
  })

  it('formats stable ticket numbers and extracts current records from conflict errors', () => {
    expect(ticketLabel({ id: 't1', number: 'BRR-000027' } as never)).toBe('BRR-000027')
    const current = { id: 't1', revision: 4 }
    expect(ticketConflict(new Error(JSON.stringify({ code: 'conflict', current })))).toEqual(current)
  })

  it('reads reviewed project roots from immutable proposal context and tolerates old or malformed context', () => {
    const snapshot = ticketProposalProjectSnapshot('Execute ticket BRR-000001\nBrr project snapshot: {"id":"project-1","folders":["/reviewed/a","/reviewed/b"],"selectedFolder":"/reviewed/b"}')
    expect(snapshot).toEqual({ id: 'project-1', folders: ['/reviewed/a', '/reviewed/b'], selectedFolder: '/reviewed/b' })
    expect(ticketProposalProjectSnapshot('Execute fixture ticket\nBrr project snapshot: JSON {"id":"project-2","folders":["/reviewed/root-one","/reviewed/root-two"]}'))
      .toEqual({ id: 'project-2', folders: ['/reviewed/root-one', '/reviewed/root-two'] })
    expect(ticketProposalProjectSnapshot('Old proposal context')).toBeNull()
    expect(ticketProposalProjectSnapshot('Brr project snapshot: {bad json')).toBeNull()
  })
})
