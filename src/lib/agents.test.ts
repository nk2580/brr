import { beforeEach, describe, expect, it, vi } from 'vitest'

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke }))

import { agents } from './agents'

describe('Agents client', () => {
  beforeEach(() => invoke.mockReset())

  it('reviews a profile draft using the candidate hash before approving it', async () => {
    invoke.mockResolvedValue({ agent: { id: 'agent-1' } })

    await agents.approve('draft-1', 'sha256:candidate')

    expect(invoke).toHaveBeenCalledWith('agents_request', {
      method: 'agents.approve',
      params: { draftId: 'draft-1', hash: 'sha256:candidate' },
    })
  })

  it('creates tasks with acceptance criteria and dependency ids intact', async () => {
    const input = {
      agentId: 'agent-1', title: 'Review release', instructions: 'Inspect the release.',
      acceptanceCriteria: ['List blockers'], dependencies: ['task-prerequisite'],
    }
    invoke.mockResolvedValue({ id: 'task-1', ...input })

    await agents.createTask(input)

    expect(invoke).toHaveBeenCalledWith('agents_request', { method: 'tasks.create', params: input })
  })

  it('schedules a reviewed proposal using its original graph hash and recurrence settings', async () => {
    const workflow = { id: 'workflow-1', name: 'Weekly release graph', expression: '0 9 * * 1', timezone: 'Australia/Brisbane' }
    invoke.mockResolvedValue(workflow)

    await expect(agents.scheduleProposal('proposal-1', 'graph-hash', {
      name: 'Weekly release graph', expression: '0 9 * * 1', timezone: 'Australia/Brisbane',
    })).resolves.toEqual(workflow)

    expect(invoke).toHaveBeenCalledWith('agents_request', {
      method: 'tasks.schedule',
      params: { proposalId: 'proposal-1', hash: 'graph-hash', name: 'Weekly release graph', expression: '0 9 * * 1', timezone: 'Australia/Brisbane' },
    })
  })

  it('requests the next task page with its agent scope, cursor, and limit', async () => {
    invoke.mockResolvedValue({ data: [], nextCursor: null })

    await agents.tasks('agent-1', { after: 'task-cursor', limit: 25 })

    expect(invoke).toHaveBeenCalledWith('agents_request', {
      method: 'tasks.list', params: { agentId: 'agent-1', after: 'task-cursor', limit: 25 },
    })
  })

  it('returns the exact memory event id needed to undo a forget action', async () => {
    invoke.mockResolvedValue({ eventId: 42 })

    await expect(agents.forgetMemory('memory-1', 'agent-1')).resolves.toEqual({ eventId: 42 })

    expect(invoke).toHaveBeenCalledWith('agents_request', {
      method: 'memory.forget', params: { id: 'memory-1', agentId: 'agent-1' },
    })
  })
})
