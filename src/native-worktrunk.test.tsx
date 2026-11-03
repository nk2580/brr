import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import App from './App'

vi.mock('./components/ChatEditor', async () => {
  const React = await import('react')
  const ChatEditor = React.forwardRef<any, any>((props, ref) => {
    const input = React.useRef<HTMLTextAreaElement>(null)
    const publish = (text: string) => props.onChange({version: 1, text, references: [], quotes: []})
    React.useImperativeHandle(ref, () => ({
      focus: () => input.current?.focus(),
      insertText: (value: string) => publish(`${props.document.text}${props.document.text ? '\n\n' : ''}${value}`),
      insertQuote: (value: string) => publish(`${props.document.text}${props.document.text ? ' ' : ''}“${value}”`),
      openMenu: () => {},
    }), [props.document])
    return <textarea aria-label="Message" placeholder="Ask anything" ref={input} value={props.document.text} onChange={event => publish(event.target.value)} onKeyDown={event => {
      if (event.key === 'Tab' && event.shiftKey) {event.preventDefault(); props.onTogglePlan()}
      else if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {event.preventDefault(); props.onSubmit(true)}
      else if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {event.preventDefault(); props.onSubmit(false)}
    }} />
  })
  return {ChatEditor}
})

const backend = vi.hoisted(() => ({
  connect: vi.fn(async () => ({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [] })),
  subscribe: vi.fn(async () => () => {}),
  projects: vi.fn(async () => ({data: [{id: 'project-main', name: 'Project', folders: ['/project'], primaryFolder: '/project', revision: 1, model: null, effort: null, permission: null, hidden: false}]})),
  inspectProject: vi.fn(async () => ({commands: []})),
  start: vi.fn(),
  approveWorktree: vi.fn(async () => ({})),
  send: vi.fn(async () => ({ turn: { id: 'turn-1' } })),
}))
vi.mock('./lib/client', () => ({ client: backend, isDesktop: () => true }))

const setupError = (commands = [
  { cwd: '/project', phase: 'post-create', name: 'Install dependencies', template: 'pnpm install', approved: false },
  { cwd: '/project', phase: 'post-create', name: 'Prepare project', template: 'git status --short', approved: false },
]) => Object.assign(new Error('Worktree setup approval required'), {
  kind: 'worktreeSetupRequired', projectId: 'project-main', commands,
})

describe('native WorkTrunk setup review', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    backend.connect.mockResolvedValue({ account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' }, models: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }], threads: [] } as never)
    backend.inspectProject.mockResolvedValue({commands: []})
    backend.start.mockRejectedValue(setupError())
  })

  it('shows the exact pending hook commands and keeps the message for a later send', async () => {
    const user = userEvent.setup()
    render(<App />)
    await user.selectOptions(await screen.findByRole('combobox', {name: 'Project'}), 'project-main')
    const draft = screen.getByRole('textbox', { name: 'Message' })
    await user.type(draft, 'Fix the login bug')
    await user.click(screen.getByRole('button', { name: 'Send message' }))

    expect(await screen.findByRole('region', { name: 'Worktree setup approval' })).toBeInTheDocument()
    expect(screen.getByText('pnpm install')).toBeInTheDocument()
    expect(screen.getByText('git status --short')).toBeInTheDocument()
    expect(screen.getByText('post-create · Install dependencies')).toBeInTheDocument()
    expect(draft).toHaveValue('Fix the login bug')
    expect(backend.send).not.toHaveBeenCalled()
  })

  it('approves only the displayed hooks for the inspected project without sending', async () => {
    const user = userEvent.setup()
    render(<App />)
    await user.selectOptions(await screen.findByRole('combobox', {name: 'Project'}), 'project-main')
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Continue')
    await user.click(screen.getByRole('button', { name: 'Send message' }))
    await user.click(await screen.findByRole('button', { name: 'Approve setup hooks' }))

    await waitFor(() => expect(backend.approveWorktree).toHaveBeenCalledWith('/project', ['pnpm install', 'git status --short']))
    expect(backend.send).not.toHaveBeenCalled()
    expect(screen.queryByRole('region', { name: 'Worktree setup approval' })).not.toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Continue')
  })

  it('cancels setup review without approving hooks or sending', async () => {
    const user = userEvent.setup()
    render(<App />)
    await user.selectOptions(await screen.findByRole('combobox', {name: 'Project'}), 'project-main')
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Continue')
    await user.click(screen.getByRole('button', { name: 'Send message' }))
    await user.click(await screen.findByRole('button', { name: 'Cancel setup' }))

    expect(backend.approveWorktree).not.toHaveBeenCalled()
    expect(backend.send).not.toHaveBeenCalled()
    expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Continue')
  })

  it('ignores an inspection result after the selected project changes', async () => {
    const user = userEvent.setup()
    let rejectStart!: (reason: Error) => void
    backend.start.mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectStart = reject }) as never)
    backend.projects.mockResolvedValueOnce({data: [
      {id: 'project-main', name: 'Project', folders: ['/project'], primaryFolder: '/project', revision: 1, model: null, effort: null, permission: null, hidden: false},
      {id: 'project-other', name: 'Other project', folders: ['/other-project'], primaryFolder: '/other-project', revision: 1, model: null, effort: null, permission: null, hidden: false},
    ]})
    render(<App />)
    await user.selectOptions(await screen.findByRole('combobox', {name: 'Project'}), 'project-main')
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Continue')
    await user.click(screen.getByRole('button', { name: 'Send message' }))
    await user.selectOptions(screen.getByRole('combobox', {name: 'Project'}), 'project-other')
    rejectStart(setupError())

    expect(screen.getByRole('combobox', {name: 'Project'})).toHaveValue('project-other')
    expect(screen.queryByRole('region', { name: 'Worktree setup approval' })).not.toBeInTheDocument()
    expect(backend.approveWorktree).not.toHaveBeenCalled()
  })

  it('keeps a newer setup review when an earlier approval resolves late', async () => {
    const user = userEvent.setup()
    let resolveApproval!: (value: {}) => void
    backend.approveWorktree.mockImplementationOnce(() => new Promise(resolve => { resolveApproval = resolve }) as never)
    render(<App />)
    await user.selectOptions(await screen.findByRole('combobox', {name: 'Project'}), 'project-main')
    await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Continue')
    await user.click(screen.getByRole('button', { name: 'Send message' }))
    await screen.findByRole('region', { name: 'Worktree setup approval' })

    backend.start.mockRejectedValueOnce(setupError([
      { cwd: '/project', phase: 'post-start', name: 'New hook', template: 'pnpm test', approved: false },
    ]))
    await user.click(screen.getByRole('button', { name: 'Approve setup hooks' }))
    await user.click(screen.getByRole('button', { name: 'Send message' }))
    expect(await screen.findByText('pnpm test')).toBeInTheDocument()

    resolveApproval({})

    expect(await screen.findByText('pnpm test')).toBeInTheDocument()
    expect(screen.getByRole('region', { name: 'Worktree setup approval' })).toBeInTheDocument()
  })
})
