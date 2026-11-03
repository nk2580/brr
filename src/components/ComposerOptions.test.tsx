import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { expect, it } from 'vitest'
import { ComposerOptions } from './ComposerOptions'

it('opens chat options and keeps the parent selection when the panel stays open', async () => {
  const user = userEvent.setup()
  render(<ComposerOptions>
    <label>Model<select aria-label="Model"><option>Fast</option><option>Deep</option></select></label>
  </ComposerOptions>)

  const trigger = screen.getByRole('button', { name: 'Chat options' })
  await user.tab()
  expect(trigger).toHaveFocus()
  await user.keyboard('{Enter}')

  const dialog = screen.getByRole('dialog', { name: 'Chat options' })
  const model = screen.getByRole('combobox', { name: 'Model' })
  expect(dialog).toBeVisible()
  expect(trigger).toHaveAttribute('aria-expanded', 'true')
  expect(trigger).toHaveAttribute('aria-haspopup', 'dialog')
  expect(model).toHaveFocus()

  await user.selectOptions(model, 'Deep')
  expect(model).toHaveValue('Deep')
  expect(screen.getByRole('dialog', { name: 'Chat options' })).toBeInTheDocument()
})

it('restores focus on Escape and dismisses on outside pointer without moving focus', async () => {
  const user = userEvent.setup()
  render(<><ComposerOptions><label>Mode<select aria-label="Mode"><option>Auto</option></select></label></ComposerOptions><button>Outside</button></>)
  const trigger = screen.getByRole('button', { name: 'Chat options' })
  const outside = screen.getByRole('button', { name: 'Outside' })

  await user.tab()
  await user.keyboard('{Enter}')
  expect(screen.getByRole('combobox', { name: 'Mode' })).toHaveFocus()
  await user.keyboard('{Escape}')
  expect(screen.queryByRole('dialog', { name: 'Chat options' })).not.toBeInTheDocument()
  expect(trigger).toHaveFocus()

  await user.keyboard('{Enter}')
  outside.focus()
  await user.pointer({ keys: '[MouseLeft]', target: outside })
  expect(screen.queryByRole('dialog', { name: 'Chat options' })).not.toBeInTheDocument()
  expect(outside).toHaveFocus()
})

it('continues Tab from the portalled fields to the next outside control', async () => {
  const user = userEvent.setup()
  render(<><ComposerOptions><label>Mode<select aria-label="Mode"><option>Auto</option></select></label></ComposerOptions><button>Next</button></>)
  const trigger = screen.getByRole('button', { name: 'Chat options' })
  const next = screen.getByRole('button', { name: 'Next' })
  await user.tab()
  await user.keyboard('{Enter}')
  await user.tab()
  expect(screen.getByRole('button', { name: 'Close chat options' })).toHaveFocus()
  await user.tab()
  expect(next).toHaveFocus()
  expect(screen.queryByRole('dialog', { name: 'Chat options' })).not.toBeInTheDocument()
  expect(trigger).toHaveAttribute('aria-expanded', 'false')
})

it('returns Shift+Tab from the first option field to its trigger', async () => {
  const user = userEvent.setup()
  render(<ComposerOptions><label>Mode<select aria-label="Mode"><option>Auto</option></select></label></ComposerOptions>)
  const trigger = screen.getByRole('button', { name: 'Chat options' })
  await user.tab()
  await user.keyboard('{Enter}')
  await user.keyboard('{Shift>}{Tab}{/Shift}')
  expect(trigger).toHaveFocus()
  expect(screen.getByRole('dialog', { name: 'Chat options' })).toBeInTheDocument()
  await user.tab()
  expect(screen.getByRole('combobox', { name: 'Mode' })).toHaveFocus()
})

it('returns Shift+Tab from the close button to the last option field', async () => {
  const user = userEvent.setup()
  render(<ComposerOptions><label>Mode<select aria-label="Mode"><option>Auto</option></select></label><label>Effort<select aria-label="Effort"><option>Balanced</option></select></label></ComposerOptions>)
  await user.tab()
  await user.keyboard('{Enter}')
  await user.tab()
  await user.tab()
  const close = screen.getByRole('button', { name: 'Close chat options' })
  const lastField = screen.getByRole('combobox', { name: 'Effort' })
  expect(close).toHaveFocus()

  await user.keyboard('{Shift>}{Tab}{/Shift}')

  expect(lastField).toHaveFocus()
  expect(screen.getByRole('dialog', { name: 'Chat options' })).toBeInTheDocument()
})

it('wraps Tab from close to the first outside control when there are no options or next controls', async () => {
  const user = userEvent.setup()
  render(<><button>Before A</button><button>Before B</button><ComposerOptions>{null}</ComposerOptions></>)
  const firstBefore = screen.getByRole('button', { name: 'Before A' })
  const trigger = screen.getByRole('button', { name: 'Chat options' })
  await user.click(trigger)
  const close = screen.getByRole('button', { name: 'Close chat options' })
  close.focus()

  await user.tab()

  expect(firstBefore).toHaveFocus()
  expect(screen.queryByRole('dialog', { name: 'Chat options' })).not.toBeInTheDocument()
})
