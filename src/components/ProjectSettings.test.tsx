import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it, vi } from 'vitest'
import { ProjectSettings } from './ProjectSettings'

const project = {id: 'p1', revision: 1, folders: ['/work/brr'], primaryFolder: '/work/brr', name: 'brr', model: null, effort: null, permission: null, hidden: false}
const models = [{id: 'sol', model: 'gpt-6.1-sol', displayName: 'GPT Sol', isDefault: true, defaultReasoningEffort: 'medium', supportedReasoningEfforts: [{reasoningEffort: 'medium', description: ''}, {reasoningEffort: 'high', description: ''}]}]

it('edits project identity and new-chat defaults without changing its folder', async () => {
  const user = userEvent.setup()
  const save = vi.fn(async () => {})
  render(<ProjectSettings project={project} models={models} onSave={save} onClose={vi.fn()} />)
  await user.clear(screen.getByRole('textbox', {name: 'Project name'}))
  await user.type(screen.getByRole('textbox', {name: 'Project name'}), 'Brr desktop')
  await user.click(screen.getByText('New chat defaults'))
  await user.selectOptions(screen.getByRole('combobox', {name: 'Default model'}), 'gpt-6.1-sol')
  await user.selectOptions(screen.getByRole('combobox', {name: 'Default reasoning'}), 'high')
  await user.selectOptions(screen.getByRole('combobox', {name: 'Default permissions'}), 'read-only')
  await user.click(screen.getByRole('button', {name: 'Save changes'}))
  await waitFor(() => expect(save).toHaveBeenCalledWith({...project, name: 'Brr desktop', model: 'gpt-6.1-sol', effort: 'high', permission: 'read-only'}))
})
