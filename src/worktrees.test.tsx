import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
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
  connect: vi.fn(async () => ({
    account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' },
    models: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }],
    threads: [{ id: 'saved', projectId: 'project-main', preview: 'Isolated task', cwd: '/project.worktree', updatedAt: 1,
      workspace: {cwd: '/project.worktree', roots: ['/project.worktree'], scopeProjectId: 'project-main', projectRevision: 3, folders: [], worktrees: []},
      worktree: { projectPath: '/project', path: '/project.worktree', cwd: '/project.worktree', branch: 'codex/brr-saved' } }],
  })),
  subscribe: vi.fn(async () => () => {}),
  resume: vi.fn(async () => ({ thread: { id: 'saved', projectId: 'project-main', preview: 'Isolated task', cwd: '/project.worktree', updatedAt: 1, turns: [],
    workspace: {cwd: '/project.worktree', roots: ['/project.worktree'], scopeProjectId: 'project-main', projectRevision: 3, folders: [], worktrees: []},
    worktree: { projectPath: '/project', path: '/project.worktree', cwd: '/project.worktree', branch: 'codex/brr-saved' } } })),
  start: vi.fn(async () => ({ thread: { id: 'new', projectId: 'project-main', preview: '', cwd: '/project.new', updatedAt: 2,
    workspace: {cwd: '/project.new', roots: ['/project.new'], scopeProjectId: 'project-main', projectRevision: 3, folders: [], worktrees: []},
    worktree: { projectPath: '/project', path: '/project.new', cwd: '/project.new', branch: 'codex/brr-new' } } })),
  send: vi.fn(async () => ({ turn: { id: 'turn-new' } })),
  projects: vi.fn(async () => ({data: [{id: 'project-main', name: 'project', folders: ['/project'], primaryFolder: '/project', revision: 1, model: null, effort: null, permission: null, hidden: false}]})),
  inspectProject: vi.fn(async () => ({commands: []})),
}))
vi.mock('./lib/client', () => ({ client: backend, isDesktop: () => true }))

describe('project worktree sessions', () => {
  it('starts the next conversation from the original project after opening a saved worktree', async () => {
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Isolated task' }))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('saved'))
    await userEvent.click(screen.getByRole('button', { name: 'New conversation' }))
    await userEvent.selectOptions(await screen.findByRole('combobox', {name: 'Project'}), 'project-main')
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Another isolated task')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith('project-main', 'gpt-6.1-sol', false))
  })

  it('shows the saved worktree branch and full path while keeping the project visible', async () => {
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Isolated task' }))
    await userEvent.click(screen.getByRole('button', { name: 'Chat options' }))
    expect(await screen.findByLabelText('Session worktree')).toHaveTextContent('codex/brr-saved')
    expect(screen.getByLabelText('Session worktree')).toHaveAttribute('title', '/project.worktree')
    expect(screen.getByRole('button', {name: 'Manage project project'})).toBeVisible()
  })
})
