import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { IntelligentUi, IntelligentUiErrorBoundary } from './IntelligentUi'
import type { UiDocument } from '../lib/intelligent-ui'
import { clearIntelligentUiControllersForTests } from '../lib/intelligent-ui'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
const call = vi.mocked(invoke)
const document: UiDocument = {
  version: 1, kind: 'native', title: 'Budget options', text: 'Current total is {{total}}', initialState: { total: 5, tab: 'overview' },
  actions: [{ id: 'save', label: 'Save budget', prompt: 'Use the saved budget.' }],
  components: [
    { id: 'copy', type: 'text', text: 'Total {{total}}' },
    { id: 'amount', type: 'input', label: 'Total', stateKey: 'total', inputType: 'number' },
    { id: 'go', type: 'button', label: 'Save budget', actionId: 'save' },
    { id: 'chart', type: 'chart', chartType: 'bar', points: [{ label: 'Current', value: 5 }] },
  ],
}
describe('IntelligentUi', () => {
  beforeEach(() => { call.mockReset(); clearIntelligentUiControllersForTests() })
  it('keeps an app mounted when an unrelated callback changes', async () => {
    const {components: _components, ...base} = document
    const app = {...base, kind: 'app' as const, html: '<p>Live app</p>'}
    call.mockImplementation(async (_command, args: any) => {
      if (args.method === 'document.read') return {document: app, documentHash: 'h'} as never
      if (args.method === 'state.read') return {state: app.initialState, revision: 0} as never
      throw new Error(args.method)
    })
    const cleanup = vi.fn(), mount = vi.fn(() => cleanup)
    const view = render(<IntelligentUi identity={{threadId: 'stable', callId: 'app'}} onReview={() => {}} onMountApp={mount} />)
    await waitFor(() => expect(mount).toHaveBeenCalledOnce())
    view.rerender(<IntelligentUi identity={{threadId: 'stable', callId: 'app'}} onReview={() => {}} onMountApp={mount} />)
    expect(mount).toHaveBeenCalledOnce()
    expect(cleanup).not.toHaveBeenCalled()
  })

  it('contains render errors and keeps their text alternative available', () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => undefined)
    function Broken(): React.ReactNode { throw new Error('render crash') }
    render(<IntelligentUiErrorBoundary alternative="Readable fallback"><Broken /></IntelligentUiErrorBoundary>)
    expect(screen.getByRole('alert')).toHaveTextContent(/could not be displayed/)
    expect(screen.getByText('Readable fallback')).toBeInTheDocument()
    errorSpy.mockRestore()
  })
  it('mounts an app only while its card intersects the viewport and tears it down offscreen', async () => {
    const observers: { callback: IntersectionObserverCallback; disconnect: ReturnType<typeof vi.fn> }[] = []
    class Observer {
      callback: IntersectionObserverCallback; disconnect = vi.fn()
      constructor(callback: IntersectionObserverCallback) { this.callback = callback; observers.push(this) }
      observe() {}; unobserve() {}
    }
    vi.stubGlobal('IntersectionObserver', Observer)
    const {components: _components, ...base} = document
    const app = {...base, kind: 'app' as const, html: '<p>Live app</p>'}
    call.mockImplementation(async (_command, args: any) => {
      if (args.method === 'document.read') return {document: app, documentHash: 'viewport-hash'} as never
      if (args.method === 'state.read') return {state: app.initialState, revision: 0} as never
      throw new Error(args.method)
    })
    const cleanup = vi.fn(), mount = vi.fn(() => cleanup)
    const view = render(<IntelligentUi identity={{threadId: 'viewport', callId: 'app'}} onMountApp={mount} />)
    await waitFor(() => expect(observers.length).toBeGreaterThan(0))
    expect(mount).not.toHaveBeenCalled()
    act(() => observers[0].callback([{isIntersecting: true, intersectionRatio: .5} as IntersectionObserverEntry], {} as IntersectionObserver))
    await waitFor(() => expect(mount).toHaveBeenCalledOnce())
    act(() => observers[0].callback([{isIntersecting: false, intersectionRatio: 0} as IntersectionObserverEntry], {} as IntersectionObserver))
    await waitFor(() => expect(cleanup).toHaveBeenCalledOnce())
    view.unmount(); vi.unstubAllGlobals()
  })
  it('keeps rapid typing visible and editable while serialized saves are delayed', async () => {
    const textDoc: UiDocument = { version: 1, kind: 'native', title: 'Name', text: 'Name: {{name}}', initialState: { name: '' }, actions: [], components: [{ id: 'name-input', type: 'input', label: 'Name', stateKey: 'name' }] }
    const pending: { state: { name: string }; resolve(value: unknown): void }[] = []
    call.mockImplementation(async (_command, args: any) => {
      if (args.method === 'document.read') return { document: textDoc, documentHash: 'typing-hash' } as never
      if (args.method === 'state.read') return { state: textDoc.initialState, revision: 0 } as never
      if (args.method === 'state.write') return await new Promise(resolve => pending.push({ state: args.params.state, resolve })) as never
      throw new Error(args.method)
    })
    const user = userEvent.setup()
    render(<IntelligentUi identity={{ threadId: 'typing', callId: 'app' }} />)
    const input = await screen.findByRole('textbox', { name: 'Name' }) as HTMLInputElement
    await user.type(input, 'Alice')
    await waitFor(() => expect(pending).toHaveLength(1))
    expect(input).toHaveValue('Alice')
    expect(input).toBeEnabled()
    for (let index = 0; index < 5; index++) {
      const current = pending[index]
      expect(current.state.name).toBe('Alice'.slice(0, index + 1))
      await act(async () => { current.resolve({ state: current.state, revision: index + 1 }) })
      if (index < 4) await waitFor(() => expect(pending).toHaveLength(index + 2))
      expect(input).toHaveValue('Alice')
    }
    await waitFor(() => expect(screen.getByText('Name: Alice')).toBeInTheDocument())
  })
  it('renders supplied previews as accessible but fully disabled components without reading authority', () => {
    render(<IntelligentUi identity={{ threadId: 't', callId: 'c' }} document={document} preview />)
    expect(screen.getByText('Preview')).toBeInTheDocument()
    expect(screen.getByLabelText('Total')).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Save budget' })).toBeDisabled()
    expect(screen.getByRole('img', { name: /Bar chart/ })).toBeInTheDocument()
    expect(call).not.toHaveBeenCalled()
  })
  it('uses the authoritative saved state, supports text alternative and prepares actions', async () => {
    call.mockImplementation(async (_command, args: any) => {
      if (args.method === 'document.read') return { document, documentHash: 'h' } as never
      if (args.method === 'state.read') return { state: { total: 8, tab: 'overview' }, revision: 2 } as never
      if (args.method === 'action.prepare') return { reviewId: 'r', text: 'Prepared task', threadId: 't', callId: 'c', documentHash: 'h' } as never
      throw new Error(args.method)
    })
    const onReview = vi.fn()
    render(<IntelligentUi identity={{ threadId: 't', callId: 'c' }} onReview={onReview} />)
    expect(await screen.findByText('Total 8')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'View text' }))
    expect(screen.getAllByText('Current total is 8').length).toBeGreaterThan(0)
    fireEvent.click(screen.getByRole('button', { name: 'Save budget' }))
    await waitFor(() => expect(onReview).toHaveBeenCalledWith(expect.objectContaining({ reviewId: 'r' })))
  })
  it('mounts an authoritative app through the host sandbox and authenticates the narrow bridge', async () => {
    let failAction = false, failState = false, revision = 4
    const app: UiDocument = { version: 1, kind: 'app', title: 'Picker', text: 'Pick a color', initialState: { color: 'blue' }, actions: [{ id: 'choose', label: 'Choose', prompt: 'Chosen.' }], html: '<button>Choose</button>' }
    call.mockImplementation(async (_command, args: any) => {
      if (args.method === 'document.read') return { document: app, documentHash: 'app-hash' } as never
      if (args.method === 'state.read') return { state: { color: 'blue' }, revision: 4 } as never
      if (args.method === 'sandbox.create') return { url: 'http://127.0.0.1:4321/ui/capability', token: 'capability' } as never
      if (args.method === 'state.write') { if (failState) throw new Error('Could not save state'); return { state: args.params.state, revision: ++revision } as never }
      if (args.method === 'action.prepare') { if (failAction) throw new Error('Could not prepare action'); return { reviewId: 'review', text: 'Chosen.', threadId: 't', callId: 'app-call', documentHash: 'app-hash' } as never }
      if (args.method === 'sandbox.destroy') return null as never
      throw new Error(args.method)
    })
    const onReview = vi.fn()
    const { unmount } = render(<IntelligentUi identity={{ threadId: 't', callId: 'app-call' }} onReview={onReview} />)
    const frame = await screen.findByTitle('Picker interactive app') as HTMLIFrameElement
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts allow-same-origin')
    window.dispatchEvent(new MessageEvent('message', { source: frame.contentWindow, origin: 'http://127.0.0.1:4321', data: { channel: 'brr-ui', token: 'wrong', id: '1', method: 'setState', args: { color: 'red' } } }))
    await Promise.resolve()
    expect(call).not.toHaveBeenCalledWith('intelligent_ui_request', expect.objectContaining({ method: 'state.write' }))
    const appPosts = vi.spyOn(frame.contentWindow!, 'postMessage')
    window.dispatchEvent(new MessageEvent('message', { source: frame.contentWindow, origin: 'http://127.0.0.1:4321', data: { channel: 'brr-ui', token: 'capability', id: '2', method: 'setState', args: { color: 'red' } } }))
    await waitFor(() => expect(appPosts).toHaveBeenCalledWith(expect.objectContaining({ channel: 'brr-ui-state', token: 'capability', state: { color: 'red' }, revision: 5 }), 'http://127.0.0.1:4321'))
    window.dispatchEvent(new MessageEvent('message', { source: frame.contentWindow, origin: 'http://127.0.0.1:4321', data: { channel: 'brr-ui', token: 'capability', id: '3', method: 'requestAction', args: 'choose' } }))
    await waitFor(() => expect(onReview).toHaveBeenCalledWith(expect.objectContaining({ reviewId: 'review' })))
    failState = true
    window.dispatchEvent(new MessageEvent('message', { source: frame.contentWindow, origin: 'http://127.0.0.1:4321', data: { channel: 'brr-ui', token: 'capability', id: '5', method: 'setState', args: { color: 'green' } } }))
    await waitFor(() => expect(appPosts).toHaveBeenCalledWith(expect.objectContaining({ channel: 'brr-ui-result', id: '5', ok: false, error: 'Could not save state' }), 'http://127.0.0.1:4321'))
    window.dispatchEvent(new MessageEvent('message', { source: frame.contentWindow, origin: 'http://127.0.0.1:4321', data: { channel: 'brr-ui', token: 'capability', id: '6', method: 'requestAction', args: 'choose' } }))
    await waitFor(() => expect(appPosts).toHaveBeenCalledWith(expect.objectContaining({ channel: 'brr-ui-result', id: '6', ok: false, error: 'Save the UI inputs before requesting this action.' }), 'http://127.0.0.1:4321'))
    failState = false
    window.dispatchEvent(new MessageEvent('message', { source: frame.contentWindow, origin: 'http://127.0.0.1:4321', data: { channel: 'brr-ui', token: 'capability', id: '7', method: 'setState', args: { color: 'red' } } }))
    await waitFor(() => expect(appPosts).toHaveBeenCalledWith(expect.objectContaining({ channel: 'brr-ui-state', token: 'capability', state: { color: 'red' }, revision: 6 }), 'http://127.0.0.1:4321'))
    failAction = true
    window.dispatchEvent(new MessageEvent('message', { source: frame.contentWindow, origin: 'http://127.0.0.1:4321', data: { channel: 'brr-ui', token: 'capability', id: '4', method: 'requestAction', args: 'choose' } }))
    await waitFor(() => expect(appPosts).toHaveBeenCalledWith(expect.objectContaining({ channel: 'brr-ui-result', id: '4', ok: false, error: 'Could not prepare action' }), 'http://127.0.0.1:4321'))
    failAction = false
    fireEvent.click(screen.getByRole('button', { name: 'Stop app' }))
    await waitFor(() => expect(call).toHaveBeenCalledWith('intelligent_ui_request', { method: 'sandbox.destroy', params: { token: 'capability' } }))
    expect(screen.getByText(/Interactive app is stopped\./)).toBeInTheDocument()
    expect(screen.queryByText(/active in another view/)).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Run app' }))
    await waitFor(() => expect(call.mock.calls.filter(([, options]) => (options as any)?.method === 'sandbox.create').length).toBe(2))
    unmount()
    expect(call).toHaveBeenCalledWith('intelligent_ui_request', { method: 'sandbox.destroy', params: { token: 'capability' } })
  })
  it('plots negative and positive values around a zero baseline', () => {
    const mixed = { ...document, components: [{ id: 'signed', type: 'chart' as const, chartType: 'bar' as const, points: [{ label: 'Loss', value: -4 }, { label: 'Gain', value: 4 }] }] }
    render(<IntelligentUi identity={{ threadId: 't', callId: 'signed' }} document={mixed} preview />)
    const axis = screen.getByRole('img', { name: /Bar chart/ }).querySelector('.chart-axis')
    expect(axis?.getAttribute('y1')).toBe('107')
    const bars = screen.getByRole('img', { name: /Bar chart/ }).querySelectorAll('rect.chart-mark')
    expect(bars[0].getAttribute('y')).toBe('107')
    expect(bars[1].getAttribute('y')).toBe('36')
  })
  it('does not execute an app supplied in preview', () => {
    const app: UiDocument = { version: 1, kind: 'app', title: 'App', text: 'Accessible app text', initialState: {}, actions: [], html: '<script>window.ran=true</script>' }
    const onMountApp = vi.fn()
    render(<IntelligentUi identity={{ threadId: 't', callId: 'app' }} document={app} preview onMountApp={onMountApp} />)
    expect(screen.getByText('Preview')).toBeInTheDocument()
    expect(onMountApp).not.toHaveBeenCalled()
  })
})
