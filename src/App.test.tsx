import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, it, expect, vi } from 'vitest'
import App from './App'

const events = vi.hoisted(() => ({ handler: null as null | ((event: any) => void), handlers: [] as Array<(event: any) => void> }))
const backend = vi.hoisted(() => ({
  desktop: true,
  connect: vi.fn(async () => ({ account: null, models: [], threads: [] })),
  login: vi.fn(async () => ({ loginId: 'login-1' })),
  cancelLogin: vi.fn(async () => ({status:'canceled'})),
  account: vi.fn(async () => null),
  subscribe: vi.fn(async (handler: (event: any) => void) => {
    events.handlers.push(handler)
    events.handler = handler
    return () => {
      events.handlers = events.handlers.filter(candidate => candidate !== handler)
      events.handler = events.handlers.at(-1) ?? null
    }
  }),
  chooseWorkspace: vi.fn(async () => '/project'),
  approveWorktree: vi.fn(async () => undefined),
  start: vi.fn(async () => ({thread:{id:'thread-1', cwd:'/project', preview:'', updatedAt:1}})),
  resume: vi.fn(async (id: string) => ({thread:{id, cwd:'/project', preview:'', updatedAt:1, turns:[]}})),
  send: vi.fn(async () => ({turn:{id:'turn-1'}})),
  steer: vi.fn(async () => ({turnId:'turn-1'})),
  respond: vi.fn(async () => ({})),
  interrupt: vi.fn(async () => ({})),
  archive: vi.fn(async () => ({})),
  logout: vi.fn(async () => ({})),
  threads: vi.fn(async () => []),
}))
const agentApi = vi.hoisted(() => ({
  list: vi.fn(async () => ({ data: [] })), settings: vi.fn(async () => ({ maxBackground: 4, maxPerAgent: 2 })),
  decisions: vi.fn(async () => ({ data: [] })), drafts: vi.fn(async () => ({ data: [] })),
  interview: vi.fn(), approve: vi.fn(), sessions: vi.fn(async () => ({ data: [] })), open: vi.fn(),
  tasks: vi.fn(async () => ({ data: [] })), createTask: vi.fn(), proposals: vi.fn(async () => ({ data: [] })),
  workflows: vi.fn(async () => ({ data: [] })), memory: vi.fn(async () => ({ data: [] })),
  skills: vi.fn(async () => ({ data: [] })), projects: vi.fn(async () => ({ data: [] })),
  inspectWorkspace: vi.fn(async (cwd: string) => ({ cwd, git: false, commands: [] })),
}))
vi.mock('./lib/client', () => ({ client: backend, isDesktop: () => backend.desktop, WorktreeSetupRequired: class extends Error { kind = 'worktreeSetupRequired'; constructor(public cwd: string, public commands: unknown[]) { super('Review project setup hooks') } } }))
vi.mock('./lib/agents', () => ({ agents: agentApi }))

