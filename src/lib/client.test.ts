import { beforeEach, describe, expect, it, vi } from 'vitest'

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }))

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }))

import { client } from './client'

describe('project file manager', () => {
  beforeEach(() => invokeMock.mockReset())

  it('reveals the original project folder and reports native errors', async () => {
    invokeMock.mockResolvedValueOnce(undefined)
    const path = '/Users/example/Project λ & notes'
    await client.revealProject(path)
    expect(invokeMock).toHaveBeenCalledWith('reveal_project_folder', {path})

    invokeMock.mockRejectedValueOnce('Cannot access project folder')
    await expect(client.revealProject(path)).rejects.toBe('Cannot access project folder')
  })
})

describe('project management', () => {
  beforeEach(() => invokeMock.mockReset())

  it('creates folderless project buckets and moves chats without archiving them', async () => {
    const input = {name: 'Research', folders: [], primaryFolder: null, model: null, effort: null, permission: null}
    const project = {...input, id: 'project-research', revision: 1, hidden: false}
    invokeMock.mockResolvedValueOnce(project).mockResolvedValueOnce(undefined).mockResolvedValueOnce(undefined)
    expect(await client.createProject(input)).toEqual(project)
    await client.moveChat('chat-1', project.id)
    await client.deleteProject(project.id)
    expect(invokeMock.mock.calls).toEqual([
      ['create_project', {project: input}],
      ['move_chat_to_project', {threadId: 'chat-1', projectId: project.id}],
      ['delete_project', {projectId: project.id}],
    ])
  })

  it('starts a folderless chat with explicit project membership and host-resolved folders', async () => {
    invokeMock.mockResolvedValueOnce({commands: []}).mockResolvedValueOnce({thread: {id: 'chat-1', projectId: 'research'}})
    await client.start('research', 'gpt-6.1-sol', true)
    expect(invokeMock.mock.calls).toEqual([
      ['engine_request', {method: 'brr/project/inspect', params: {projectId: 'research'}}],
      ['engine_request', {method: 'thread/start', params: {projectId: 'research', model: 'gpt-6.1-sol', modelProvider: 'openai', approvalPolicy: 'on-request', sandbox: 'read-only'}}],
    ])
  })

})

describe('client thread resume', () => {
  beforeEach(() => invokeMock.mockReset())

  it('loads every full turn page when resumed history is paginated', async () => {
    const firstTurn = { id: 'turn-1', status: 'completed', items: [{ id: 'message-1' }] }
    const secondTurn = { id: 'turn-2', status: 'completed', items: [{ id: 'message-2' }] }
    invokeMock.mockImplementation(async (_command: string, args: any = {}) => {
      const { method, params = {} } = args
      if (method === 'thread/resume') {
        return { thread: { id: 'thread-1', historyMode: 'paginated', turns: [] } }
      }
      if (method === 'thread/turns/list' && !params.cursor) {
        return { data: [firstTurn], nextCursor: 'page-2' }
      }
      if (method === 'thread/turns/list' && params.cursor === 'page-2') {
        return { data: [secondTurn], nextCursor: null }
      }
      if (_command !== 'engine_request') return undefined
      if (_command !== 'engine_request') return undefined
      throw new Error(`Unexpected RPC: ${method}`)
    })

    const result = await client.resume('thread-1')

    expect(result.thread.turns).toEqual([firstTurn, secondTurn])
    const requests = invokeMock.mock.calls
      .filter(([command]) => command === 'engine_request')
      .map(([, args]) => args)
    expect(requests[0]).toMatchObject({ method: 'thread/resume', params: { threadId: 'thread-1' } })
    expect(requests[0].params).not.toHaveProperty('sandbox')
    expect(requests.slice(1)).toEqual([
      {
        method: 'thread/turns/list',
        params: { threadId: 'thread-1', itemsView: 'full', sortDirection: 'asc', limit: 100 },
      },
      {
        method: 'thread/turns/list',
        params: { threadId: 'thread-1', itemsView: 'full', sortDirection: 'asc', limit: 100, cursor: 'page-2' },
      },
    ])
  })

  it('preserves the root saved collaboration mode when collecting paginated turns', async () => {
    invokeMock.mockImplementation(async (_command: string, args: any = {}) => {
      const { method } = args
      if (method === 'thread/resume') return { thread: { id: 'thread-plan', historyMode: 'paginated' }, collaborationMode: { mode: 'plan' } }
      if (method === 'thread/turns/list') return { data: [], nextCursor: null }
      if (_command !== 'engine_request') return undefined
      throw new Error(`Unexpected RPC: ${method}`)
    })

    const result = await client.resume('thread-plan')

    expect(result.thread.collaborationMode).toEqual({ mode: 'plan' })
  })

  it('keeps legacy resume responses and does not request additional pages', async () => {
    const turn = { id: 'turn-legacy', status: 'completed', items: [{ id: 'legacy-message' }] }
    invokeMock.mockResolvedValue({ thread: { id: 'thread-legacy', historyMode: 'legacy', turns: [turn] } })

    const result = await client.resume('thread-legacy')

    expect(result.thread.turns).toEqual([turn])
    expect(invokeMock).toHaveBeenCalledTimes(1)
    expect(invokeMock.mock.calls[0][1].params).not.toHaveProperty('sandbox')
  })
})

describe('client steering', () => {
  beforeEach(() => invokeMock.mockReset())

  it('targets the active turn with a text input and returns the runtime turn id', async () => {
    invokeMock.mockResolvedValue({ turnId: 'turn-active' })

    const result = await client.steer('thread-1', 'Use the smaller helper', 'turn-active')

    expect(result).toEqual({ turnId: 'turn-active' })
    expect(invokeMock).toHaveBeenCalledWith('engine_request', {
      method: 'turn/steer',
      params: {
        threadId: 'thread-1',
        input: [{ type: 'text', text: 'Use the smaller helper', text_elements: [] }],
        expectedTurnId: 'turn-active',
      },
    })
  })
})

describe('client planning mode', () => {
  beforeEach(() => invokeMock.mockReset())

  it('sends an explicit collaboration mode with canonical reasoning settings', async () => {
    invokeMock.mockResolvedValue({ turn: { id: 'turn-1' } })

    await client.send('thread-1', 'Make a plan', 'gpt-6.1-sol', 'high', 'plan')

    expect(invokeMock).toHaveBeenCalledWith('engine_request', {
      method: 'turn/start',
      params: {
        threadId: 'thread-1',
        input: [{ type: 'text', text: 'Make a plan', text_elements: [] }],
        model: 'gpt-6.1-sol',
        effort: 'high',
        collaborationMode: {
          mode: 'plan',
          settings: { model: 'gpt-6.1-sol', reasoning_effort: 'high', developer_instructions: null },
        },
      },
    })
  })

  it('defaults every send to an explicit default collaboration mode', async () => {
    invokeMock.mockResolvedValue({ turn: { id: 'turn-2' } })

    await client.send('thread-1', 'Continue', 'gpt-6.1-sol', 'medium')

    expect(invokeMock).toHaveBeenCalledWith('engine_request', expect.objectContaining({
      method: 'turn/start',
      params: expect.objectContaining({
        collaborationMode: {
          mode: 'default',
          settings: { model: 'gpt-6.1-sol', reasoning_effort: 'medium', developer_instructions: null },
        },
      }),
    }))
  })
})
