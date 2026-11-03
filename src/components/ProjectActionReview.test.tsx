import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it, vi } from 'vitest'
import type { ProjectArchiveResult, ProjectRecord } from '../lib/client'
import { ProjectActionReview } from './ProjectActionReview'

const project: ProjectRecord = {
  id: 'project-brr', revision: 1, folders: ['/work/Brr'], primaryFolder: '/work/Brr', name: 'Brr', model: null, effort: null, permission: null, hidden: false,
}

const renderReview = (overrides: Partial<React.ComponentProps<typeof ProjectActionReview>> = {}) => {
  const props: React.ComponentProps<typeof ProjectActionReview> = {
    project, action: 'archive', chatCount: 3, pending: false, result: null, error: '',
    onConfirm: vi.fn(), onClose: vi.fn(), ...overrides,
  }
  return { ...render(<ProjectActionReview {...props} />), props }
}

it('shows archive consequences without acting until the user confirms', async () => {
  const user = userEvent.setup()
  const { props } = renderReview()
  const review = screen.getByRole('region', { name: 'Archive chats in Brr' })
  expect(review).toHaveFocus()
  expect(screen.getByText((_, element) => !!element?.classList.contains('project-action-count'))).toHaveTextContent('Brr currently lists 3 chats. The count is checked again when archiving starts.')
  expect(screen.getByText('Active chats and background tasks stay as they are and are skipped until they are resolved.')).toBeInTheDocument()
  expect(props.onConfirm).not.toHaveBeenCalled()

  await user.click(screen.getByRole('button', { name: 'Archive chats' }))
  expect(props.onConfirm).toHaveBeenCalledOnce()
})

it('describes project deletion as ungrouping chats without archiving or removing files', () => {
  renderReview({ action: 'delete' })
  expect(screen.getByRole('heading', { name: 'Delete project Brr?' })).toBeInTheDocument()
  expect(screen.getByText('Delete this project from Brr. Its chats will become ungrouped. Project files and worktrees stay on disk.')).toBeInTheDocument()
  expect(screen.getByText('Deleting a project does not archive chats or delete project files.')).toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Delete project' })).toBeInTheDocument()
})

it('disables confirmation and closing while work is pending, and ignores Escape', async () => {
  const user = userEvent.setup()
  const { props } = renderReview({ pending: true, action: 'delete' })
  const confirm = screen.getByRole('button', { name: 'Deleting project…' })
  const cancel = screen.getByRole('button', { name: 'Cancel' })
  expect(confirm).toBeDisabled()
  expect(cancel).toBeDisabled()
  await user.keyboard('{Escape}')
  expect(props.onClose).not.toHaveBeenCalled()
  await user.click(confirm)
  expect(props.onConfirm).not.toHaveBeenCalled()
})

it('guards against duplicate confirmation before the parent reports pending', async () => {
  const user = userEvent.setup()
  const { props } = renderReview()
  const confirm = screen.getByRole('button', { name: 'Archive chats' })
  await user.click(confirm)
  expect(confirm).toBeDisabled()
  await user.click(confirm)
  expect(props.onConfirm).toHaveBeenCalledOnce()
})

it('lists partial outcomes and lets the user retry remaining chats', async () => {
  const user = userEvent.setup()
  const result: ProjectArchiveResult = {
    archivedIds: ['archived-1'],
    skipped: [{ threadId: 'active-2', reason: 'An active turn is still running' }],
    failed: [{ threadId: 'chat-3', error: 'The conversation could not be archived' }],
  }
  const { props } = renderReview({ result })
  expect(screen.getByText('1 chat archived.')).toBeInTheDocument()
  expect(screen.getByRole('list', { name: 'Skipped chats' })).toHaveTextContent('Conversation active-2: An active turn is still running')
  expect(screen.getByRole('list', { name: 'Chats that could not be archived' })).toHaveTextContent('Conversation chat-3: The conversation could not be archived')
  expect(screen.queryByText(/transcript|message content/i)).not.toBeInTheDocument()
  await user.click(screen.getByRole('button', { name: 'Retry remaining chats' }))
  expect(props.onConfirm).toHaveBeenCalledOnce()
})

it('shows backend errors accessibly and closes on Escape with the review focused', async () => {
  const user = userEvent.setup()
  const { props } = renderReview({ error: 'The project could not be removed' })
  const review = screen.getByRole('region', { name: 'Archive chats in Brr' })
  expect(screen.getByRole('alert')).toHaveTextContent('The project could not be removed')
  expect(review).toHaveFocus()
  await user.keyboard('{Escape}')
  expect(props.onClose).toHaveBeenCalledOnce()
  expect(review).toHaveFocus()
})