describe('desktop workspace', () => {
  beforeEach(() => localStorage.clear())
  it('loads saved agent identities after delayed connection and refreshes them on agent changes', async () => {
    let finishConnection!: (connection: unknown) => void
    const savedAgent = { id: 'saved-agent', name: 'Release guide', description: 'Keeps releases safe', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'high', revision: 1, createdAt: 1, updatedAt: 1 }
    backend.connect.mockImplementationOnce(() => new Promise(resolve => { finishConnection = resolve }) as never)
    agentApi.list.mockResolvedValueOnce({ data: [savedAgent] } as never)
    render(<App />)

    await waitFor(() => expect(backend.connect).toHaveBeenCalled())
    expect(agentApi.list).not.toHaveBeenCalled()
    await act(async () => {
      finishConnection({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [] })
    })

    const identity = await screen.findByRole('combobox', { name: 'Chat identity' })
    await waitFor(() => expect(Array.from((identity as HTMLSelectElement).options).map(option => option.text)).toContain('Release guide'))

    agentApi.list.mockResolvedValueOnce({ data: [{ ...savedAgent, name: 'Release guide revised', revision: 2 }] } as never)
    act(() => events.handlers.forEach(handler => handler({ method: 'brr/agents/changed', params: {} })))
    await waitFor(() => expect(Array.from((identity as HTMLSelectElement).options).map(option => option.text)).toContain('Release guide revised'))
  })

  it('uses a contextual Agents sidebar and preserves its collapsed state and draft when returning Home', async () => {
    render(<App />)
    const message = screen.getByRole('textbox', { name: 'Message' })
    await userEvent.type(message, 'Keep this draft')

    await userEvent.click(await screen.findByRole('button', { name: 'Agents' }))

    expect(await screen.findByRole('region', { name: 'Agents' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Agents' })).toBeInTheDocument()
    expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Activity on' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'New conversation' })).not.toBeInTheDocument()
    const collapse = screen.getByRole('button', { name: 'Collapse sidebar' })
    expect(collapse).toHaveAttribute('aria-controls', 'agents-sidebar')
    expect(collapse).toHaveAttribute('aria-expanded', 'true')
    await userEvent.click(collapse)
    expect(screen.getByRole('button', { name: 'Expand sidebar' })).toHaveAttribute('aria-controls', 'agents-sidebar')

    await userEvent.click(screen.getByRole('button', { name: 'Home' }))
    expect(screen.queryByRole('region', { name: 'Agents' })).not.toBeInTheDocument()
    expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this draft')
    const expand = screen.getByRole('button', { name: 'Expand sidebar' })
    expect(expand).toHaveAttribute('aria-controls', 'main-sidebar')
    await userEvent.click(expand)
    expect(screen.getByRole('complementary', { name: 'Main sidebar' })).toBeVisible()
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this draft')
  })

  it('returns to the selected conversation when opening a recent chat from Agents', async () => {
    backend.connect.mockResolvedValueOnce({ account: null, models: [], threads: [
      { id: 'navigation-thread', name: 'Existing navigation thread', preview: 'Existing navigation thread', cwd: '/project', updatedAt: 1 },
    ] } as never)
    render(<App />)
    await screen.findByRole('button', { name: 'Existing navigation thread' })
    await userEvent.click(screen.getByRole('button', { name: 'Agents' }))
    expect(await screen.findByRole('region', { name: 'Agents' })).toBeVisible()

    await userEvent.click(screen.getByRole('button', { name: 'Home' }))
    await userEvent.click(screen.getByRole('button', { name: 'Existing navigation thread' }))

    expect(screen.queryByRole('region', { name: 'Agents' })).not.toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Message' })).toBeVisible()
  })

  it('starts a selected named agent with its saved model and workspace defaults', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    backend.chooseWorkspace.mockResolvedValueOnce('/project')
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'high', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    agentApi.open.mockResolvedValueOnce({ thread: { id: 'agent-session', cwd: '/project', preview: '', updatedAt: 1 } })
    render(<App />)

    await userEvent.click(await screen.findByRole('button', { name: 'Choose project folder' }))
    await userEvent.selectOptions(await screen.findByRole('combobox', { name: 'Chat identity' }), 'agent-1')
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Review this release')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(agentApi.open).toHaveBeenCalledWith('agent-1', { model: 'gpt-6.1-sol', effort: 'high', cwd: '/project', readOnly: false }))
    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('agent-session', 'Review this release', 'gpt-6.1-sol', 'high', 'default'))
    expect(backend.start).not.toHaveBeenCalled()
  })

  it('can start a named agent chat without requiring a project folder', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'medium', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    agentApi.open.mockResolvedValueOnce({ thread: { id: 'agent-session-no-project', cwd: '', preview: '', updatedAt: 1 } })
    render(<App />)

    const identity = await screen.findByRole('combobox', { name: 'Chat identity' })
    await waitFor(() => expect(Array.from((identity as HTMLSelectElement).options).map(option => option.value)).toContain('agent-1'))
    await userEvent.selectOptions(identity, 'agent-1')
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Start here')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(agentApi.open).toHaveBeenCalledWith('agent-1', { model: 'gpt-6.1-sol', effort: 'medium', readOnly: false }))
    expect(backend.start).not.toHaveBeenCalled()
  })

  it('opens a fresh interview thread without trying to resume its unpersisted rollout', async () => {
    const defaultResume = backend.resume.getMockImplementation()
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    agentApi.interview.mockResolvedValueOnce({ thread: { id: 'fresh-interview', cwd: '/project', preview: 'New agent interview', updatedAt: 1 } })
    backend.resume.mockImplementation(async id => {
      if (id === 'fresh-interview') throw new Error('no rollout found for thread id')
      return { thread: { id, cwd: '/project', preview: '', updatedAt: 1, turns: [] } }
    })
    render(<App />)

    await userEvent.click(await screen.findByRole('button', { name: 'Agents' }))
    await userEvent.click((await screen.findAllByRole('button', { name: 'New agent' }))[0])
    const message = await screen.findByRole('textbox', { name: 'Message' })
    expect(backend.resume).not.toHaveBeenCalledWith('fresh-interview')
    await userEvent.type(message, 'Help me define a careful release guide')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('fresh-interview', 'Help me define a careful release guide', 'gpt-6.1-sol', 'medium', 'default'))
    expect(screen.queryByText('no rollout found for thread id')).not.toBeInTheDocument()
    backend.resume.mockImplementation(defaultResume!)
  })

  it('opens a fresh named-agent session without resuming before its first turn', async () => {
    const defaultResume = backend.resume.getMockImplementation()
    const profile = { id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'high', revision: 1, createdAt: 1, updatedAt: 1 }
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    agentApi.list.mockResolvedValueOnce({ data: [profile] } as never)
    agentApi.list.mockResolvedValueOnce({ data: [profile] } as never)
    agentApi.open.mockResolvedValueOnce({ thread: { id: 'fresh-agent-session', cwd: '', preview: 'New session', updatedAt: 1 } })
    backend.resume.mockImplementation(async id => {
      if (id === 'fresh-agent-session') throw new Error('no rollout found for thread id')
      return { thread: { id, cwd: '/project', preview: '', updatedAt: 1, turns: [] } }
    })
    render(<App />)

    await userEvent.click(await screen.findByRole('button', { name: 'Agents' }))
    await userEvent.click(await screen.findByRole('button', { name: /Release guide/ }))
    await userEvent.click(await screen.findByRole('button', { name: 'New session' }))
    const message = await screen.findByRole('textbox', { name: 'Message' })
    expect(backend.resume).not.toHaveBeenCalledWith('fresh-agent-session')
    await userEvent.type(message, 'Review this release')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('fresh-agent-session', 'Review this release', 'gpt-6.1-sol', 'high', 'default'))
    expect(screen.queryByText('no rollout found for thread id')).not.toBeInTheDocument()
    backend.resume.mockImplementation(defaultResume!)
  })

  it('shows exact project setup hooks before starting a named agent in a Git project', async () => {
    const agentOpensBefore = agentApi.open.mock.calls.length
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    backend.chooseWorkspace.mockResolvedValueOnce('/project')
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'medium', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    agentApi.inspectWorkspace.mockResolvedValueOnce({ cwd: '/project', git: true, commands: [{ phase: 'post-create', name: 'install', template: 'pnpm install', approved: false }] } as never)
    render(<App />)

    await userEvent.click(await screen.findByRole('button', { name: 'Choose project folder' }))
    await userEvent.selectOptions(await screen.findByRole('combobox', { name: 'Chat identity' }), 'agent-1')
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Review the release')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    const review = await screen.findByRole('region', { name: 'Worktree setup approval' })
    expect(review).toHaveTextContent('pnpm install')
    expect(agentApi.open.mock.calls.length).toBe(agentOpensBefore)
  })

  it('restores a saved session identity from the thread binding after reload', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'agent-session', cwd:'/project', preview:'Release review', updatedAt:1, brr:{agentId:'agent-1', kind:'agent'}}]} as never)
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'high', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    render(<App />)

    await userEvent.click(await screen.findByRole('button', { name: 'Release review' }))

    const identity = await screen.findByRole('combobox', { name: 'Chat identity' })
    await waitFor(() => expect(identity).toHaveValue('agent-1'))
    expect(identity).toBeDisabled()
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('agent-session'))
  })

  it('turns an exact @mention into a separate task with only the current message', async () => {
    const sendsBefore = backend.send.mock.calls.length
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    backend.chooseWorkspace.mockResolvedValueOnce('/project')
    agentApi.list.mockResolvedValueOnce({ data: [{ id: 'agent-1', name: 'Release guide', description: '', instructions: '', soul: '', model: 'gpt-6.1-sol', effort: 'medium', revision: 1, createdAt: 1, updatedAt: 1 }] } as never)
    agentApi.createTask.mockResolvedValueOnce({ id: 'task-1', status: 'queued' })
    render(<App />)

    await userEvent.click(await screen.findByRole('button', { name: 'Choose project folder' }))
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Check this change. @Release guide list concrete blockers.')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))

    await waitFor(() => expect(agentApi.createTask).toHaveBeenCalledWith(expect.objectContaining({
      agentId: 'agent-1', instructions: 'Check this change. list concrete blockers.', cwd: '/project',
      acceptanceCriteria: [expect.any(String)],
    })))
    expect(agentApi.createTask.mock.calls[0][0]).not.toHaveProperty('sourceThreadId')
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
  })

  it('switches into planning mode with /plan and sends the command arguments as the prompt', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    backend.chooseWorkspace.mockResolvedValueOnce('/project')
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Choose project folder'}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, '/plan inspect the authentication flow')
    await userEvent.keyboard('{Enter}')

    await waitFor(() => expect(backend.send).toHaveBeenLastCalledWith('thread-1', 'inspect the authentication flow', 'gpt-6.1-sol', 'medium', 'plan'))
    expect(screen.getByRole('button', {name:'Plan mode'})).toHaveAttribute('aria-pressed', 'true')
  })

  it('uses standalone /plan as a mode toggle without sending a turn', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    backend.chooseWorkspace.mockResolvedValueOnce('/project')
    const sendsBefore = backend.send.mock.calls.length
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Choose project folder'}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, '/plan{enter}')

    expect(screen.getByRole('button', {name:'Plan mode'})).toHaveAttribute('aria-pressed', 'true')
    expect(message).toHaveValue('')
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
  })

  it('restores a saved conversation mode and preserves the draft while Shift+Tab toggles it', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1, collaborationMode:{mode:'plan'}, turns:[]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Plan thread'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-plan'))
    const modeToggle = screen.getByRole('button', {name:'Plan mode'})
    expect(modeToggle).toHaveAttribute('aria-pressed', 'true')

    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Keep this draft')
    await userEvent.keyboard('{Shift>}{Tab}{/Shift}')

    expect(modeToggle).toHaveAttribute('aria-pressed', 'false')
    expect(message).toHaveValue('Keep this draft')

    act(() => events.handler?.({method:'thread/settings/updated', params:{threadId:'thread-plan', threadSettings:{collaborationMode:{mode:'plan'}}}}))
    expect(modeToggle).toHaveAttribute('aria-pressed', 'true')
  })

  it('waits for explicit approval before implementing the current proposed plan', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1, collaborationMode:{mode:'plan'}, turns:[]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Plan thread'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-plan'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-plan', turn:{id:'turn-plan', status:'inProgress', items:[]}}}))
    act(() => events.handler?.({method:'item/plan/delta', params:{threadId:'thread-plan', turnId:'turn-plan', itemId:'plan-item', delta:'1. Inspect the current flow'}}))
    act(() => events.handler?.({method:'item/completed', params:{threadId:'thread-plan', turnId:'turn-plan', item:{id:'plan-item', type:'plan', text:'1. Inspect the current flow'}}}))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-plan', turn:{id:'turn-plan', status:'completed', items:[{id:'plan-item', type:'plan', text:'1. Inspect the current flow'}]}}}))

    const implement = await screen.findByRole('button', {name:'Implement plan'})
    expect(implement).toBeEnabled()
    expect(backend.send).not.toHaveBeenCalledWith('thread-plan', 'Implement the plan.', 'gpt-6.1-sol', 'medium', 'default')
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Unrelated draft')
    expect(implement).toBeDisabled()
    await userEvent.clear(message)
    await userEvent.click(implement)

    await waitFor(() => expect(backend.send).toHaveBeenLastCalledWith('thread-plan', 'Implement the plan.', 'gpt-6.1-sol', 'medium', 'default'))
  })

  it('does not offer implementation for an unfinished streamed plan', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-plan', cwd:'/project', preview:'Plan thread', updatedAt:1, collaborationMode:{mode:'plan'}, turns:[]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Plan thread'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-plan'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-plan', turn:{id:'turn-plan', status:'inProgress', items:[]}}}))
    act(() => events.handler?.({method:'item/plan/delta', params:{threadId:'thread-plan', turnId:'turn-plan', itemId:'plan-item', delta:'1. Inspect the current flow'}}))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-plan', turn:{id:'turn-plan', status:'completed', items:[]}}}))

    expect(screen.queryByRole('button', {name:'Implement plan'})).not.toBeInTheDocument()
  })

  it('locks collaboration mode while a turn is running, including Shift+Tab', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const modeToggle = screen.getByRole('button', {name:'Plan mode'})
    expect(modeToggle).toBeDisabled()

    fireEvent.keyDown(screen.getByRole('textbox', {name:'Message'}), {key:'Tab', shiftKey:true})

    expect(modeToggle).toHaveAttribute('aria-pressed', 'false')
  })

  it('keeps /plan commands as a draft while a turn is running', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, '/plan continue with a different approach{enter}')

    expect(await screen.findByRole('alert')).toHaveTextContent('Wait for the current turn to finish')
    expect(message).toHaveValue('/plan continue with a different approach')
    expect(backend.steer).not.toHaveBeenCalledWith('thread-1', '/plan continue with a different approach', 'turn-active')
  })

  it('keeps plan mode and the message draft when a new conversation send fails', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    backend.chooseWorkspace.mockResolvedValueOnce('/project')
    backend.send.mockRejectedValueOnce(new Error('turn start failed'))
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Choose project folder'}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Plan a safe change')
    await userEvent.click(screen.getByRole('button', {name:'Plan mode'}))
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))

    expect(await screen.findByRole('alert')).toHaveTextContent('turn start failed')
    expect(screen.getByRole('button', {name:'Plan mode'})).toHaveAttribute('aria-pressed', 'true')
    expect(message).toHaveValue('Plan a safe change')
  })

  it('does not show a late send failure in a different conversation', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[
      {id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1},
      {id:'thread-2', cwd:'/project', preview:'Thread two', updatedAt:2},
    ]} as never)
    let rejectSend!: (reason: Error) => void
    backend.send.mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectSend = reject }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Send in the first chat')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('thread-1', 'Send in the first chat', 'gpt-6.1-sol', 'medium', 'default'))
    await userEvent.click(screen.getByRole('button', {name:'Thread two'}))

    await act(async () => rejectSend(new Error('late send failure')))

    expect(screen.getByRole('button', {name:'Thread two'})).toHaveClass('selected')
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('offers only ChatGPT sign-in and prevents sending until signed in', async () => {
    render(<App />)
    const login = await screen.findByRole('button', { name: 'Sign in with ChatGPT' })
    expect(screen.getByRole('button', { name: 'Send message' })).toBeDisabled()
    expect(screen.queryByText(/API key/i)).not.toBeInTheDocument()
    await userEvent.click(login)
    expect(backend.login).toHaveBeenCalledOnce()
    expect(await screen.findByText(/Complete sign-in in your browser/)).toBeInTheDocument()
  })

  it('clears pending sign-in and reports a failed login completion', async () => {
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Sign in with ChatGPT'}))
    expect(await screen.findByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeDisabled()
    act(() => events.handler?.({method:'account/login/completed', params:{loginId:'login-1', success:false, error:'Browser sign-in was cancelled', onboardingEntrypoint:null}}))
    expect(await screen.findByRole('alert')).toHaveTextContent('Browser sign-in was cancelled')
    expect(screen.queryByText(/Complete sign-in in your browser/)).not.toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeEnabled()
  })

  it('cancels the active ChatGPT sign-in and restores the sign-in action', async () => {
    let finishCancel!: (result: unknown) => void
    const cancellationsBefore = backend.cancelLogin.mock.calls.length
    backend.cancelLogin.mockImplementationOnce(() => new Promise(resolve => { finishCancel = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Sign in with ChatGPT'}))
    expect(await screen.findByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    await waitFor(() => expect(backend.cancelLogin).toHaveBeenCalledWith('login-1'))
    const cancelling = screen.getByRole('button', {name:'Cancelling…'})
    expect(cancelling).toBeDisabled()
    await userEvent.click(cancelling)
    expect(backend.cancelLogin.mock.calls.length).toBe(cancellationsBefore + 1)
    await act(async () => finishCancel({status:'canceled'}))
    expect(screen.queryByText(/Complete sign-in in your browser/)).not.toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeEnabled()
  })

  it('keeps the active login available to retry when cancellation fails', async () => {
    backend.cancelLogin.mockRejectedValueOnce(new Error('Could not cancel sign-in'))
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Sign in with ChatGPT'}))
    await screen.findByText(/Complete sign-in in your browser/)
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not cancel sign-in')
    expect(screen.getByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeDisabled()
    expect(screen.getByRole('button', {name:'Cancel sign-in'})).toBeEnabled()
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    await waitFor(() => expect(backend.cancelLogin).toHaveBeenLastCalledWith('login-1'))
    expect(screen.queryByText(/Complete sign-in in your browser/)).not.toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeEnabled()
  })

  it('ignores a late completion for a cancelled login while a retry is starting', async () => {
    let finishRetry!: (result: {loginId:string}) => void
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Sign in with ChatGPT'}))
    await screen.findByText(/Complete sign-in in your browser/)
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    await waitFor(() => expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeEnabled())
    backend.login.mockImplementationOnce(() => new Promise(resolve => { finishRetry = resolve }) as never)
    await userEvent.click(screen.getByRole('button', {name:'Sign in with ChatGPT'}))
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeDisabled()
    act(() => events.handler?.({method:'account/login/completed', params:{loginId:'login-1', success:false, error:'Cancelled', onboardingEntrypoint:null}}))
    expect(screen.getByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Sign in with ChatGPT'})).toBeDisabled()
    await act(async () => finishRetry({loginId:'login-2'}))
    expect(screen.getByRole('button', {name:'Cancel sign-in'})).toBeEnabled()
  })

  it('accepts login completion when cancel reports the attempt already finished', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[]} as never)
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[], threads:[],
    } as never)
    backend.cancelLogin.mockResolvedValueOnce({status:'notFound'} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Sign in with ChatGPT'}))
    await screen.findByText(/Complete sign-in in your browser/)
    await userEvent.click(screen.getByRole('button', {name:'Cancel sign-in'}))
    await waitFor(() => expect(backend.cancelLogin).toHaveBeenCalledWith('login-1'))
    act(() => events.handler?.({method:'account/login/completed', params:{loginId:'login-1', success:true, onboardingEntrypoint:null}}))
    expect(await screen.findByText('dev@example.com')).toBeInTheDocument()
  })

  it('follows streamed transcript near the bottom but preserves a scrolled-up reading position', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'inProgress', items:[{id:'message-1', type:'agentMessage', text:'Answer'}]},
    ]}} as never)
    const {container} = render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await screen.findByText('Answer')
    const scroll = container.querySelector('.conversation-scroll') as HTMLDivElement
    Object.defineProperties(scroll, {
      clientHeight: {configurable:true, value:300},
      scrollHeight: {configurable:true, value:1000},
      scrollTop: {configurable:true, writable:true, value:700},
    })
    fireEvent.scroll(scroll)
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:' next'}}))
    expect(await screen.findByText('Answer next')).toBeInTheDocument()
    expect(scroll.scrollTop).toBe(1000)
    scroll.scrollTop = 100
    fireEvent.scroll(scroll)
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:' last'}}))
    expect(await screen.findByText('Answer next last')).toBeInTheDocument()
    expect(scroll.scrollTop).toBe(100)
  })

  it('refreshes account and models after successful sign-in', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[]} as never)
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'},
      models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}],
      threads:[],
    } as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Sign in with ChatGPT'}))
    act(() => events.handler?.({method:'account/login/completed', params:{loginId:'login-1', success:true, onboardingEntrypoint:null}}))
    expect(await screen.findByText('dev@example.com')).toBeInTheDocument()
    expect(screen.getByRole('combobox', {name:'Model'})).toHaveValue('sol')
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Start now that I am signed in')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.send).toHaveBeenCalledWith('thread-1', 'Start now that I am signed in', 'gpt-6.1-sol', 'medium', 'default'))
  })

  it('refreshes a revoked account without removing active chats', async () => {
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[],
      threads:[{id:'thread-1', cwd:'/project', preview:'Existing chat', updatedAt:1}],
    } as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Existing chat', updatedAt:1, turns:[
      {id:'turn-1', status:'completed', items:[{id:'message-1', type:'agentMessage', text:'Existing answer'}]},
    ]}} as never)
    backend.account.mockResolvedValueOnce(null)
    render(<App />)
    await screen.findByText('dev@example.com')
    await userEvent.click(await screen.findByRole('button', {name:'Existing chat'}))
    expect(await screen.findByText('Existing answer')).toBeInTheDocument()
    act(() => events.handler?.({method:'account/updated', params:{authMode:null, planType:null}}))
    await waitFor(() => expect(screen.queryByText('dev@example.com')).not.toBeInTheDocument())
    expect(screen.getByRole('button', {name:'Existing chat'})).toBeInTheDocument()
    expect(screen.getByText('Existing answer')).toBeInTheDocument()
  })

  it('starts a coding turn in the chosen folder with the selected model', async () => {
    backend.connect.mockResolvedValueOnce({account: {type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    render(<App />)
    await screen.findByText('dev@example.com')
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Fix the failing tests')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(backend.start).toHaveBeenCalledWith('/project', 'gpt-6.1-sol', false)
    expect(backend.send).toHaveBeenCalledWith('thread-1', 'Fix the failing tests', 'gpt-6.1-sol', 'medium', 'default')
  })

  it('labels a new conversation with its first message after a successful send', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    render(<App />)
    await screen.findByText('dev@example.com')
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'HelloWorld')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(await screen.findByRole('button', {name:'HelloWorld'})).toBeInTheDocument()
  })

  it('keeps the first-message label on follow-up messages', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    const sendsBefore = backend.send.mock.calls.length
    render(<App />)
    await screen.findByText('dev@example.com')
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'First message')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(await screen.findByRole('button', {name:'First message'})).toBeInTheDocument()
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-1', status:'completed', items:[]}}}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Follow-up message')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.send.mock.calls.length).toBe(sendsBefore + 2))
    expect(screen.getByRole('button', {name:'First message'})).toBeInTheDocument()
    expect(screen.queryByRole('button', {name:'Follow-up message'})).not.toBeInTheDocument()
  })

  it('does not rename a new chat when its first send fails', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    backend.send.mockRejectedValueOnce(new Error('send failed'))
    render(<App />)
    await screen.findByText('dev@example.com')
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Failed first message')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(await screen.findByRole('alert')).toHaveTextContent('send failed')
    expect(screen.getByRole('button', {name:'New chat'})).toBeInTheDocument()
    expect(screen.queryByRole('button', {name:'Failed first message'})).not.toBeInTheDocument()
  })

  it('preserves an existing thread preview after sending', async () => {
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'},
      models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}],
      threads:[{id:'thread-1', cwd:'/project', preview:'Existing preview', updatedAt:1}],
    } as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Existing preview', updatedAt:1, turns:[]}} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Existing preview'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Do not replace the preview')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    expect(await screen.findByRole('button', {name:'Existing preview'})).toBeInTheDocument()
    expect(screen.queryByRole('button', {name:'Do not replace the preview'})).not.toBeInTheDocument()
  })

  it('keeps a first-message preview when a quick resume returns empty metadata', async () => {
    backend.connect.mockResolvedValueOnce({
      account:{type:'chatgpt', email:'dev@example.com', planType:'plus'},
      models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}],
      threads:[],
    } as never)
    backend.start.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'', updatedAt:1}} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'', updatedAt:2, turns:[]}} as never)
    render(<App />)
    await screen.findByText('dev@example.com')
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'First message preview')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    const chat = await screen.findByRole('button', {name:'First message preview'})

    await userEvent.click(chat)
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))

    expect(await screen.findByRole('button', {name:'First message preview'})).toBeInTheDocument()
  })

  it('shows request-user-input choices and submits answers keyed by question id', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'},
      models: [{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({
      method: 'item/tool/requestUserInput',
      id: 'request-1',
      params: {
        threadId: 'thread-1', turnId: 'turn-1', itemId: 'item-1', isBlocking: true,
        questions: [{id:'scope', header:'Scope', question:'Where should I work?', isOther:false, isSecret:false, options:[{label:'Project folder', description:'Use the selected folder'}, {label:'Another folder', description:'Choose a different folder'}]}],
      },
    }))

    expect(await screen.findByRole('button', {name:/Project folder/})).toBeInTheDocument()
    expect(screen.getByRole('textbox', {name:'Other: Where should I work?'})).toBeInTheDocument()
    expect(screen.getByRole('button', {name:'Submit answers'})).toBeDisabled()
    await userEvent.click(screen.getByRole('button', {name:/Project folder/}))
    expect(screen.getByRole('button', {name:/Another folder/})).toHaveAttribute('aria-pressed', 'false')
    await userEvent.type(screen.getByRole('textbox', {name:'Other: Where should I work?'}), 'include tests')
    expect(screen.getByRole('button', {name:'Submit answers'})).toBeEnabled()
    await userEvent.click(screen.getByRole('button', {name:'Submit answers'}))
    await waitFor(() => expect(backend.respond).toHaveBeenCalledWith('request-1', {answers:{scope:{answers:['include tests']}}}))
  })

  it('shows requested permissions and can allow once or decline for this turn', async () => {
    backend.connect.mockResolvedValueOnce({
      account: {type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[],
      threads: [{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}],
    } as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[]}})
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    const requested = {network:{enabled:true}, fileSystem:{read:['/project']}}
    act(() => events.handler?.({method:'item/permissions/requestApproval', id:91, params:{threadId:'thread-1', turnId:'turn-1', itemId:'item-1', permissions:requested, cwd:'/project'}}))

    expect(await screen.findByText('Permissions requested')).toBeInTheDocument()
    expect(screen.getByText(/\/project/)).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Allow once'}))
    await waitFor(() => expect(backend.respond).toHaveBeenCalledWith(91, {permissions:requested, scope:'turn'}))

    act(() => events.handler?.({method:'item/permissions/requestApproval', id:92, params:{threadId:'thread-1', turnId:'turn-1', itemId:'item-2', permissions:requested, cwd:'/project'}}))
    await userEvent.click(await screen.findByRole('button', {name:'Decline'}))
    await waitFor(() => expect(backend.respond).toHaveBeenCalledWith(92, {permissions:{}, scope:'turn'}))
  })

  it('responds to command and file approvals with their decision values', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/commandExecution/requestApproval', id:31, params:{threadId:'thread-1', turnId:'turn-1', itemId:'cmd-1', command:'pnpm test'}}))
    expect(await screen.findByText('Command approval requested')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Allow'}))
    await waitFor(() => expect(backend.respond).toHaveBeenCalledWith(31, {decision:'accept'}))
    act(() => events.handler?.({method:'item/fileChange/requestApproval', id:'file-request', params:{threadId:'thread-1', turnId:'turn-1', itemId:'file-1', reason:'Write a source file'}}))
    expect(await screen.findByText('File change approval requested')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', {name:'Decline'}))
    await waitFor(() => expect(backend.respond).toHaveBeenCalledWith('file-request', {decision:'decline'}))
  })

  it('loads thread history when background streaming created a partial session and keeps newer deltas', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await screen.findByRole('button', {name:'Thread one'})
    await waitFor(() => expect(events.handler).toBeTypeOf('function'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-2', itemId:'live-message', delta:'live one'}}))
    await userEvent.click(screen.getByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-2', itemId:'live-message', delta:' live two'}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'completed', items:[{id:'history-message', type:'agentMessage', text:'Past answer'}]},
      {id:'turn-2', status:'inProgress', items:[{id:'live-message', type:'agentMessage', text:'snapshot value'}]},
    ]}}))

    expect(await screen.findByText('Past answer')).toBeInTheDocument()
    expect(screen.getByText('live one live two')).toBeInTheDocument()
  })

  it('does not truncate text already included in a resume snapshot', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:'Hello'}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'inProgress', items:[{id:'message-1', type:'agentMessage', text:'Hello world'}]},
    ]}}))
    expect(await screen.findByText('Hello world')).toBeInTheDocument()
  })

  it('does not replace snapshot text with a streamed suffix', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:' world'}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'inProgress', items:[{id:'message-1', type:'agentMessage', text:'Hello world'}]},
    ]}}))
    expect(await screen.findByText('Hello world')).toBeInTheDocument()
  })

  it('joins overlapping live text onto the hydrated snapshot', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-1', itemId:'message-1', delta:'world!'}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'inProgress', items:[{id:'message-1', type:'agentMessage', text:'Hello world'}]},
    ]}}))
    expect(await screen.findByText('Hello world!')).toBeInTheDocument()
  })

  it('does not revive a completed turn from a buffered start event', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-1', status:'inProgress', items:[]}}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'completed', items:[]},
    ]}}))
    expect(screen.queryByRole('button', {name:'Stop'})).not.toBeInTheDocument()
  })

  it('keeps completed item status when stale in-progress events arrive during resume', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'item/started', params:{threadId:'thread-1', turnId:'turn-1', item:{id:'command-1', type:'commandExecution', status:'inProgress', command:'pnpm test'}}}))
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[
      {id:'turn-1', status:'completed', items:[{id:'command-1', type:'commandExecution', status:'completed', command:'pnpm test', exitCode:0}]},
    ]}}))
    expect(await screen.findByText('Command · completed')).toBeInTheDocument()
  })

  it('starts read-only work and resumes without overriding the thread sandbox', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    render(<App />)
    await screen.findByText('dev@example.com')
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    await userEvent.click(screen.getByRole('checkbox', {name:'Read only'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Review this project')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith('/project', 'gpt-6.1-sol', true))
    await userEvent.click(await screen.findByRole('button', {name:'Review this project'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    expect(backend.resume).toHaveBeenLastCalledWith('thread-1')
  })

  it('keeps a completed turn complete when its send response arrives later', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    let finishSend!: (response: unknown) => void
    backend.send.mockImplementationOnce(() => new Promise(resolve => { finishSend = resolve }) as never)
    render(<App />)
    await screen.findByText('dev@example.com')
    const sendsBefore = backend.send.mock.calls.length
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Make a change')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.send).toHaveBeenCalled())
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-fast', status:'completed', error:null, items:[{id:'answer-fast', type:'agentMessage', text:'Finished'}]}}}))
    await screen.findByText('Finished')
    await act(async () => finishSend({turn:{id:'turn-fast'}}))
    expect(screen.queryByRole('button', {name:'Stop'})).not.toBeInTheDocument()
    expect(backend.send.mock.calls.length).toBe(sendsBefore + 1)
  })

  it('shows engine disconnect state and offers a reconnect action', async () => {
    render(<App />)
    await waitFor(() => expect(events.handler).toBeTypeOf('function'))
    act(() => events.handler?.({method:'brr/disconnected', params:{reason:'engine stopped'}}))
    expect(await screen.findByRole('button', {name:'Reconnect'})).toBeInTheDocument()
    expect(screen.getByRole('status')).toHaveTextContent(/coding engine disconnected/i)
    const callsBeforeRetry = backend.connect.mock.calls.length
    await userEvent.click(screen.getByRole('button', {name:'Reconnect'}))
    await waitFor(() => expect(backend.connect.mock.calls.length).toBe(callsBeforeRetry + 1))
  })

  it('allows sign-in to retry after disconnecting during login', async () => {
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Sign in with ChatGPT'}))
    expect(await screen.findByText(/Complete sign-in in your browser/)).toBeInTheDocument()
    act(() => events.handler?.({method:'brr/disconnected', params:{reason:'engine stopped'}}))
    await userEvent.click(await screen.findByRole('button', {name:'Reconnect'}))
    const login = await screen.findByRole('button', {name:'Sign in with ChatGPT'})
    await waitFor(() => expect(login).toBeEnabled())
  })

  it('reloads the active thread after reconnecting during a running turn', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[]}})
    backend.resume.mockResolvedValueOnce({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[{id:'turn-live', status:'completed', items:[]}]}} as never)
    render(<App />)
    const resumesBefore = backend.resume.mock.calls.length
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume.mock.calls.length).toBe(resumesBefore + 1))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-live', status:'inProgress', items:[]}}}))
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
    act(() => events.handler?.({method:'brr/disconnected', params:{reason:'engine stopped'}}))
    await userEvent.click(await screen.findByRole('button', {name:'Reconnect'}))
    await waitFor(() => expect(backend.resume.mock.calls.length).toBe(resumesBefore + 2))
    expect(backend.resume).toHaveBeenLastCalledWith('thread-1')
    expect(screen.queryByRole('button', {name:'Stop'})).not.toBeInTheDocument()
  })

  it('does not connect or subscribe in browser preview', async () => {
    backend.desktop = false
    const connectsBefore = backend.connect.mock.calls.length
    const subscribesBefore = backend.subscribe.mock.calls.length
    render(<App />)
    expect(await screen.findByText(/Connect from the desktop app/)).toBeInTheDocument()
    expect(backend.connect).toHaveBeenCalledTimes(connectsBefore)
    expect(backend.subscribe).toHaveBeenCalledTimes(subscribesBefore)
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
    backend.desktop = true
  })

  it('does not submit an IME composition when Enter is pressed', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[]} as never)
    render(<App />)
    await screen.findByText('dev@example.com')
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, '日本語')
    const sendsBefore = backend.send.mock.calls.length
    fireEvent.keyDown(message, {key:'Enter', code:'Enter', isComposing:true})
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
  })

  it('keeps a blocking approval visible when activity is turned off', async () => {
    backend.connect.mockResolvedValueOnce({account:null, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    await userEvent.click(screen.getByRole('button', {name:'Activity on'}))
    act(() => events.handler?.({method:'item/commandExecution/requestApproval', id:'blocking-approval', params:{threadId:'thread-1', turnId:'turn-1', itemId:'command-1', command:'pnpm test', cwd:'/project', reason:'Run the project tests with network access'}}))
    expect(await screen.findByText('Command approval requested')).toBeInTheDocument()
    expect(screen.getByText('pnpm test')).toBeInTheDocument()
    expect(screen.getByText('/project')).toBeInTheDocument()
    expect(screen.getByText('Run the project tests with network access')).toBeInTheDocument()
  })

  it('keeps Send disabled while thread history is loading', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishResume!: (response: unknown) => void
    backend.resume.mockImplementationOnce(() => new Promise(resolve => { finishResume = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Wait for history')
    expect(screen.getByRole('button', {name:'Send message'})).toBeDisabled()
    await act(async () => finishResume({thread:{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1, turns:[]}}))
  })

  it('starts a new thread when the project folder changes from an existing thread', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'old-thread', cwd:'/existing', preview:'Existing', updatedAt:1}]} as never)
    backend.chooseWorkspace.mockResolvedValueOnce('/other')
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Existing'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('old-thread'))
    await userEvent.click(screen.getByRole('button', {name:'Choose project folder'}))
    await userEvent.type(screen.getByRole('textbox', {name:'Message'}), 'Use the new project')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith('/other', 'gpt-6.1-sol', false))
  })

  it('steers the active turn when Enter is pressed and keeps Stop available', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    backend.steer.mockResolvedValueOnce({turnId:'turn-live'}).mockResolvedValueOnce({turnId:'turn-live'})
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    await waitFor(() => expect(events.handler).toBeTypeOf('function'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-live', status:'inProgress', items:[]}}}))
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Please adjust the current work{enter}')
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Please adjust the current work', 'turn-live'))
    expect(backend.start.mock.calls.length).toBe(startsBefore)
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
    act(() => events.handler?.({method:'item/started', params:{
      threadId:'thread-1', turnId:'turn-live',
      item:{id:'steered-user-message', type:'userMessage', content:[{type:'text', text:'Please adjust the current work'}]},
    }}))
    expect(await screen.findByText('Please adjust the current work')).toBeInTheDocument()

    await userEvent.type(message, 'Use the click control')
    await userEvent.click(screen.getByRole('button', {name:'Send message'}))
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Use the click control', 'turn-live'))
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
  })

  it('keeps Stop usable during steering and preserves a newer draft when steering resolves', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishSteer!: (response: {turnId:string}) => void
    backend.steer.mockImplementationOnce(() => new Promise(resolve => { finishSteer = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Steer this turn{enter}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Steer this turn', 'turn-active'))

    const stop = screen.getByRole('button', {name:'Stop'})
    expect(stop).toBeEnabled()
    await userEvent.click(stop)
    await waitFor(() => expect(backend.interrupt).toHaveBeenLastCalledWith('thread-1', 'turn-active'))
    await userEvent.clear(message)
    await userEvent.type(message, 'A newer draft')
    await act(async () => finishSteer({turnId:'turn-active'}))

    expect(message).toHaveValue('A newer draft')
    expect(screen.getByRole('button', {name:'Stop'})).toBeInTheDocument()
  })

  it('clears an acknowledged steer when its turn completes before the reply', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishSteer!: (response: {turnId:string}) => void
    backend.steer.mockImplementationOnce(() => new Promise(resolve => { finishSteer = resolve }) as never)
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Keep going with the current approach{enter}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Keep going with the current approach', 'turn-active'))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-active', status:'completed', items:[]}}}))

    await act(async () => finishSteer({turnId:'turn-active'}))

    expect(message).toHaveValue('')
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    expect(backend.start.mock.calls.length).toBe(startsBefore)
  })

  it('does not start another turn on Enter while a completed turn steer is pending', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let finishSteer!: (response: {turnId:string}) => void
    backend.steer.mockImplementationOnce(() => new Promise(resolve => { finishSteer = resolve }) as never)
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Continue after completion{enter}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Continue after completion', 'turn-active'))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-active', status:'completed', items:[]}}}))

    await userEvent.type(message, '{enter}')

    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    expect(backend.start.mock.calls.length).toBe(startsBefore)
    expect(message).toHaveValue('Continue after completion')
    await act(async () => finishSteer({turnId:'turn-active'}))
  })

  it('shows a late no-active-turn rejection after completion and keeps its draft', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let rejectSteer!: (reason: Error) => void
    backend.steer.mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectSteer = reject }) as never)
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Keep this for a new turn{enter}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Keep this for a new turn', 'turn-active'))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-active', status:'completed', items:[]}}}))

    await act(async () => rejectSteer(new Error('no active turn')))

    expect(await screen.findByRole('alert')).toHaveTextContent('no active turn')
    expect(message).toHaveValue('Keep this for a new turn')
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    expect(backend.start.mock.calls.length).toBe(startsBefore)
  })

  it('does not surface a late steer error over a newer draft', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    let rejectSteer!: (reason: Error) => void
    backend.steer.mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectSteer = reject }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Submitted instruction{enter}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Submitted instruction', 'turn-active'))
    await userEvent.clear(message)
    await userEvent.type(message, 'A newer instruction')

    await act(async () => rejectSteer(new Error('no active turn')))

    expect(message).toHaveValue('A newer instruction')
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('does not clear another thread draft when a pending steer resolves', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[
      {id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1},
      {id:'thread-2', cwd:'/project', preview:'Thread two', updatedAt:2},
    ]} as never)
    let finishSteer!: (response: {turnId:string}) => void
    backend.steer.mockImplementationOnce(() => new Promise(resolve => { finishSteer = resolve }) as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-one', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Steer thread one{enter}')
    await waitFor(() => expect(backend.steer).toHaveBeenLastCalledWith('thread-1', 'Steer thread one', 'turn-one'))

    await userEvent.click(screen.getByRole('button', {name:'Thread two'}))
    await userEvent.clear(message)
    await userEvent.type(message, 'Draft for thread two')
    await act(async () => finishSteer({turnId:'turn-one'}))

    expect(screen.getByRole('textbox', {name:'Message'})).toHaveValue('Draft for thread two')
    expect(screen.getByRole('button', {name:'Thread two'})).toHaveClass('selected')
  })

  it('keeps a failed steering draft and never falls back to a new turn', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[{id:'sol', model:'gpt-6.1-sol', displayName:'GPT Sol', isDefault:true, defaultReasoningEffort:'medium', supportedReasoningEfforts:[]}], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    backend.steer.mockRejectedValueOnce(new Error('active turn changed'))
    const sendsBefore = backend.send.mock.calls.length
    const startsBefore = backend.start.mock.calls.length
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-active', status:'inProgress', items:[]}}}))
    const message = screen.getByRole('textbox', {name:'Message'})
    await userEvent.type(message, 'Keep this instruction{enter}')

    expect(await screen.findByRole('alert')).toHaveTextContent('active turn changed')
    expect(message).toHaveValue('Keep this instruction')
    expect(backend.send.mock.calls.length).toBe(sendsBefore)
    expect(backend.start.mock.calls.length).toBe(startsBefore)
  })

  it('keeps Stop targeted at the active turn after a prior turn delta arrives late', async () => {
    backend.connect.mockResolvedValueOnce({account:{type:'chatgpt', email:'dev@example.com', planType:'plus'}, models:[], threads:[{id:'thread-1', cwd:'/project', preview:'Thread one', updatedAt:1}]} as never)
    render(<App />)
    await userEvent.click(await screen.findByRole('button', {name:'Thread one'}))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('thread-1'))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-old', status:'inProgress', items:[]}}}))
    act(() => events.handler?.({method:'turn/completed', params:{threadId:'thread-1', turn:{id:'turn-old', status:'interrupted', items:[]}}}))
    act(() => events.handler?.({method:'turn/started', params:{threadId:'thread-1', turn:{id:'turn-current', status:'inProgress', items:[]}}}))

    act(() => events.handler?.({method:'item/agentMessage/delta', params:{threadId:'thread-1', turnId:'turn-old', itemId:'old-message', delta:'late old text'}}))
    await userEvent.click(screen.getByRole('button', {name:'Stop'}))

    await waitFor(() => expect(backend.interrupt).toHaveBeenLastCalledWith('thread-1', 'turn-current'))
  })
})
