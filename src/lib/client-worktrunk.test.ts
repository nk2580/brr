import { beforeEach, describe, expect, it, vi } from 'vitest'

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }))
vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }))

import { client, WorktreeSetupRequired } from './client'

describe('native WorkTrunk client bridge', () => {
  beforeEach(() => invokeMock.mockReset())

  it('inspects the selected project before starting with the requested model and sandbox', async () => {
    invokeMock.mockResolvedValueOnce({ commands: [{ phase: 'post-create', template: 'pnpm install', approved: true }] })
      .mockResolvedValueOnce({ thread: { id: 'thread-1' } })

    await expect(client.start('/project', 'gpt-6.1-sol', true)).resolves.toEqual({ thread: { id: 'thread-1' } })
    expect(invokeMock.mock.calls.map(([, args]) => args)).toEqual([
      { method: 'brr/worktree/inspect', params: { cwd: '/project' } },
      { method: 'thread/start', params: {
        cwd: '/project', model: 'gpt-6.1-sol', modelProvider: 'openai', approvalPolicy: 'on-request', sandbox: 'read-only',
      } },
    ])
  })

  it('returns the typed pending-hook review and never starts a thread when hooks need approval', async () => {
    const commands = [
      { phase: 'post-create', name: 'Dependencies', template: 'pnpm install', approved: false },
      { phase: 'post-start', template: 'pnpm dev', approved: false },
    ]
    invokeMock.mockResolvedValue({ commands })

    await expect(client.start('/project', 'gpt-6.1-sol', false)).rejects.toMatchObject({
      name: 'WorktreeSetupRequired', kind: 'worktreeSetupRequired', cwd: '/project', commands,
    })
    expect(invokeMock).toHaveBeenCalledTimes(1)
    expect(invokeMock).not.toHaveBeenCalledWith('engine_request', expect.objectContaining({ method: 'thread/start' }))
  })

  it('propagates inspection failures without attempting thread creation', async () => {
    invokeMock.mockRejectedValueOnce(new Error('WorkTrunk is unavailable'))

    await expect(client.start('/project', 'gpt-6.1-sol', false)).rejects.toThrow('WorkTrunk is unavailable')
    expect(invokeMock).toHaveBeenCalledTimes(1)
    expect(invokeMock.mock.calls[0][1].method).toBe('brr/worktree/inspect')
  })

  it('sends only the explicitly approved command templates for the selected project', async () => {
    invokeMock.mockResolvedValue({ approved: true })

    await client.approveWorktree('/project', ['pnpm install', 'pnpm dev'])

    expect(invokeMock).toHaveBeenCalledWith('engine_request', {
      method: 'brr/worktree/approve', params: { cwd: '/project', templates: ['pnpm install', 'pnpm dev'] },
    })
  })
})
