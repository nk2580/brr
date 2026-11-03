import React from 'react'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, expect, it, vi } from 'vitest'
import type { ProjectRecord } from './lib/client'
import App from './App'

const state = vi.hoisted(() => {
  const project: ProjectRecord = {id: 'p-1', name: 'Brr', folders: ['/work/brr', '/work/docs'], primaryFolder: '/work/brr', revision: 1, model: null, effort: null, permission: null, hidden: false}
  return {
    projects: [project] as ProjectRecord[],
    threads: [
      {id: 'member', projectId: 'p-1', cwd: '/different', preview: 'Assigned chat', updatedAt: 1},
      {id: 'unassigned', projectId: null, cwd: '/work/brr', preview: 'Unassigned chat', updatedAt: 2},
    ] as any[],
    createProject: vi.fn(async (input: any) => ({...input, id: 'p-2', revision: 1, hidden: false})),
    saveProject: vi.fn(async (project: ProjectRecord) => project),
    deleteProject: vi.fn(async () => {}),
    archiveProject: vi.fn(async () => ({archivedIds: [], skipped: [], failed: []})),
    moveChat: vi.fn(async () => {}),
    start: vi.fn(async (_projectId: string | null, _model: string, _readOnly: boolean) => ({thread: {id: 'new-thread', projectId: 'p-1', cwd: '/work/brr', preview: '', updatedAt: 3}})),
    send: vi.fn(async (..._args: unknown[]) => ({turn: {id: 'turn-1'}})),
    subscribeHandler: null as null | ((event: any) => void),
    records: [] as any[],
  }
})

