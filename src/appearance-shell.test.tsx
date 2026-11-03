import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import App from './App'
import { SIDEBAR_COLORS } from './lib/appearance'

// Shell behavior uses a controlled editor; Lexical input is covered by its own
// component tests and the browser suite, where native selection APIs are present.
vi.mock('./components/ChatEditor', async () => {
  const React = await import('react')
  return { ChatEditor: React.forwardRef<any, any>((props, ref) => {
    React.useImperativeHandle(ref, () => ({focus: () => {}, insertText: () => {}, insertQuote: () => {}, openMenu: () => {}}))
    return <textarea aria-label="Message" value={props.document.text} onChange={event => props.onChange({...props.document, text: event.target.value})} />
  }) }
})

vi.mock('./lib/client', () => ({ isDesktop: () => false, client: {} }))

beforeEach(() => localStorage.clear())
afterEach(() => vi.restoreAllMocks())

it('applies the selected Lime color to the permanent rail, never the main sidebar', async () => {
  const user = userEvent.setup()
  render(<App />)
  await user.click(screen.getByRole('button', { name: 'Settings' }))
  await user.click(screen.getByRole('radio', { name: 'Lime' }))

  const lime = SIDEBAR_COLORS.find(color => color.id === 'lime')!
  const rail = screen.getByRole('navigation', { name: 'Main rail' })
  const sidebar = screen.getByRole('complementary', { name: 'Main sidebar' })
  expect(rail.style.getPropertyValue('--rail-background')).toBe(lime.background)
  expect(rail.style.getPropertyValue('--rail-foreground')).toBe(lime.foreground)
  expect(sidebar.style.getPropertyValue('--sidebar-background')).toBe('')
  expect(sidebar.style.getPropertyValue('--sidebar-foreground')).toBe('')
})

it('starts with graphite and remembers the rail color without losing the conversation draft', async () => {
  const user = userEvent.setup()
  const app = render(<App />)
  await user.type(screen.getByRole('textbox', { name: 'Message' }), 'Keep this draft')
  await user.click(screen.getByRole('button', { name: 'Settings' }))
  expect(screen.getByRole('region', { name: 'Application settings' })).toBeInTheDocument()
  expect(screen.getByRole('radio', { name: 'Graphite' })).toBeChecked()
  await user.click(screen.getByRole('radio', { name: 'Violet' }))
  await user.click(screen.getByRole('button', { name: 'Close settings' }))
  expect(screen.getByRole('button', { name: 'Settings' })).toHaveFocus()
  expect(screen.getByRole('textbox', { name: 'Message' })).toHaveValue('Keep this draft')
  app.unmount()

  render(<App />)
  await user.click(screen.getByRole('button', { name: 'Settings' }))
  expect(screen.getByRole('radio', { name: 'Violet' })).toBeChecked()
  const violet = SIDEBAR_COLORS.find(color => color.id === 'violet')!
  const rail = screen.getByRole('navigation', { name: 'Main rail' })
  expect(rail.style.getPropertyValue('--rail-background')).toBe(violet.background)
  expect(rail.style.getPropertyValue('--rail-foreground')).toBe(violet.foreground)
})

it('keeps the chosen color available for the current session if saving is unavailable', async () => {
  vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('Storage unavailable') })
  const user = userEvent.setup()
  render(<App />)
  await user.click(screen.getByRole('button', { name: 'Settings' }))
  await user.click(screen.getByRole('radio', { name: 'Pink' }))
  expect(screen.getByRole('radio', { name: 'Pink' })).toBeChecked()
  expect(screen.getByRole('status')).toHaveTextContent('Color changed for this session. Could not save your preference.')
})
