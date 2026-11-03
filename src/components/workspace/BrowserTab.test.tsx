import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { BrowserTab } from './BrowserTab'

const api = vi.hoisted(() => ({invoke: vi.fn()}))
vi.mock('@tauri-apps/api/core', () => api)
vi.mock('@tauri-apps/api/event', () => ({listen: vi.fn().mockResolvedValue(() => {})}))
beforeEach(() => {
  vi.stubGlobal('ResizeObserver', class {observe() {} disconnect() {}})
  api.invoke.mockReset()
})
afterEach(() => { cleanup(); vi.unstubAllGlobals() })

it('collects page notes for explicit sharing, preserves notes across hiding, and binds sharing to the original chat', async () => {
  let selection: unknown = {url: 'https://example.com', title: 'Example', selector: '#heading', quote: 'Welcome'}
  api.invoke.mockImplementation(async (_command, {method}) => {
    if (method === 'browser.create') return {id: 'browser-1'}
    if (method === 'browser.snapshot') { const next = selection; selection = null; return {url: 'https://example.com', title: 'Example', selection: next} }
    return {}
  })
  const onSendNotes = vi.fn(), onNativeId = vi.fn(), onDirtyChange = vi.fn(), onTitle = vi.fn()
  const props = {initialUrl: 'https://example.com', desktop: true, visible: true, canSendNotes: true, onSendNotes, onNativeId, onDirtyChange, onTitle}
  const view = render(<BrowserTab {...props} />)
  fireEvent.change(await screen.findByRole('textbox', {name: 'Annotation note'}), {target: {value: 'Make this heading smaller'}})
  fireEvent.click(screen.getByRole('button', {name: 'Add note'}))
  expect(onSendNotes).not.toHaveBeenCalled()
  expect(onDirtyChange).toHaveBeenLastCalledWith(true)
  view.rerender(<BrowserTab {...props} visible={false} />)
  view.rerender(<BrowserTab {...props} canSendNotes={false} />)
  expect(screen.getByRole('button', {name: 'Add notes to message'})).toBeDisabled()
  expect(screen.getByText('1. Make this heading smaller')).toBeVisible()
  view.rerender(<BrowserTab {...props} />)
  fireEvent.click(screen.getByRole('button', {name: 'Add notes to message'}))
  expect(onSendNotes).toHaveBeenCalledWith(expect.stringContaining('Note: Make this heading smaller'))
  expect(onSendNotes.mock.calls[0][0]).toContain('Page: https://example.com')
  await waitFor(() => expect(onDirtyChange).toHaveBeenLastCalledWith(false))
  view.unmount()
  await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('workspace_browser_request', {method: 'browser.close', params: {id: 'browser-1'}}))
})

it('shows the navigated page address without replacing an address being edited', async () => {
  api.invoke.mockImplementation(async (_command, {method}) => method === 'browser.create' ? {id: 'browser-2'} : method === 'browser.snapshot' ? {url: 'https://example.com/new', title: 'New page'} : {})
  render(<BrowserTab initialUrl="https://example.com" desktop visible canSendNotes onSendNotes={vi.fn()} onNativeId={vi.fn()} onTitle={vi.fn()} onDirtyChange={vi.fn()} />)
  const address = await screen.findByRole('textbox', {name: 'Website address'})
  await waitFor(() => expect(address).toHaveValue('https://example.com/new'))
  fireEvent.focus(address)
  fireEvent.change(address, {target: {value: 'https://another.test'}})
  fireEvent.submit(address.closest('form')!)
  await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('workspace_browser_request', {method: 'browser.navigate', params: {id: 'browser-2', url: 'https://another.test'}}))
  expect(address).toHaveValue('https://another.test')
})

it('leaves a blank new tab quiet until the user navigates to a website', async () => {
  const created = vi.fn()
  api.invoke.mockImplementation(async (_command, {method}) => {
    if (method === 'browser.create') return {id: 'blank-tab'}
    if (method === 'browser.snapshot') throw new Error('Browser evaluation was canceled')
    return {}
  })
  render(<BrowserTab desktop visible canSendNotes onSendNotes={vi.fn()} onNativeId={created} onTitle={vi.fn()} onDirtyChange={vi.fn()} />)
  await waitFor(() => expect(created).toHaveBeenCalledWith('blank-tab'))
  expect(api.invoke.mock.calls.some(([, args]) => args.method === 'browser.snapshot')).toBe(false)
  expect(screen.queryByRole('alert')).not.toBeInTheDocument()
})

it('recovers navigation that completed before the location listener registered', async () => {
  api.invoke.mockImplementation(async (_command, {method}) => {
    if (method === 'browser.create') return {id: 'early-navigation'}
    if (method === 'browser.location') return {id: 'early-navigation', url: 'https://example.com/agent-opened'}
    if (method === 'browser.snapshot') return {url: 'https://example.com/agent-opened', title: 'Agent page'}
    return {}
  })
  render(<BrowserTab desktop visible canSendNotes onSendNotes={vi.fn()} onNativeId={vi.fn()} onTitle={vi.fn()} onDirtyChange={vi.fn()} />)
  await waitFor(() => expect(screen.getByRole('textbox', {name: 'Website address'})).toHaveValue('https://example.com/agent-opened'))
})