vi.mock('./lib/client', () => ({
  client: {
    connect: vi.fn(async () => ({account: {type: 'chatgpt', email: 'dev@example.com', planType: 'plus'}, models: [{id: 'm1', model: 'gpt-6.1-sol', displayName: 'Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{reasoningEffort: 'medium', description: ''}] }], threads: state.threads})),
    subscribe: vi.fn(async (handler: any) => { state.subscribeHandler = handler; return () => { state.subscribeHandler = null } }),
    projects: vi.fn(async () => ({data: state.projects})), threads: vi.fn(async () => state.threads),
    chooseWorkspace: vi.fn(async () => '/work/new'), chooseFolders: vi.fn(async () => ['/work/new']), revealProject: vi.fn(async () => {}),
    createProject: state.createProject, saveProject: state.saveProject, deleteProject: state.deleteProject, archiveProject: state.archiveProject, moveChat: state.moveChat,
    inspectProject: vi.fn(async () => ({commands: []})), start: state.start, resume: vi.fn(async (id: string) => ({thread: state.threads.find(thread => thread.id === id)})), send: state.send,
    approveWorktree: vi.fn(async () => {}), account: vi.fn(async () => null), logout: vi.fn(async () => {}), login: vi.fn(async () => ({loginId: 'login'})), cancelLogin: vi.fn(async () => ({})), interrupt: vi.fn(async () => {}), archive: vi.fn(async () => {}),
  }, isDesktop: () => true,
  WorktreeSetupRequired: class extends Error { kind = 'worktreeSetupRequired'; constructor(public projectId: string, public commands: any[]) { super('Review project setup hooks') } },
}))

vi.mock('./lib/chat', () => ({chat: {
  listDrafts: vi.fn(async () => state.records), getDraft: vi.fn(async (id: string) => state.records.find(item => item.id === id)),
  saveDraft: vi.fn(async (draft: any) => { const next = {...draft, revision: 1, updatedAt: new Date().toISOString()}; state.records = [...state.records.filter(item => item.id !== draft.id), next]; return next }),
  promoteDraft: vi.fn(async (id: string, threadId: string) => { const item = state.records.find(value => value.id === id); return {...item, id: threadId, threadId} }),
  submit: vi.fn(async ({threadId}: any) => ({turn: await state.send(threadId)})), attachments: vi.fn(async () => []), queue: vi.fn(async () => ({entries: [], state: {paused: false}})), skills: vi.fn(async () => []), references: vi.fn(async () => []), importFiles: vi.fn(async () => []), removeAttachment: vi.fn(async () => {}),
}}))
vi.mock('./lib/agents', () => ({agents: {
  list: vi.fn(async () => ({data: []})), decisions: vi.fn(async () => ({data: []})), tasks: vi.fn(async () => ({data: []})), drafts: vi.fn(async () => ({data: []})),
  questions: {list: vi.fn(async () => ({data: []})), saveDraft: vi.fn(), answer: vi.fn(), dismiss: vi.fn()},
  open: vi.fn(async () => ({thread: {id: 'agent-thread', projectId: 'p-1', cwd: '/work/brr', preview: '', updatedAt: 1}})), createTask: vi.fn(),
}}))
vi.mock('./components/ChatEditor', () => ({ChatEditor: React.forwardRef((props: any, ref: any) => {
  React.useImperativeHandle(ref, () => ({focus: () => {}, insertText: () => {}, insertQuote: () => {}, openMenu: () => {}}))
  return <textarea aria-label="Message" value={props.document.text} onChange={event => props.onChange({...props.document, text: event.target.value})} />
})}))
vi.mock('./components/TicketsPanel', () => ({TicketsPanel: () => null}))
vi.mock('./components/McpRequests', () => ({McpRequests: () => null}))

beforeEach(() => {
  localStorage.clear(); state.projects = [{id: 'p-1', name: 'Brr', folders: ['/work/brr', '/work/docs'], primaryFolder: '/work/brr', revision: 1, model: null, effort: null, permission: null, hidden: false}]
  state.threads = [
    {id: 'member', projectId: 'p-1', cwd: '/different', preview: 'Assigned chat', updatedAt: 1},
    {id: 'unassigned', projectId: null, cwd: '/work/brr', preview: 'Unassigned chat', updatedAt: 2},
  ]
  state.records = []; vi.clearAllMocks()
})

it('groups chats only by explicit project ID, never by a matching cwd', async () => {
  const user = userEvent.setup()
  render(<App />)
  await screen.findByRole('button', {name: 'New chat in Brr'})
  await user.click(screen.getByRole('button', {name: 'Expand chats in Brr'}))
  const projectGroup = screen.getByRole('button', {name: 'New chat in Brr'}).parentElement?.parentElement
  expect(within(projectGroup!).getByRole('button', {name: 'Open Assigned chat'})).toBeInTheDocument()
  expect(within(projectGroup!).queryByRole('button', {name: 'Open Unassigned chat'})).not.toBeInTheDocument()
})

it('creates a project from the dialog and selects its welcome chat', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(await screen.findByRole('button', {name: 'New project'}))
  await user.type(screen.getByRole('textbox', {name: 'Project name'}), 'Docs')
  await user.click(screen.getByRole('button', {name: 'Add folders'}))
  await user.click(screen.getByRole('button', {name: 'Create project'}))
  await waitFor(() => expect(state.createProject).toHaveBeenCalledWith({name: 'Docs', folders: ['/work/new'], primaryFolder: '/work/new', model: null, effort: null, permission: null}))
  expect(await screen.findByRole('heading', {name: 'New chat in Docs'})).toBeInTheDocument()
  expect(screen.getByText('/work/new')).toBeInTheDocument()
})

it('keeps the rich draft when switching project scope and starts the selected project', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(await screen.findByRole('button', {name: 'New chat in Brr'}))
  const message = screen.getByRole('textbox', {name: 'Message'})
  fireEvent.change(message, {target: {value: 'Keep this draft'}})
  await user.selectOptions(screen.getByRole('combobox', {name: 'Project'}), '')
  expect(message).toHaveValue('Keep this draft')
  await user.selectOptions(screen.getByRole('combobox', {name: 'Project'}), 'p-1')
  expect(message).toHaveValue('Keep this draft')
  await user.click(screen.getByRole('button', {name: 'Send message'}))
  await waitFor(() => expect(state.start).toHaveBeenCalledWith('p-1', 'gpt-6.1-sol', false))
})

it('moves a chat to No project without changing the conversation and carries that membership to New chat', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(await screen.findByRole('button', {name: 'Assigned chat'}))
  await user.selectOptions(await screen.findByRole('combobox', {name: 'Move chat to project'}), '')
  await waitFor(() => expect(state.moveChat).toHaveBeenCalledWith('member', null))
  expect(screen.getByRole('button', {name: 'Archive'})).toBeInTheDocument()
  await user.click(screen.getByRole('button', {name: 'New conversation'}))
  expect(screen.getByRole('combobox', {name: 'Project'})).toHaveValue('')
})

it('does not change the selected draft when a pending move resolves after navigation', async () => {
  const user = userEvent.setup()
  let resolveMove!: () => void
  state.moveChat.mockImplementationOnce(() => new Promise<void>(resolve => { resolveMove = resolve }))
  render(<App />)
  await user.click(await screen.findByRole('button', {name: 'Assigned chat'}))
  const moveChoice = await screen.findByRole('combobox', {name: 'Move chat to project'})
  await user.selectOptions(moveChoice, '')
  expect(moveChoice).toBeDisabled()
  await user.click(screen.getByRole('button', {name: 'New conversation'}))
  expect(screen.getByRole('combobox', {name: 'Project'})).toHaveValue('p-1')
  await act(async () => { resolveMove() })
  await waitFor(() => expect(state.moveChat).toHaveBeenCalledWith('member', null))
  expect(screen.getByRole('combobox', {name: 'Project'})).toHaveValue('p-1')
})

it('deletes a project by stable ID without archiving its chats', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(await screen.findByRole('button', {name: 'Manage project Brr'}))
  await user.click(await screen.findByRole('menuitem', {name: 'Delete project'}))
  expect(screen.getByRole('heading', {name: 'Delete project Brr?'})).toBeInTheDocument()
  await user.click(screen.getByRole('button', {name: 'Delete project'}))
  await waitFor(() => expect(state.deleteProject).toHaveBeenCalledWith('p-1'))
  expect(state.archiveProject).not.toHaveBeenCalled()
})

it('offers Move to project in the shared chat options menu', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(await screen.findByRole('button', {name: 'Assigned chat'}))
  await user.click(screen.getByRole('button', {name: 'Chat options'}))
  const menu = screen.getByRole('dialog', {name: 'Chat options'})
  await user.selectOptions(within(menu).getByRole('combobox', {name: 'Move to project'}), '')
  await waitFor(() => expect(state.moveChat).toHaveBeenCalledWith('member', null))
})
