import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
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

it('keeps the six fixed rail destinations and never collapses the sidebar through Home', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(screen.getByRole('button', { name: 'Collapse sidebar' }))

  const rail = screen.getByRole('navigation', { name: 'Main rail' })
  expect(rail).toBeVisible()
  expect(within(rail).getAllByRole('button').map(button => button.getAttribute('aria-label'))).toEqual(['Home', 'Agents', 'Tickets', 'Plugins', 'Profile', 'Settings'])
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

it('opens a full profile page without changing the sidebar and restores focus when dismissed', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Keep this draft')
  await user.click(screen.getByRole('button', { name: 'Collapse sidebar' }))
  const profile = screen.getByRole('button', { name: 'Profile' })
  await user.click(profile)

  expect(screen.queryByRole('complementary', { name: 'Main sidebar' })).not.toBeInTheDocument()
  expect(screen.getByRole('region', { name: 'User profile' })).toBeVisible()
  expect(screen.queryByRole('dialog', { name: 'User profile' })).not.toBeInTheDocument()
  expect(profile).toHaveAttribute('aria-current', 'page')
  expect(screen.queryByRole('textbox', { name: 'Message' })).not.toBeInTheDocument()
  expect(screen.getByRole('button', { name: 'Sign in with ChatGPT' })).toBeVisible()
  await user.click(profile)
  expect(screen.getByRole('region', { name: 'User profile' })).toBeVisible()
  expect(screen.getByRole('region', { name: 'User profile' })).toHaveFocus()
  await user.keyboard('{Escape}')
  expect(screen.queryByRole('region', { name: 'User profile' })).not.toBeInTheDocument()
  expect(profile).toHaveFocus()
  expect(screen.getByRole('button', { name: 'Expand sidebar' })).toBeVisible()
  expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this draft')
})

it('replaces Activity with new workspace tabs while preserving the chat draft', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.type(screen.getByRole('textbox', {name: 'Message'}), 'Keep this message')
  expect(screen.queryByRole('button', {name: /Activity/})).not.toBeInTheDocument()
  await user.click(screen.getByRole('button', {name: 'New tab'}))
  const panel = screen.getByRole('complementary', {name: 'Workspace tools'})
  expect(panel).toBeVisible()
  expect(within(panel).getByRole('button', {name: 'Browser'})).toBeVisible()
  expect(within(panel).getByRole('button', {name: 'Terminal'})).toBeDisabled()
  expect(within(panel).getByRole('button', {name: 'Git diff'})).toBeDisabled()
  expect(within(panel).getByRole('button', {name: 'Files'})).toBeDisabled()
  await user.click(within(panel).getByRole('button', {name: 'Close workspace panel'}))
  expect(screen.queryByRole('complementary', {name: 'Workspace tools'})).not.toBeInTheDocument()
  expect(screen.getByRole('textbox', {name: 'Message'})).toHaveValue('Keep this message')
})
