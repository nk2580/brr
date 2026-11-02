import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import App from './App'

vi.mock('./lib/client', () => ({ isDesktop: () => false, client: {} }))

beforeEach(() => localStorage.clear())
afterEach(() => vi.restoreAllMocks())

it('collapses and reopens the main sidebar without losing the message draft', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Keep working on this')
  const toggle = screen.getByRole('button', { name: 'Collapse sidebar' })
  expect(toggle).toHaveAttribute('aria-expanded', 'true')
  await user.click(toggle)

  expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
  expect(screen.queryByRole('button', { name: 'New conversation' })).not.toBeInTheDocument()
  expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep working on this')
  const expand = screen.getByRole('button', { name: 'Expand sidebar' })
  expect(expand).toHaveAttribute('aria-expanded', 'false')
  expect(expand).toHaveFocus()
  await user.keyboard(' ')

  expect(screen.getByRole('complementary', { name: 'Main sidebar' })).toBeVisible()
  expect(screen.getByRole('button', { name: 'New conversation' })).toBeVisible()
  expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep working on this')
})

it('keeps only the five fixed rail destinations and never collapses the sidebar through Home', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(screen.getByRole('button', { name: 'Collapse sidebar' }))

  const rail = screen.getByRole('navigation', { name: 'Main rail' })
  expect(rail).toBeVisible()
  expect(within(rail).getAllByRole('button').map(button => button.getAttribute('aria-label'))).toEqual(['Home', 'Agents', 'Plugins', 'Profile', 'Settings'])
  expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
  const home = screen.getByRole('button', { name: 'Home' })
  expect(home).not.toHaveAttribute('aria-expanded')
  expect(home).not.toHaveAttribute('aria-controls', 'main-sidebar')
  await user.click(screen.getByRole('button', { name: 'Settings' }))
  await user.click(home)
  expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
  expect(screen.queryByRole('region', { name: 'Application settings' })).not.toBeInTheDocument()
  expect(screen.getByRole('textbox', { name: 'Message' })).toBeVisible()
  await user.click(home)
  expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
  await user.click(screen.getByRole('button', { name: 'Expand sidebar' }))
  await user.click(home)
  expect(screen.getByRole('complementary', { name: 'Main sidebar' })).toBeVisible()
})

it('keeps chat search inside the expandable sidebar', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(screen.getByRole('button', { name: 'Collapse sidebar' }))
  expect(within(screen.getByRole('navigation', { name: 'Main rail' })).queryByRole('button', { name: 'Search chats' })).not.toBeInTheDocument()
  await user.click(screen.getByRole('button', { name: 'Expand sidebar' }))
  await user.click(screen.getByRole('button', { name: 'Search chats' }))

  expect(screen.getByRole('complementary', { name: 'Main sidebar' })).toBeVisible()
  expect(screen.getByRole('textbox', { name: 'Search chats' })).toHaveFocus()
})

it('remembers collapsed and expanded layouts while preserving the chosen sidebar color', async () => {
  const user = userEvent.setup()
  const first = render(<App />)
  await user.click(screen.getByRole('button', { name: 'Settings' }))
  await user.click(screen.getByRole('radio', { name: 'Violet' }))
  await user.click(screen.getByRole('button', { name: 'Close settings' }))
  await user.click(screen.getByRole('button', { name: 'Collapse sidebar' }))
  first.unmount()

  const second = render(<App />)
  await user.click(screen.getByRole('button', { name: 'Expand sidebar' }))
  await user.click(screen.getByRole('button', { name: 'Settings' }))
  expect(screen.getByRole('radio', { name: 'Violet' })).toBeChecked()
  second.unmount()

  render(<App />)
  expect(screen.getByRole('button', { name: 'Collapse sidebar' })).toHaveAttribute('aria-expanded', 'true')
  expect(screen.getByRole('button', { name: 'Settings' })).toBeVisible()
})

it('returns focus to the always-visible settings control when closing settings', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(screen.getByRole('button', { name: 'Settings' }))
  await user.click(screen.getByRole('button', { name: 'Collapse sidebar' }))
  expect(screen.getByRole('region', { name: 'Application settings' })).toBeVisible()
  await user.click(screen.getByRole('button', { name: 'Close settings' }))
  expect(screen.getByRole('button', { name: 'Settings' })).toHaveFocus()
  expect(screen.getByRole('navigation', { name: 'Main rail' })).toBeVisible()
})

it('opens the profile without changing the sidebar and restores focus when dismissed', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(screen.getByRole('button', { name: 'Collapse sidebar' }))
  await user.click(screen.getByRole('button', { name: 'Profile' }))

  expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
  expect(screen.getByRole('dialog', { name: 'User profile' })).toHaveFocus()
  expect(screen.getByRole('button', { name: 'Sign in with ChatGPT' })).toBeVisible()
  await user.keyboard('{Escape}')
  expect(screen.queryByRole('dialog', { name: 'User profile' })).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Profile' })).toHaveFocus()
  expect(screen.getByRole('button', { name: 'Expand sidebar' })).toBeVisible()
})
