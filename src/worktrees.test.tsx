import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import App from './App'

const backend = vi.hoisted(() => ({
  connect: vi.fn(async () => ({
    account: { type: 'chatgpt', email: 'dev@example.com', planType: 'plus' },
    models: [{ id: 'sol', model: 'gpt-6.1-sol', displayName: 'Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [] }],
    threads: [{ id: 'saved', preview: 'Isolated task', cwd: '/project.worktree', updatedAt: 1,
      worktree: { projectPath: '/project', path: '/project.worktree', cwd: '/project.worktree', branch: 'codex/brr-saved' } }],
  })),
  subscribe: vi.fn(async () => () => {}),
  resume: vi.fn(async () => ({ thread: { id: 'saved', preview: 'Isolated task', cwd: '/project.worktree', updatedAt: 1, turns: [],
    worktree: { projectPath: '/project', path: '/project.worktree', cwd: '/project.worktree', branch: 'codex/brr-saved' } } })),
  start: vi.fn(async () => ({ thread: { id: 'new', preview: '', cwd: '/project.new', updatedAt: 2,
    worktree: { projectPath: '/project', path: '/project.new', cwd: '/project.new', branch: 'codex/brr-new' } } })),
  send: vi.fn(async () => ({ turn: { id: 'turn-new' } })),
  chooseWorkspace: vi.fn(async () => '/project'),
}))
vi.mock('./lib/client', () => ({ client: backend, isDesktop: () => true }))

describe('project worktree sessions', () => {
  it('starts the next conversation from the original project after opening a saved worktree', async () => {
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Isolated task' }))
    await waitFor(() => expect(backend.resume).toHaveBeenCalledWith('saved'))
    await userEvent.click(screen.getByRole('button', { name: 'New conversation' }))
    await userEvent.type(screen.getByRole('textbox', { name: 'Message' }), 'Another isolated task')
    await userEvent.click(screen.getByRole('button', { name: 'Send message' }))
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith('/project', 'gpt-6.1-sol', false))
  })

  it('shows the saved worktree branch and full path while keeping the project visible', async () => {
    render(<App />)
    await userEvent.click(await screen.findByRole('button', { name: 'Isolated task' }))
    expect(await screen.findByLabelText('Session worktree')).toHaveTextContent('codex/brr-saved')
    expect(screen.getByLabelText('Session worktree')).toHaveAttribute('title', '/project.worktree')
    expect(screen.getByTitle('/project')).toHaveTextContent('project')
  })
})
