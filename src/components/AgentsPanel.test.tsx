import { beforeEach, describe, expect, it, vi } from 'vitest'
import { act, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

const api = vi.hoisted(() => ({
  list: vi.fn(), settings: vi.fn(), decisions: vi.fn(), drafts: vi.fn(), interview: vi.fn(), approve: vi.fn(), reject: vi.fn(),
  sessions: vi.fn(), open: vi.fn(), tasks: vi.fn(), createTask: vi.fn(), proposals: vi.fn(), approveProposal: vi.fn(), scheduleProposal: vi.fn(), workflows: vi.fn(), memory: vi.fn(), skills: vi.fn(),
  updateSettings: vi.fn(), projects: vi.fn(), searchMemory: vi.fn(), forgetMemory: vi.fn(), undoMemory: vi.fn(), inspectWorkspace: vi.fn(),
}))
vi.mock('../lib/agents', () => ({ agents: api }))
const ticketApi = vi.hoisted(() => ({ proposals: vi.fn(), approveRun: vi.fn(), rejectRun: vi.fn() }))
vi.mock('../lib/tickets', async () => ({ ...(await vi.importActual('../lib/tickets')), tickets: ticketApi, ticketOperationId: () => 'stable-operation-id' }))
const runtime = vi.hoisted(() => ({ approveWorktree: vi.fn(async () => undefined), inspectProject: vi.fn(async (_id: string | null) => ({commands: [] as any[]})), onEvent: null as null | ((event: { method: string }) => void) }))
vi.mock('../lib/client', () => ({ client: { subscribe: vi.fn(async (handler: (event: { method: string }) => void) => { runtime.onEvent = handler; return () => { runtime.onEvent = null } }), approveWorktree: runtime.approveWorktree, inspectProject: runtime.inspectProject } }))

import { AgentsPanel } from './AgentsPanel'

const model = { id: 'model-1', model: 'gpt-5', displayName: 'GPT-5', isDefault: true, supportedReasoningEfforts: [], defaultReasoningEffort: 'medium' }
const thread = { id: 'thread-interview', name: 'New agent interview', preview: '', cwd: '/workspace', updatedAt: 1 }
const candidate = { name: 'Release guide', description: 'Helps ship safely', instructions: 'Review changes and release risks.', soul: 'Calm, direct, evidence-led.', model: 'gpt-5', effort: 'medium' }

describe('AgentsPanel', () => {
  beforeEach(() => {
    Object.values(api).forEach(mock => mock.mockReset())
    Object.values(ticketApi).forEach(mock => mock.mockReset())
    ticketApi.proposals.mockResolvedValue({ data: [] })
    api.list.mockResolvedValue({ data: [] })
    api.settings.mockResolvedValue({ maxBackground: 4, maxPerAgent: 2 })
    api.decisions.mockResolvedValue({ data: [] })
    api.drafts.mockResolvedValue({ data: [] })
    api.tasks.mockResolvedValue({ data: [] })
    api.proposals.mockResolvedValue({ data: [] })
    api.workflows.mockResolvedValue({ data: [] })
    api.memory.mockResolvedValue({ data: [] })
    api.skills.mockResolvedValue({ data: [] })
    api.sessions.mockResolvedValue({ data: [] })
    api.createTask.mockResolvedValue({ id: 'task-1', status: 'queued' })
    api.updateSettings.mockImplementation(async settings => settings)
    api.projects.mockResolvedValue({ data: [] })
    api.searchMemory.mockResolvedValue({ data: [], sources: [] })
    api.forgetMemory.mockResolvedValue({ eventId: 7 })
    api.undoMemory.mockResolvedValue({})
    api.inspectWorkspace.mockImplementation(async cwd => ({ cwd, git: false, commands: [] }))
    runtime.approveWorktree.mockClear()
    runtime.onEvent = null
  })

  it('starts a conversational interview and reviews the exact candidate before creation', async () => {
    const user = userEvent.setup()
    const onOpenThread = vi.fn()
    api.interview.mockResolvedValue({ thread })
    api.drafts.mockResolvedValue({ data: [{ id: 'draft-1', threadId: thread.id, candidate, hash: 'sha256:abc', status: 'proposed' }] })
    api.approve.mockResolvedValue({ agent: { id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 } })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={onOpenThread} onClose={vi.fn()} />)

    await user.click(screen.getAllByRole('button', { name: 'New agent' })[0])
    await waitFor(() => expect(api.interview).toHaveBeenCalledWith('gpt-5', 'medium'))
    expect(onOpenThread).toHaveBeenCalledWith(thread, undefined, { fresh: true })
    expect(await screen.findByRole('region', { name: 'Review agent profile' })).toHaveTextContent(candidate.instructions)
    await user.click(screen.getByRole('button', { name: 'Create agent' }))

    await waitFor(() => expect(api.approve).toHaveBeenCalledWith('draft-1', 'sha256:abc'))
    expect(await screen.findAllByRole('heading', { name: 'Release guide' })).toHaveLength(2)
  })

  it('shows pending questions without requiring named profiles and opens the matching question in its conversation', async () => {
    const onOpenThread = vi.fn()
    const question = { id: 'question-7', threadId: 'question-thread', turnId: 'turn-2', itemId: 'tool-3', agentId: 'agent-7', taskId: 'task-7', kind: 'blocking' as const,
      questions: [{ id: 'scope', header: 'Scope', question: 'Which scope?', options: null }], draft: {}, resolution: 'pending' as const, delivery: 'none' as const, createdAt: 1, updatedAt: 3 }
    api.decisions.mockResolvedValue({ data: [], questions: [question] })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={onOpenThread} onClose={vi.fn()} />)

    const section = await screen.findByRole('region', { name: 'Pending questions' })
    expect(section).toHaveTextContent('Scope')
    await userEvent.click(within(section).getByRole('button', { name: 'Open question' }))

    expect(onOpenThread).toHaveBeenCalledWith(expect.objectContaining({ id: 'question-thread' }), 'agent-7', { readOnly: true, questionId: 'question-7' })
  })

  it('reviews ticket generated run proposals in the agent decisions surface', async () => {
    const proposal = { id: 'proposal-1', ticketId: 'ticket-1', ticketRevision: 3, hash: 'ticket-hash', status: 'proposed', createdAt: 1, snapshot: { agentId: 'agent-1', title: 'Review login flow', instructions: 'Inspect sign in.', acceptanceCriteria: ['Report failed paths'], projectId: 'project-1', cwd: '/reviewed/root-two', context: 'Execute ticket BRR-000008\nBrr project snapshot: {"id":"project-1","folders":["/reviewed/root-one","/reviewed/root-two"],"selectedFolder":"/reviewed/root-two"}', workspacePermission: 'read-only', model: 'gpt-5', effort: 'high' } }
    ticketApi.proposals.mockResolvedValue({ data: [proposal] })
    api.projects.mockResolvedValue({ data: [{ id: 'project-1', name: 'Changed project', folders: ['/current/root'], primaryFolder: '/current/root', revision: 2, hidden: false }] })
    ticketApi.approveRun.mockResolvedValue({ id: 'task-1' })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)
    const section = await screen.findByRole('region', { name: 'Pending ticket run proposals' })
    expect(section).toHaveTextContent('Inspect sign in.')
    expect(section).toHaveTextContent('Report failed paths')
    expect(section).toHaveTextContent('Project ID')
    expect(section).toHaveTextContent('Run folder')
    expect(section).toHaveTextContent('/reviewed/root-two')
    expect(section).toHaveTextContent('/reviewed/root-one')
    expect(section).not.toHaveTextContent('/current/root')
    expect(within(section).getByRole('list', { name: 'Reviewed project folders for ticket-1' }).children).toHaveLength(2)
    expect(section).toHaveTextContent('read-only')
    expect(section).toHaveTextContent('gpt-5')
    expect(section).toHaveTextContent('high')
    await userEvent.click(within(section).getByRole('button', { name: 'Approve' }))
    await waitFor(() => expect(ticketApi.approveRun).toHaveBeenCalledWith('ticket-1', 'ticket-hash', 'stable-operation-id'))
  })

  it('identifies proposals whose reviewed project snapshot has no attached folders as private workspace', async () => {
    ticketApi.proposals.mockResolvedValue({ data: [{ id: 'proposal-private', ticketId: 'ticket-private', ticketRevision: 1, hash: 'private-hash', status: 'proposed', createdAt: 1, snapshot: { agentId: 'agent-1', title: 'Private task', instructions: 'Review safely.', acceptanceCriteria: [], projectId: 'project-private', cwd: null, context: 'Execute ticket BRR-000009\nBrr project snapshot: {"id":"project-private","folders":[],"selectedFolder":null}', workspacePermission: 'read-only' } }] })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)

    const section = await screen.findByRole('region', { name: 'Pending ticket run proposals' })
    expect(section).toHaveTextContent('Run folder')
    expect(section).toHaveTextContent('No folders attached · private workspace')
    expect(section).not.toHaveTextContent('Not specified')
    expect(section).not.toHaveTextContent('Not recorded in this proposal')
  })

  it('identifies multi-question requests by their question header, task title, and owning agent', async () => {
    api.list.mockResolvedValue({ data: [{ id: 'agent-7', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.tasks.mockResolvedValue({ data: [{ id: 'task-7', agentId: 'agent-7', title: 'Deploy release', instructions: '', acceptanceCriteria: [], status: 'waiting_input', threadId: 'question-thread', createdAt: 1, updatedAt: 2 }], nextCursor: null })
    api.decisions.mockResolvedValue({ data: [], questions: [{ id: 'question-7', threadId: 'question-thread', turnId: 'turn-2', itemId: 'tool-3', agentId: 'agent-7', taskId: 'task-7', kind: 'blocking',
      questions: [{ id: 'scope', header: 'Scope', question: 'Which scope?', options: null }, { id: 'window', header: 'Window', question: 'When should it ship?', options: null }], draft: {}, resolution: 'pending', delivery: 'none', createdAt: 1, updatedAt: 3 }] })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)
    const section = await screen.findByRole('region', { name: 'Pending questions' })
    await userEvent.click(await screen.findByRole('button', { name: 'Tasks' }))

    expect(await within(section).findByText('Scope · 2 questions')).toBeInTheDocument()
    expect(section).toHaveTextContent('Deploy release · Release guide')
  })

  it('refreshes pending questions when a question changes while the Agents panel is open', async () => {
    const question = { id: 'question-live', threadId: 'question-thread', turnId: 'turn-2', itemId: 'tool-3', kind: 'async' as const,
      questions: [{ id: 'scope', header: 'Scope', question: 'Which scope?', options: null }], draft: {}, resolution: 'pending' as const, delivery: 'none' as const, createdAt: 1, updatedAt: 3 }
    api.decisions.mockResolvedValueOnce({ data: [], questions: [] }).mockResolvedValueOnce({ data: [], questions: [question] })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)

    expect(await screen.findByText('Your agents work in their own conversations')).toBeInTheDocument()
    act(() => runtime.onEvent?.({ method: 'brr/questions/changed' }))

    expect(await screen.findByRole('region', { name: 'Pending questions' })).toHaveTextContent('Scope')
    expect(screen.getByRole('region', { name: 'Pending questions' })).toHaveTextContent('question-thread')
    expect(api.decisions).toHaveBeenCalledTimes(2)
  })

  it('labels waiting-input tasks clearly and offers pause and cancellation without resume', async () => {
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.tasks.mockResolvedValue({ data: [{ id: 'task-input', agentId: 'agent-1', title: 'Check the deployment', instructions: '', acceptanceCriteria: [], status: 'waiting_input', threadId: 'task-thread', createdAt: 1, updatedAt: 2 }], nextCursor: null })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)
    await userEvent.click(await screen.findByRole('button', { name: 'Tasks' }))

    expect(await screen.findByText('Waiting for input')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Pause' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Resume' })).not.toBeInTheDocument()
  })

  it('assigns work with written instructions and acceptance criteria', async () => {
    const user = userEvent.setup()
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)

    await user.click(await screen.findByRole('button', { name: 'Tasks' }))
    await user.type(screen.getByLabelText('Title'), 'Review the next release')
    await user.type(screen.getAllByLabelText('Instructions')[0], 'Check changes for release blockers.')
    await user.type(screen.getByLabelText('Acceptance criteria'), 'List blockers with evidence')
    await user.click(screen.getByRole('button', { name: 'Create task' }))

    await waitFor(() => expect(api.createTask).toHaveBeenCalledWith(expect.objectContaining({
      agentId: 'agent-1', title: 'Review the next release', instructions: 'Check changes for release blockers.',
      acceptanceCriteria: ['List blockers with evidence'], dependencies: [],
    })))
  })

  it('reviews the complete proposed graph before approving its keyed dependencies', async () => {
    const user = userEvent.setup()
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.decisions.mockResolvedValue({ data: [{ id: 'proposal-1', parentTaskId: 'parent-1', hash: 'graph-hash', status: 'proposed', tasks: [
      { key: 'inspect', task: { agentId: 'agent-1', title: 'Inspect release', instructions: 'Check the change set.', acceptanceCriteria: ['List blockers'], cwd: '/workspace' }, dependsOn: [] },
      { key: 'report', task: { agentId: 'agent-1', title: 'Write release report', instructions: 'Summarize verified findings.', acceptanceCriteria: ['Cite evidence'] }, dependsOn: ['inspect'] },
    ] }] })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)

    await user.click(await screen.findByRole('button', { name: 'Tasks' }))

    const review = await screen.findByRole('heading', { name: 'Review proposed task graph' })
    expect(review.parentElement).toHaveTextContent('Inspect release')
    expect(review.parentElement).toHaveTextContent('Write release report')
    expect(review.parentElement).toHaveTextContent('Depends on: inspect')
    await user.click(screen.getByRole('button', { name: 'Approve task graph' }))
    await waitFor(() => expect(api.approveProposal).toHaveBeenCalledWith('proposal-1', 'graph-hash'))
  })

  it('schedules a reviewed dependency graph using the same proposal hash without launching one-off tasks', async () => {
    const user = userEvent.setup()
    const graph = { id: 'proposal-repeat', parentTaskId: 'parent-1', hash: 'exact-graph-hash', status: 'proposed', tasks: [
      { key: 'inspect', task: { agentId: 'agent-1', title: 'Inspect release', instructions: 'Check changes.', acceptanceCriteria: ['List risks'] }, dependsOn: [] },
      { key: 'report', task: { agentId: 'agent-2', title: 'Write report', instructions: 'Summarize findings.', acceptanceCriteria: ['Cite evidence'] }, dependsOn: ['inspect'] },
    ] }
    let scheduled = false
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.decisions.mockResolvedValue({ data: [graph] })
    api.proposals.mockImplementation(async () => ({ data: scheduled ? [{ ...graph, status: 'scheduled' }] : [graph] }))
    api.workflows.mockImplementation(async () => ({ data: scheduled ? [{ id: 'workflow-1', name: 'Weekly release graph', agentId: 'agent-1', expression: '0 9 * * 1', timezone: 'Australia/Brisbane', paused: false }] : [] }))
    api.scheduleProposal.mockImplementation(async () => {
      scheduled = true
      return { id: 'workflow-1', name: 'Weekly release graph', agentId: 'agent-1', expression: '0 9 * * 1', timezone: 'Australia/Brisbane', paused: false }
    })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)

    await user.click(await screen.findByRole('button', { name: 'Tasks' }))
    const graphCard = screen.getByRole('heading', { name: 'Review proposed task graph' }).closest('article')!
    expect(graphCard).toHaveTextContent('Inspect release')
    expect(graphCard).toHaveTextContent('Write report')
    expect(graphCard).toHaveTextContent('Depends on: inspect')
    await user.click(within(graphCard).getByText('Repeat this graph'))
    await user.type(within(graphCard).getByLabelText('Schedule name'), 'Weekly release graph')
    await user.type(within(graphCard).getByLabelText('Cron expression'), '0 9 * * 1')
    await user.clear(within(graphCard).getByLabelText('Timezone'))
    await user.type(within(graphCard).getByLabelText('Timezone'), 'Australia/Brisbane')
    await user.click(within(graphCard).getByRole('button', { name: 'Schedule graph' }))

    await waitFor(() => expect(api.scheduleProposal).toHaveBeenCalledWith('proposal-repeat', 'exact-graph-hash', {
      name: 'Weekly release graph', expression: '0 9 * * 1', timezone: 'Australia/Brisbane',
    }))
    expect(api.approveProposal).not.toHaveBeenCalled()
    expect(api.createTask).not.toHaveBeenCalled()
    await waitFor(() => expect(screen.queryByRole('heading', { name: 'Review proposed task graph' })).not.toBeInTheDocument())
    expect(screen.getByText('Weekly release graph')).toBeInTheDocument()
  })

  it('lets users raise worker limits up to the backend supported range', async () => {
    const user = userEvent.setup()
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.settings.mockResolvedValue({ maxBackground: 8, maxPerAgent: 5 })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)

    await user.click(await screen.findByRole('button', { name: 'Tasks' }))
    const total = screen.getByRole('spinbutton', { name: 'Total background workers' })
    const perAgent = screen.getByRole('spinbutton', { name: 'Workers per agent' })
    await waitFor(() => expect(total).toHaveValue(8))
    expect(total).toHaveAttribute('max', '32')
    expect(perAgent).toHaveAttribute('max', '32')
  })

  it('loads another task page without replacing the tasks already shown', async () => {
    const user = userEvent.setup()
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.tasks.mockResolvedValue({ data: [
      { id: 'task-1', agentId: 'agent-1', title: 'First task', instructions: '', acceptanceCriteria: [], status: 'succeeded', createdAt: 1, updatedAt: 1 },
    ], nextCursor: 'cursor-1' })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)

    await user.click(await screen.findByRole('button', { name: 'Tasks' }))
    expect(await screen.findAllByText('First task')).toHaveLength(2)
    await user.click(await screen.findByRole('button', { name: 'Load more tasks' }))

    await waitFor(() => expect(api.tasks).toHaveBeenCalledWith('agent-1', { after: 'cursor-1', limit: 25 }))
    expect(screen.getAllByText('First task')).toHaveLength(2)
  })

  it('shows task results and evidence, opens its thread read-only, and avoids retrying unknown outcomes', async () => {
    const user = userEvent.setup()
    const onOpenThread = vi.fn()
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.tasks.mockResolvedValue({ data: [{ id: 'task-unknown', agentId: 'agent-1', title: 'Check deployment', instructions: '', acceptanceCriteria: ['Confirm state'], status: 'blocked', reason: 'Outcome unknown after disconnect', result: 'Could not confirm deployment.', evidence: ['thread-item-9'], threadId: 'task-thread', cwd: '/workspace', createdAt: 1, updatedAt: 2 }], nextCursor: null })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={onOpenThread} onClose={vi.fn()} />)

    await user.click(await screen.findByRole('button', { name: 'Tasks' }))

    expect(await screen.findByText('Could not confirm deployment.')).toBeInTheDocument()
    expect(screen.getByText('thread-item-9')).toBeInTheDocument()
    expect(screen.getByText(/outcome is unknown/i)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Retry' })).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Open conversation' }))
    expect(onOpenThread).toHaveBeenCalledWith(expect.objectContaining({ id: 'task-thread' }), 'agent-1', { readOnly: true })
  })

  it('filters memory by project, shows source evidence, and opens its source conversation', async () => {
    const user = userEvent.setup()
    const onOpenThread = vi.fn()
    const memory = { id: 'memory-1', scope: 'agent', scopeId: 'agent-1', kind: 'preference', content: 'Check release notes.', revision: 1, sourceIds: ['source-1'], createdAt: 1, updatedAt: 1 }
    const source = { sourceId: 'source-1', revision: 3, threadId: 'source-thread', agentId: 'agent-1', content: 'The user explicitly asked to check release notes.', active: true }
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.projects.mockResolvedValue({ data: [{id: 'project-1', name: 'Release project', folders: ['/workspace/project'], primaryFolder: '/workspace/project', revision: 1, model: null, effort: null, permission: null, hidden: false}] })
    api.memory.mockResolvedValue({ data: [memory], sources: [source] })
    api.searchMemory.mockResolvedValue({ data: [memory], sources: [source] })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={onOpenThread} onClose={vi.fn()} />)

    await user.click(await screen.findByRole('button', { name: 'Memory' }))
    await user.selectOptions(await screen.findByRole('combobox', { name: 'Memory project' }), 'project-1')
    await user.type(screen.getByRole('textbox', { name: 'Search memories' }), 'release notes')
    await user.click(screen.getByRole('button', { name: 'Search' }))

    await waitFor(() => expect(api.searchMemory).toHaveBeenCalledWith('release notes', 'agent-1', 'project-1'))
    await user.click(await screen.findByText('Source · revision 3'))
    expect(await screen.findByText('The user explicitly asked to check release notes.')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Open source conversation' }))
    expect(onOpenThread).toHaveBeenCalledWith(expect.objectContaining({ id: 'source-thread' }), 'agent-1')
  })

  it('uses the exact forget event id for undo and lets a user discard an unapproved profile proposal', async () => {
    const user = userEvent.setup()
    const memory = { id: 'memory-1', scope: 'agent', scopeId: 'agent-1', kind: 'preference', content: 'Check release notes.', revision: 1, sourceIds: [], createdAt: 1, updatedAt: 1 }
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.memory.mockResolvedValue({ data: [memory], sources: [] })
    api.drafts.mockResolvedValue({ data: [{ id: 'draft-1', threadId: thread.id, agentId: 'agent-1', candidate: { ...candidate, soul: 'New soul' }, hash: 'draft-hash', status: 'proposed' }] })
    render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)

    await user.click(await screen.findByRole('button', { name: 'Memory' }))
    await user.click(await screen.findByRole('button', { name: 'Forget' }))
    await waitFor(() => expect(api.forgetMemory).toHaveBeenCalledWith('memory-1', 'agent-1', undefined))
    expect(await screen.findByRole('button', { name: 'Undo forget' })).toBeInTheDocument()
    await user.click(await screen.findByRole('button', { name: 'Undo forget' }))
    await waitFor(() => expect(api.undoMemory).toHaveBeenCalledWith(7))

    expect(await screen.findByText('Compare with revision 1')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Discard proposal' }))
    await waitFor(() => expect(api.reject).toHaveBeenCalledWith('draft-1', 'draft-hash'))
  })

  it('starts a named chat in the selected project without using its ID as a folder', async () => {
    api.list.mockResolvedValue({data: [{id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1}]})
    api.open.mockResolvedValue({thread})
    render(<AgentsPanel models={[model]} desktop online projectId="research-project" onOpenThread={vi.fn()} onClose={vi.fn()} />)
    await userEvent.click(await screen.findByRole('button', {name: 'New session'}))
    await waitFor(() => expect(api.open).toHaveBeenCalledWith('agent-1', {model: 'gpt-5', effort: 'medium', projectId: 'research-project'}))
    expect(runtime.inspectProject).toHaveBeenCalledWith('research-project')
  })

  it('reviews exact project setup commands for named sessions and retries only after approval', async () => {
    const user = userEvent.setup()
    const onOpenThread = vi.fn()
    api.list.mockResolvedValue({ data: [{ id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }] })
    api.inspectWorkspace.mockResolvedValue({ cwd: '/workspace', git: true, commands: [{ phase: 'post-create', name: 'install', template: 'pnpm install', approved: false }] })
    render(<AgentsPanel models={[model]} desktop online cwd="/workspace" onOpenThread={onOpenThread} onClose={vi.fn()} />)

    await user.click(await screen.findByRole('button', { name: 'New session' }))
    expect(await screen.findByRole('region', { name: 'Review project setup hooks' })).toHaveTextContent('pnpm install')
    expect(api.open).not.toHaveBeenCalled()
    await user.click(screen.getByRole('button', { name: 'Approve setup hooks' }))
    await waitFor(() => expect(runtime.approveWorktree).toHaveBeenCalledWith('/workspace', ['pnpm install']))
    expect(api.open).not.toHaveBeenCalled()
  })

  it('does not let a delayed profile list replace a newer approved revision', async () => {
    const user = userEvent.setup()
    let resolveDelayed!: (value: { data: typeof currentProfile[] }) => void
    const currentProfile = { id: 'agent-1', ...candidate, revision: 1, createdAt: 1, updatedAt: 1 }
    const approvedProfile = { ...currentProfile, description: 'Updated release guidance', revision: 2 }
    api.list.mockResolvedValueOnce({ data: [currentProfile] })
    api.drafts.mockResolvedValue({ data: [{ id: 'draft-1', threadId: thread.id, agentId: 'agent-1', candidate: { ...candidate, description: approvedProfile.description }, hash: 'hash-2', status: 'proposed' }] })
    api.approve.mockResolvedValue({ agent: approvedProfile })
    const { rerender } = render(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)
    await screen.findAllByRole('heading', { name: 'Release guide' })
    await user.click(await screen.findByRole('button', { name: 'Apply profile changes' }))
    expect(await screen.findAllByText('Updated release guidance')).toHaveLength(2)

    api.list.mockImplementationOnce(() => new Promise(resolve => { resolveDelayed = resolve }))
    runtime.onEvent?.({ method: 'brr/agents/changed' })
    await waitFor(() => expect(resolveDelayed).toBeDefined())
    await act(async () => resolveDelayed({ data: [currentProfile] }))
    rerender(<AgentsPanel models={[model]} desktop online onOpenThread={vi.fn()} onClose={vi.fn()} />)

    await user.click(screen.getByRole('button', { name: 'Profile' }))
    expect(await screen.findByText('Revision 2')).toBeInTheDocument()
  })
})
