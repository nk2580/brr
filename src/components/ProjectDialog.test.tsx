import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it, vi } from 'vitest'
import { ProjectDialog } from './ProjectDialog'
import type { Model } from '../lib/client'

const models: Model[] = [{id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{reasoningEffort: 'medium', description: ''}, {reasoningEffort: 'high', description: ''}]}]

it('creates a project from the name and selected folders', async () => {
  const user = userEvent.setup()
  const onSave = vi.fn(async () => {})
  render(<ProjectDialog models={models} onSave={onSave} onChooseFolders={async () => ['/work/one']} onClose={vi.fn()} />)
  expect(screen.getByRole('dialog', {name: 'Create project'})).toBeInTheDocument()
  expect(screen.getByRole('textbox', {name: 'Project name'})).toHaveFocus()
  await user.type(screen.getByRole('textbox', {name: 'Project name'}), 'One')
  await user.click(screen.getByRole('button', {name: 'Add folders'}))
  await user.click(screen.getByRole('button', {name: 'Create project'}))
  await waitFor(() => expect(onSave).toHaveBeenCalledWith({name: 'One', folders: ['/work/one'], primaryFolder: '/work/one', model: null, effort: null, permission: null}))
})

it('rejects duplicate folders, chooses a primary, and selects the first remaining after removal', async () => {
  const user = userEvent.setup()
  render(<ProjectDialog models={models} onSave={vi.fn(async () => {})} onChooseFolders={async () => ['/work/a', '/work/a', '/work/b']} onClose={vi.fn()} />)
  await user.type(screen.getByRole('textbox', {name: 'Project name'}), 'Project')
  await user.click(screen.getByRole('button', {name: 'Add folders'}))
  expect(screen.getByRole('alert')).toHaveTextContent('already added')
  const radios = screen.getAllByRole('radio', {name: /primary folder/i})
  expect(radios[0]).toBeChecked()
  await user.click(radios[1])
  expect(radios[1]).toBeChecked()
  await user.click(screen.getByRole('button', {name: 'Remove /work/b'}))
  expect(screen.queryAllByRole('radio')).toHaveLength(0)
  expect(screen.getByText('/work/a')).toBeInTheDocument()
})

it('keeps a draft on save failure and permits retry; Escape cancels only while idle', async () => {
  const user = userEvent.setup()
  const onClose = vi.fn()
  const onSave = vi.fn().mockRejectedValueOnce(new Error('Registry unavailable')).mockResolvedValue(undefined)
  render(<ProjectDialog models={models} onSave={onSave} onChooseFolders={async () => null} onClose={onClose} />)
  await user.type(screen.getByRole('textbox', {name: 'Project name'}), 'Draft')
  await user.keyboard('{Escape}')
  expect(onClose).toHaveBeenCalledOnce()
  onClose.mockClear()
  await user.click(screen.getByRole('button', {name: 'Create project'}))
  expect(await screen.findByRole('alert')).toHaveTextContent('Registry unavailable')
  expect(screen.getByRole('textbox', {name: 'Project name'})).toHaveValue('Draft')
  await user.click(screen.getByRole('button', {name: 'Create project'}))
  await waitFor(() => expect(onSave).toHaveBeenCalledTimes(2))
})

it('prevents Escape dismissal during a pending save', async () => {
  const user = userEvent.setup()
  let finish!: () => void
  const onClose = vi.fn()
  const onSave = vi.fn(() => new Promise<void>(resolve => { finish = resolve }))
  render(<ProjectDialog models={models} onSave={onSave} onChooseFolders={async () => null} onClose={onClose} />)
  await user.type(screen.getByRole('textbox', {name: 'Project name'}), 'Saving')
  await user.click(screen.getByRole('button', {name: 'Create project'}))
  await user.keyboard('{Escape}')
  expect(onClose).not.toHaveBeenCalled()
  expect(screen.getByRole('button', {name: 'Cancel'})).toBeDisabled()
  finish()
  await waitFor(() => expect(screen.getByRole('button', {name: 'Cancel'})).toBeEnabled())
})

it('shows editable new-chat defaults when editing a project', async () => {
  const user = userEvent.setup()
  const project = {id: 'p1', name: 'Existing', folders: ['/work/a'], primaryFolder: '/work/a', revision: 2, model: 'retired-model', effort: 'high', permission: 'read-only' as const, hidden: false}
  render(<ProjectDialog project={project} models={models} onSave={vi.fn(async () => {})} onChooseFolders={async () => null} onClose={vi.fn()} />)
  const details = screen.getByText('New chat defaults').closest('details')!
  expect(details).not.toHaveAttribute('open')
  await user.click(screen.getByText('New chat defaults'))
  expect(within(details).getByRole('combobox', {name: 'Default model'})).toHaveValue('retired-model')
  expect(within(details).getByRole('alert')).toHaveTextContent(/unavailable/i)
})

it('keeps keyboard focus inside the dialog and restores the opener after cancellation', async () => {
  const user = userEvent.setup()
  const opener = document.createElement('button')
  document.body.append(opener)
  opener.focus()
  const onClose = vi.fn()
  const view = render(<ProjectDialog models={models} onSave={vi.fn(async () => {})} onChooseFolders={async () => null} onClose={onClose} />)
  expect(screen.getByRole('textbox', {name: 'Project name'})).toHaveFocus()
  expect(screen.getByRole('button', {name: 'Create project'})).toBeDisabled()
  await user.tab({shift: true})
  expect(screen.getByRole('button', {name: 'Close project dialog'})).toHaveFocus()
  await user.tab({shift: true})
  expect(screen.getByRole('button', {name: 'Cancel'})).toHaveFocus()
  await user.tab()
  expect(screen.getByRole('button', {name: 'Close project dialog'})).toHaveFocus()
  await user.keyboard('{Escape}')
  expect(onClose).toHaveBeenCalledOnce()
  view.unmount()
  expect(opener).toHaveFocus()
  opener.remove()
})
