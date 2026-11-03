import { beforeEach, describe, expect, it, vi } from 'vitest'
import { act, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

const api = vi.hoisted(() => ({ projects: vi.fn(), agents: vi.fn(), list: vi.fn(), read: vi.fn(), create: vi.fn(), update: vi.fn(), move: vi.fn(), comment: vi.fn(), proposeRun: vi.fn(), proposals: vi.fn(), approveRun: vi.fn(), rejectRun: vi.fn() }))
vi.mock('../lib/tickets', async () => ({ ...(await vi.importActual('../lib/tickets')), tickets: api }))
const runtime = vi.hoisted(() => ({ handler: null as null | ((event: { method: string }) => void) }))
vi.mock('../lib/client', () => ({ client: { subscribe: vi.fn(async (handler: (event: { method: string }) => void) => { runtime.handler = handler; return () => { runtime.handler = null } }) } }))

import { TicketsPanel } from './TicketsPanel'

const project = { id: 'project-1', name: 'App', folders: ['/workspace/app'], primaryFolder: '/workspace/app', revision: 1, hidden: false }
const ticket = { id: 'ticket-1', number: 'BRR-000008', projectId: project.id, title: 'Review auth flow', description: 'Check login behavior.', acceptanceCriteria: [{ id: 'c1', text: 'Keep ChatGPT-only login', checked: false }], agentId: null, priority: 'high', status: 'todo', position: 0, revision: 1, statusRevision: 1, archived: false, createdAt: 1, updatedAt: 1 }

describe('TicketsPanel', () => {
  beforeEach(() => {
    Object.values(api).forEach(mock => mock.mockReset())
    api.projects.mockResolvedValue({ data: [project] })
    api.agents.mockResolvedValue({ data: [] })
    api.list.mockResolvedValue({ data: [ticket], nextCursor: null })
    api.read.mockResolvedValue({ ticket, comments: { data: [], nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [] })
    api.create.mockImplementation(async input => ({ ...ticket, ...input, id: 'ticket-2', number: 'BRR-000009' }))
    api.update.mockResolvedValue(ticket)
    api.move.mockResolvedValue({ ...ticket, status: 'in_progress' })
    api.comment.mockResolvedValue({ id: 'comment-1', ticketId: ticket.id, body: 'Looks good', actor: { kind: 'human' }, createdAt: 2 })
    api.proposals.mockResolvedValue({ data: [] })
    runtime.handler = null
  })

  it('shows project tickets and creates a ticket from a quick title', async () => {
    const user = userEvent.setup()
    const created = { ...ticket, id: 'ticket-2', number: 'BRR-000009', title: 'Add a settings page' }
    api.create.mockResolvedValue(created)
    api.list.mockResolvedValue({ data: [ticket, created], nextCursor: null })
    api.read.mockImplementation(async (id: string) => ({ ticket: id === created.id ? created : ticket, comments: { data: [], nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [] }))
    render(<TicketsPanel onClose={vi.fn()} />)
    expect(await screen.findByRole('heading', { name: 'Tickets' })).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ })).toBeInTheDocument()
    await user.type(screen.getByRole('textbox', { name: 'New ticket title' }), 'Add a settings page')
    await user.keyboard('{Enter}')
    await waitFor(() => expect(api.create).toHaveBeenCalledWith(expect.objectContaining({ projectId: project.id, title: 'Add a settings page' })))
    expect(await screen.findByRole('button', { name: /BRR-000009 Add a settings page/ })).toBeInTheDocument()
  })

  it('does not inject a newly created active ticket into the archived board', async () => {
    api.list.mockResolvedValue({ data: [], nextCursor: null })
    api.read.mockImplementation(async id => ({ ticket: { ...ticket, id, number: 'BRR-000009', title: 'Active local ticket' }, comments: { data: [] }, activity: { data: [] }, runs: [] }))
    render(<TicketsPanel onClose={vi.fn()} />)
    await userEvent.type(await screen.findByRole('textbox', { name: 'New ticket title' }), 'Active local ticket')
    await userEvent.click(screen.getByRole('button', { name: 'Add ticket' }))
    await screen.findByRole('dialog', { name: 'Ticket details' })
    await userEvent.click(screen.getByRole('button', { name: 'Close ticket details' }))
    await userEvent.click(screen.getByRole('button', { name: 'Archived' }))
    await waitFor(() => expect(api.list).toHaveBeenLastCalledWith(expect.objectContaining({ archived: true })))
    await waitFor(() => expect(screen.queryByRole('button', { name: /BRR-000009 Active local ticket/ })).not.toBeInTheDocument())
  })

  it('moves a ticket with the accessible status control and edits its description', async () => {
    const user = userEvent.setup()
    render(<TicketsPanel onClose={vi.fn()} />)
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    const details = await screen.findByRole('dialog', { name: 'Ticket details' })
    expect(within(details).getByRole('textbox', { name: 'Criterion 1' })).toHaveValue('Keep ChatGPT-only login')
    await user.selectOptions(within(details).getByRole('combobox', { name: 'Status' }), 'in_progress')
    await waitFor(() => expect(api.move).toHaveBeenCalledWith(ticket.id, 1, 'in_progress', undefined, expect.any(String)))
    await user.clear(within(details).getByRole('textbox', { name: 'Description' }))
    await user.type(within(details).getByRole('textbox', { name: 'Description' }), 'Updated behavior.')
    await user.click(within(details).getByRole('button', { name: 'Save description' }))
    await waitFor(() => expect(api.update).toHaveBeenCalledWith(ticket.id, 1, { description: 'Updated behavior.' }, expect.any(String)))
  })

  it('ignores an older detail read that resolves after a successful save', async () => {
    const user = userEvent.setup()
    let finishRead!: (value: unknown) => void
    api.read.mockImplementationOnce(() => new Promise(resolve => { finishRead = resolve }))
    api.update.mockResolvedValue({ ...ticket, description: 'Saved before the detail read', revision: 2 })
    render(<TicketsPanel onClose={vi.fn()} />)
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    await waitFor(() => expect(api.read).toHaveBeenCalledWith(ticket.id))
    const savedDescription = 'Saved before the detail read'
    const description = screen.getByRole('textbox', { name: 'Description' })
    await user.clear(description)
    await user.type(description, savedDescription)
    expect(screen.getByRole('textbox', { name: 'Description' })).toHaveValue(savedDescription)
    await user.click(screen.getByRole('button', { name: 'Save description' }))
    await waitFor(() => expect(api.update).toHaveBeenCalledWith(ticket.id, ticket.revision, {description: savedDescription}, expect.any(String)))
    await act(async () => { finishRead({ ticket, comments: { data: [] }, activity: { data: [] }, runs: [] }) })
    expect(screen.getByRole('textbox', { name: 'Description' })).toHaveValue(savedDescription)
  })

  it('keeps newer description typing when an earlier save resolves', async () => {
    const user = userEvent.setup()
    let resolveSave!: (value: typeof ticket) => void
    api.update.mockImplementationOnce(() => new Promise(resolve => { resolveSave = resolve }))
    render(<TicketsPanel onClose={vi.fn()} />)
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    let details = await screen.findByRole('dialog', { name: 'Ticket details' })
    const description = within(details).getByRole('textbox', { name: 'Description' })
    await user.clear(description)
    await user.type(description, 'First version')
    await user.click(within(details).getByRole('button', { name: 'Save description' }))
    await waitFor(() => expect(api.update).toHaveBeenCalled())
    await user.clear(description)
    await user.type(description, 'Newer local draft')
    await act(async () => resolveSave({ ...ticket, description: 'First version', revision: 2 }))
    details = await screen.findByRole('dialog', { name: 'Ticket details' })
    expect(within(details).getByRole('textbox', { name: 'Description' })).toHaveValue('Newer local draft')
  })

  it('does not let a delayed save from ticket A overwrite ticket B details', async () => {
    const user = userEvent.setup()
    const other = { ...ticket, id: 'ticket-2', number: 'BRR-000009', title: 'Update account screen', description: 'Second ticket description.' }
    api.list.mockResolvedValue({ data: [ticket, other], nextCursor: null })
    api.read.mockImplementation(async (id: string) => ({ ticket: id === other.id ? other : ticket, comments: { data: [], nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [] }))
    let resolveSave!: (value: typeof ticket) => void
    api.update.mockImplementationOnce(() => new Promise(resolve => { resolveSave = resolve }))
    render(<TicketsPanel onClose={vi.fn()} />)
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    let details = await screen.findByRole('dialog', { name: 'Ticket details' })
    const description = within(details).getByRole('textbox', { name: 'Description' })
    await user.clear(description)
    await user.type(description, 'Delayed response from A')
    await user.click(within(details).getByRole('button', { name: 'Save description' }))
    await waitFor(() => expect(api.update).toHaveBeenCalled())
    await user.click(within(details).getByRole('button', { name: 'Close ticket details' }))
    await user.click(await screen.findByRole('button', { name: /BRR-000009 Update account screen/ }))
    details = await screen.findByRole('dialog', { name: 'Ticket details' })
    await act(async () => resolveSave({ ...ticket, description: 'Delayed response from A', revision: 2 }))
    expect(within(details).getByRole('textbox', { name: 'Description' })).toHaveValue('Second ticket description.')
  })

  it('retains field drafts when ticket details close, saves criteria edits, and archives the ticket', async () => {
    const user = userEvent.setup()
    const loadedDescription = 'Details loaded from the ticket service.'
    api.read.mockResolvedValueOnce({ ticket: { ...ticket, description: loadedDescription }, comments: { data: [], nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [] })
    render(<TicketsPanel onClose={vi.fn()} />)
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    const details = await screen.findByRole('dialog', { name: 'Ticket details' })
    const description = within(details).getByRole('textbox', { name: 'Description' })
    await waitFor(() => expect(api.read).toHaveBeenCalledWith(ticket.id))
    await waitFor(() => expect(description).toHaveValue(loadedDescription))
    await user.clear(description)
    await user.type(description, 'Draft that must survive closing the details.')
    await user.click(within(details).getByRole('button', { name: 'Close ticket details' }))
    await user.click(screen.getByRole('button', { name: /BRR-000008 Review auth flow/ }))
    const reopened = await screen.findByRole('dialog', { name: 'Ticket details' })
    await waitFor(() => expect(api.read).toHaveBeenCalledTimes(2))
    await waitFor(() => expect(within(reopened).getByRole('textbox', { name: 'Description' })).toHaveValue('Draft that must survive closing the details.'))
    await user.type(within(reopened).getByRole('textbox', { name: 'New acceptance criterion' }), 'Show a review summary')
    await user.click(within(reopened).getByRole('button', { name: 'Add' }))
    await user.click(within(reopened).getByRole('button', { name: 'Save checklist' }))
    await waitFor(() => expect(api.update).toHaveBeenCalledWith(ticket.id, 1, { acceptanceCriteria: expect.arrayContaining([expect.objectContaining({ id: 'c1', text: 'Keep ChatGPT-only login', checked: false }), expect.objectContaining({ text: 'Show a review summary', checked: false })]) }, expect.any(String)))
    await waitFor(() => expect(within(reopened).getByRole('button', { name: 'Archive' })).not.toBeDisabled())
    await user.click(within(reopened).getByRole('button', { name: 'Archive' }))
    await waitFor(() => expect(api.update).toHaveBeenCalledWith(ticket.id, 1, { archived: true }, expect.any(String)))
    expect(screen.queryByRole('button', { name: /BRR-000008 Review auth flow/ })).not.toBeInTheDocument()
  })

  it('lets keyboard users reorder tickets within a status column', async () => {
    const second = { ...ticket, id: 'ticket-2', number: 'BRR-000009', title: 'Update workspace settings', position: 1 }
    api.list.mockResolvedValue({ data: [ticket, second], nextCursor: null })
    render(<TicketsPanel onClose={vi.fn()} />)
    await userEvent.click(await screen.findByRole('button', { name: 'Move BRR-000009 up' }))
    await waitFor(() => expect(api.move).toHaveBeenCalledWith(second.id, 1, 'todo', ticket.id, expect.any(String)))
  })

  it('loads every board page and provides an accessible route to tickets beyond the first page', async () => {
    const second = { ...ticket, id: 'ticket-2', number: 'BRR-000009', title: 'Update workspace settings', position: 0 }
    api.list.mockImplementation(async ({ cursor }: { cursor?: string }) => cursor ? { data: [second], nextCursor: null } : { data: [ticket], nextCursor: 'page-2' })
    render(<TicketsPanel onClose={vi.fn()} />)
    expect(await screen.findByRole('button', { name: /BRR-000009 Update workspace settings/ })).toBeInTheDocument()
    expect(api.list).toHaveBeenCalledWith(expect.objectContaining({ projectId: project.id, cursor: 'page-2', limit: 100 }))
  })

  it('loads older comments from the read cursor and closes detail with Escape', async () => {
    api.read.mockImplementation(async (_id: string, cursor: { commentsCursor?: number } = {}) => cursor.commentsCursor
      ? { ticket, comments: { data: [{ id: 'comment-old', ticketId: ticket.id, body: 'Earlier note', actor: { kind: 'human' }, createdAt: 1 }], nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [] }
      : { ticket, comments: { data: [{ id: 'comment-new', ticketId: ticket.id, body: 'Latest note', actor: { kind: 'human' }, createdAt: 2 }], nextCursor: 17 }, activity: { data: [], nextCursor: null }, runs: [] })
    const user = userEvent.setup()
    render(<TicketsPanel onClose={vi.fn()} />)
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    const details = await screen.findByRole('dialog', { name: 'Ticket details' })
    await user.click(within(details).getByRole('button', { name: 'Load older comments' }))
    expect(await within(details).findByText('Earlier note')).toBeInTheDocument()
    expect(api.read).toHaveBeenCalledWith(ticket.id, { commentsCursor: 17 })
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Ticket details' })).not.toBeInTheDocument())
    await waitFor(() => expect(screen.getByRole('button', { name: /BRR-000008 Review auth flow/ })).toHaveFocus())
  })

  it('refreshes open ticket activity after a native ticket change while retaining local edits', async () => {
    const user = userEvent.setup()
    render(<TicketsPanel onClose={vi.fn()} />)
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    const details = await screen.findByRole('dialog', { name: 'Ticket details' })
    const description = within(details).getByRole('textbox', { name: 'Description' })
    await user.clear(description)
    await user.type(description, 'Keep this local edit')
    api.read.mockResolvedValue({ ticket: { ...ticket, title: 'Remote ticket update', revision: 2 }, comments: { data: [{ id: 'comment-event', ticketId: ticket.id, body: 'New activity from another writer', actor: { kind: 'human' }, createdAt: 3 }], nextCursor: null }, activity: { data: [{ id: 'activity-event', ticketId: ticket.id, kind: 'updated', payload: {}, actor: { kind: 'human' }, createdAt: 3 }], nextCursor: null }, runs: [] })
    await waitFor(() => expect(runtime.handler).toBeTruthy())
    await act(async () => { runtime.handler?.({ method: 'brr/tickets/changed' }); await Promise.resolve() })
    expect(await within(details).findByText('New activity from another writer')).toBeInTheDocument()
    expect(within(details).getByRole('textbox', { name: 'Description' })).toHaveValue('Keep this local edit')
    expect(screen.getByRole('alert')).toHaveTextContent(/changed elsewhere/i)
  })

  it('surfaces linked execution state on the card and evidence in ticket details', async () => {
    const user = userEvent.setup()
    const execution = { taskId: 'task-linked', status: 'blocked', result: 'Waiting on workspace access', evidence: ['Permission prompt was declined'] }
    api.list.mockResolvedValue({ data: [{ ...ticket, execution }], nextCursor: null })
    api.read.mockResolvedValue({ ticket: { ...ticket, execution }, comments: { data: [], nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [execution] })
    render(<TicketsPanel onClose={vi.fn()} />)
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    expect(screen.getByLabelText('Agent execution: blocked')).toBeInTheDocument()
    expect(await screen.findByText('Permission prompt was declined')).toBeInTheDocument()
  })

  it('waits for an explicit multi-folder choice before queuing a run and previews the exact scope', async () => {
    const user = userEvent.setup()
    const multiFolderProject = { ...project, folders: ['/reviewed/root-one', '/reviewed/root-two'] }
    const assignedTicket = { ...ticket, agentId: 'agent-1' }
    const proposal = { id: 'proposal-1', ticketId: ticket.id, ticketRevision: 1, hash: 'run-hash', status: 'proposed', createdAt: 1, snapshot: { agentId: 'agent-1', title: ticket.title, instructions: ticket.description, acceptanceCriteria: ['Keep ChatGPT-only login'], projectId: project.id, cwd: '/reviewed/root-two', context: 'Execute ticket BRR-000008\nBrr project snapshot: {"id":"project-1","folders":["/reviewed/root-one","/reviewed/root-two"],"selectedFolder":"/reviewed/root-two"}', workspacePermission: 'workspace-write' as const, model: 'gpt-5', effort: 'high' } }
    api.projects.mockResolvedValue({ data: [multiFolderProject] })
    api.list.mockResolvedValue({ data: [assignedTicket], nextCursor: null })
    api.read.mockResolvedValue({ ticket: assignedTicket, comments: { data: [], nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [] })
    let resolveProposal!: (value: typeof proposal) => void
    api.proposeRun.mockImplementationOnce(() => new Promise(resolve => { resolveProposal = resolve }))
    render(<TicketsPanel onClose={vi.fn()}/> )
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    const details = await screen.findByRole('dialog', { name: 'Ticket details' })
    const runButton = within(details).getByRole('button', { name: 'Propose agent run' })
    expect(within(details).getByRole('combobox', { name: 'Run folder' })).toHaveValue('')
    expect(runButton).toBeDisabled()
    expect(api.proposeRun).not.toHaveBeenCalled()
    await user.selectOptions(within(details).getByRole('combobox', { name: 'Run folder' }), '/reviewed/root-two')
    await user.click(runButton)
    await waitFor(() => expect(api.proposeRun).toHaveBeenCalledWith(ticket.id, 1, 'agent-1', '/reviewed/root-two', expect.any(String)))
    multiFolderProject.folders = ['/current/new-root']
    await act(async () => resolveProposal(proposal))
    const review = await within(details).findByRole('region', { name: 'Review proposed agent run' })
    expect(review).toHaveTextContent('App · project-1')
    expect(review).toHaveTextContent('/reviewed/root-one')
    expect(review).toHaveTextContent('/reviewed/root-two')
    expect(within(review).getByRole('list', { name: 'Reviewed project folders' }).children).toHaveLength(2)
    expect(review).not.toHaveTextContent('/current/new-root')
    expect(review).toHaveTextContent('workspace-write')
  })

  it('uses the sole registered folder by default and proposes folderless runs in private workspace', async () => {
    const user = userEvent.setup()
    const assignedTicket = { ...ticket, agentId: 'agent-1' }
    api.list.mockResolvedValue({ data: [assignedTicket], nextCursor: null })
    api.read.mockResolvedValue({ ticket: assignedTicket, comments: { data: [], nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [] })
    api.proposeRun.mockResolvedValue({ id: 'proposal-single', ticketId: ticket.id, ticketRevision: 1, hash: 'single-hash', status: 'proposed', createdAt: 1, snapshot: { agentId: 'agent-1', title: ticket.title, instructions: ticket.description, acceptanceCriteria: ['Keep ChatGPT-only login'], projectId: project.id, cwd: '/workspace/app', workspacePermission: 'read-only' } })
    const { unmount } = render(<TicketsPanel onClose={vi.fn()}/> )
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    const details = await screen.findByRole('dialog', { name: 'Ticket details' })
    expect(within(details).queryByRole('combobox', { name: 'Run folder' })).not.toBeInTheDocument()
    await user.click(within(details).getByRole('button', { name: 'Propose agent run' }))
    await waitFor(() => expect(api.proposeRun).toHaveBeenCalledWith(ticket.id, 1, 'agent-1', '/workspace/app', expect.any(String)))

    unmount()
    api.projects.mockResolvedValue({ data: [{ ...project, folders: [] }] })
    const noFolderTicket = { ...ticket, id: 'ticket-no-folder', projectId: project.id, number: 'BRR-000010', title: 'No workspace yet', agentId: 'agent-1' }
    const createdTicket = { ...noFolderTicket, id: 'ticket-2', number: 'BRR-000011', title: 'Track workspace registration' }
    api.proposeRun.mockResolvedValue({ id: 'proposal-scratch', ticketId: noFolderTicket.id, ticketRevision: 1, hash: 'scratch-hash', status: 'proposed', createdAt: 1, snapshot: { agentId: 'agent-1', title: noFolderTicket.title, instructions: noFolderTicket.description, acceptanceCriteria: ['Keep ChatGPT-only login'], projectId: project.id, cwd: null, context: 'Execute ticket BRR-000010\nBrr project snapshot: {"id":"project-1","folders":[],"selectedFolder":null}', workspacePermission: 'workspace-write' } })
    api.list.mockResolvedValue({ data: [noFolderTicket], nextCursor: null })
    api.create.mockResolvedValue(createdTicket)
    api.read.mockImplementation(async (id: string) => ({ ticket: id === createdTicket.id ? createdTicket : noFolderTicket, comments: { data: [], nextCursor: null }, activity: { data: [], nextCursor: null }, runs: [] }))
    render(<TicketsPanel onClose={vi.fn()}/> )
    await user.click(await screen.findByRole('button', { name: /BRR-000010 No workspace yet/ }))
    const noFolderDetails = await screen.findByRole('dialog', { name: 'Ticket details' })
    await waitFor(() => expect(within(noFolderDetails).getByRole('button', { name: 'Propose agent run' })).toBeEnabled())
    expect(screen.getByText('No folders attached · private workspace')).toBeInTheDocument()
    await user.click(within(noFolderDetails).getByRole('button', { name: 'Propose agent run' }))
    await waitFor(() => expect(api.proposeRun).toHaveBeenCalledWith(noFolderTicket.id, 1, 'agent-1', undefined, expect.any(String)))
    const review = await within(noFolderDetails).findByRole('region', { name: 'Review proposed agent run' })
    expect(review).toHaveTextContent('No folders attached · private workspace')
    const title = screen.getByRole('textbox', { name: 'New ticket title' })
    expect(title).toBeEnabled()
    await user.type(title, 'Track workspace registration')
    await user.keyboard('{Enter}')
    await waitFor(() => expect(api.create).toHaveBeenCalledWith(expect.objectContaining({ projectId: project.id, title: 'Track workspace registration' })))
  })

  it('keeps a locally captured edit visible when another writer changed the ticket', async () => {
    const user = userEvent.setup()
    api.update.mockRejectedValueOnce(new Error(JSON.stringify({ code: 'conflict', current: { ...ticket, title: 'Remote title', revision: 2 } })))
    render(<TicketsPanel onClose={vi.fn()} />)
    await user.click(await screen.findByRole('button', { name: /BRR-000008 Review auth flow/ }))
    const details = await screen.findByRole('dialog', { name: 'Ticket details' })
    const description = within(details).getByRole('textbox', { name: 'Description' })
    await user.clear(description)
    await user.type(description, 'My unsaved edit')
    await user.click(within(details).getByRole('button', { name: 'Save description' }))
    expect(await screen.findByRole('alert')).toHaveTextContent(/changed elsewhere/i)
    expect(within(details).getByRole('textbox', { name: 'Description' })).toHaveValue('My unsaved edit')
    expect(within(details).getByText('Remote title')).toBeInTheDocument()
  })
})
