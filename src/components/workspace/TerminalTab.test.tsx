import { act, fireEvent, render, screen } from '@testing-library/react'
import { StrictMode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { TerminalTab } from './TerminalTab'

const terminalMock = vi.hoisted(() => ({input: undefined as undefined | ((data: string) => void), writes: [] as Uint8Array[], writeCallbacks: [] as Array<() => void>, autoCompleteWrites: true, opens: 0, options: undefined as {fontFamily?: string; theme?: Record<string, string>} | undefined}))
const api = vi.hoisted(() => ({workspaceRequest: vi.fn()}))
vi.mock('@/lib/workspace-client', () => api)
vi.mock('@xterm/xterm', () => ({Terminal: class {
  cols = 80; rows = 24; element: HTMLElement | undefined
  options: typeof terminalMock.options
  constructor(options: typeof terminalMock.options) { terminalMock.opens++; terminalMock.options = this.options = options }
  loadAddon() {}
  open(element: HTMLElement) { this.element = element }
  onData(handler: (data: string) => void) { terminalMock.input = handler; return {dispose: vi.fn()} }
  write(data: Uint8Array, callback?: () => void) { terminalMock.writes.push(data); if (callback) { terminalMock.writeCallbacks.push(callback); if (terminalMock.autoCompleteWrites) callback() } }
  dispose() {}
}}))
vi.mock('@xterm/addon-fit', () => ({FitAddon: class {fit() {}}}))

describe('TerminalTab', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    terminalMock.input = undefined; terminalMock.writes = []; terminalMock.writeCallbacks = []; terminalMock.autoCompleteWrites = true; terminalMock.opens = 0; terminalMock.options = undefined
    document.documentElement.style.setProperty('--font-mono', 'ui-monospace, SFMono-Regular, Menlo, monospace')
    document.documentElement.style.setProperty('--background', 'oklch(0% 0 0)')
    document.documentElement.style.setProperty('--foreground', 'oklch(100% 0 0)')
    document.documentElement.style.setProperty('--accent', 'var(--foreground)')
    api.workspaceRequest.mockReset()
    api.workspaceRequest.mockImplementation(async (method: string) => {
      if (method === 'terminal.create') return {id: 'pty-1'}
      if (method === 'terminal.read') return {chunks: [{seq: 1, data: [72, 105]}], cursor: 1, exited: false}
      return undefined
    })
    vi.stubGlobal('ResizeObserver', class {observe() {} disconnect() {}})
  })
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals() })

  async function startEchoRace() {
    let echoAvailable = false
    api.workspaceRequest.mockImplementation(async (method: string) => {
      if (method === 'terminal.create') return {id: 'pty-1'}
      if (method === 'terminal.read') return echoAvailable
        ? {chunks: [{seq: 1, data: [120]}], cursor: 1, exited: false}
        : {chunks: [], cursor: 0, exited: false}
      if (method === 'terminal.write') echoAvailable = true
      return undefined
    })
    render(<TerminalTab root="/project" visible />)
    await act(async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.read', {id: 'pty-1', after: 0, waitMs: 1000})
    expect(terminalMock.writes).toHaveLength(0)
    act(() => terminalMock.input?.('x'))
  }

  it('presents an available command echo within 50ms of terminal input', async () => {
    await startEchoRace()
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.read', {id: 'pty-1', after: 0, waitMs: 1000})
    await act(async () => { await vi.advanceTimersByTimeAsync(50) })
    expect(terminalMock.writes).toEqual([new Uint8Array([120])])
  })

  it('waits for xterm to finish a reply before requesting more output', async () => {
    terminalMock.autoCompleteWrites = false
    let reads = 0
    api.workspaceRequest.mockImplementation(async (method: string) => {
      if (method === 'terminal.create') return {id: 'pty-1'}
      if (method === 'terminal.read') {
        reads++
        return {chunks: [{seq: reads, data: [64 + reads]}], cursor: reads, exited: false}
      }
      return undefined
    })
    const view = render(<TerminalTab root="/project" visible />)
    await act(async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
    expect(reads).toBe(1)
    expect(terminalMock.writes).toEqual([new Uint8Array([65])])
    await act(async () => { await vi.advanceTimersByTimeAsync(100) })
    expect(reads).toBe(1)
    act(() => terminalMock.writeCallbacks[0]())
    await act(async () => { await Promise.resolve(); await vi.advanceTimersByTimeAsync(0); await Promise.resolve() })
    expect(reads).toBe(2)
    expect(terminalMock.writes).toEqual([new Uint8Array([65]), new Uint8Array([66])])
    view.unmount()
  })

  it('retries after visibility returns without overlapping or dropping output from a hidden read', async () => {
    let resolveFirstRead!: (reply: {chunks: {seq: number; data: number[]}[]; cursor: number; exited: boolean}) => void
    let reads = 0
    api.workspaceRequest.mockImplementation((method: string) => {
      if (method === 'terminal.create') return Promise.resolve({id: 'pty-1'})
      if (method === 'terminal.read') {
        reads++
        if (reads === 1) return new Promise(resolve => { resolveFirstRead = resolve })
        return Promise.resolve({chunks: [{seq: 1, data: [79]}], cursor: 1, exited: true})
      }
      return Promise.resolve(undefined)
    })
    const view = render(<TerminalTab root="/project" visible />)
    await act(async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
    expect(reads).toBe(1)
    view.rerender(<TerminalTab root="/project" visible={false} />)
    view.rerender(<TerminalTab root="/project" visible />)
    await act(async () => { await vi.advanceTimersByTimeAsync(32) })
    expect(reads).toBe(1)
    await act(async () => {
      resolveFirstRead({chunks: [{seq: 1, data: [79]}], cursor: 1, exited: false})
      await Promise.resolve()
      await vi.advanceTimersByTimeAsync(8)
    })
    expect(reads).toBe(2)
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.read')[1][1]).toEqual({id: 'pty-1', after: 0, waitMs: 1000})
    expect(terminalMock.writes).toEqual([new Uint8Array([79])])
    view.unmount()
  })

  it('ignores a pending visible read from the old root after the project changes', async () => {
    let resolveOldRead!: (reply: {chunks: {seq: number; data: number[]}[]; cursor: number; exited: boolean}) => void
    api.workspaceRequest.mockImplementation((method: string, params: {id?: string; root?: string} = {}) => {
      if (method === 'terminal.create') return Promise.resolve({id: params.root === '/first' ? 'pty-first' : 'pty-second'})
      if (method === 'terminal.read' && params.id === 'pty-first') return new Promise(resolve => { resolveOldRead = resolve })
      if (method === 'terminal.read') return Promise.resolve({chunks: [{seq: 1, data: [78]}], cursor: 1, exited: true})
      return Promise.resolve(undefined)
    })
    const view = render(<TerminalTab root="/first" visible />)
    await act(async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.read', {id: 'pty-first', after: 0, waitMs: 1000})
    view.rerender(<TerminalTab root="/second" visible />)
    await act(async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
    expect(terminalMock.opens).toBe(2)
    expect(terminalMock.writes).toEqual([new Uint8Array([78])])
    await act(async () => { resolveOldRead({chunks: [{seq: 1, data: [88]}], cursor: 1, exited: false}); await Promise.resolve() })
    expect(terminalMock.writes).toEqual([new Uint8Array([78])])
    view.unmount()
  })

  it('serializes ordered input batches while coalescing keystrokes from one microtask', async () => {
    let releaseFirstWrite!: () => void
    api.workspaceRequest.mockImplementation((method: string, params: {data?: string} = {}) => {
      if (method === 'terminal.create') return Promise.resolve({id: 'pty-1'})
      if (method === 'terminal.write' && params.data === 'a') return new Promise(resolve => { releaseFirstWrite = () => resolve(undefined) })
      return Promise.resolve(undefined)
    })
    const view = render(<TerminalTab root="/project" visible={false} />)
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    await act(async () => { terminalMock.input?.('a'); await Promise.resolve() })
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.write').map(([, params]) => params)).toEqual([{id: 'pty-1', data: 'a'}])
    await act(async () => { terminalMock.input?.('b'); terminalMock.input?.('c'); await Promise.resolve() })
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.write').map(([, params]) => params)).toEqual([{id: 'pty-1', data: 'a'}])
    await act(async () => { releaseFirstWrite(); await Promise.resolve(); await Promise.resolve() })
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.write').map(([, params]) => params)).toEqual([{id: 'pty-1', data: 'a'}, {id: 'pty-1', data: 'bc'}])
    view.unmount()
  })

  it('closes immediately on unmount and drops queued input behind an active write', async () => {
    let releaseWrite!: () => void
    api.workspaceRequest.mockImplementation((method: string, params: {data?: string} = {}) => {
      if (method === 'terminal.create') return Promise.resolve({id: 'pty-1'})
      if (method === 'terminal.write' && params.data === 'a') return new Promise(resolve => { releaseWrite = () => resolve(undefined) })
      return Promise.resolve(undefined)
    })
    const view = render(<TerminalTab root="/project" visible={false} />)
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    await act(async () => { terminalMock.input?.('a'); await Promise.resolve() })
    await act(async () => { terminalMock.input?.('b'); await Promise.resolve() })
    view.unmount()
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.close', {id: 'pty-1'})
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.write').map(([, params]) => params.data)).toEqual(['a'])
    await act(async () => { releaseWrite(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.write').map(([, params]) => params.data)).toEqual(['a'])
  })

  it('splits pasted input on UTF-8 boundaries at the 16 KiB write limit', async () => {
    const view = render(<TerminalTab root="/project" visible={false} />)
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    const paste = `${'a'.repeat(16 * 1024 - 1)}😀z`
    await act(async () => { terminalMock.input?.(paste); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
    const writes = api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.write').map(([, params]) => params.data as string)
    expect(writes).toEqual(['a'.repeat(16 * 1024 - 1), '😀z'])
    expect(writes.map(value => new TextEncoder().encode(value).length)).toEqual([16 * 1024 - 1, 5])
    view.unmount()
  })

  it('creates one PTY, polls only while visible, and forwards terminal input', async () => {
    const view = render(<TerminalTab root="/project" visible={false} />)
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.create', expect.objectContaining({root: '/project'}))
    await act(async () => { vi.advanceTimersByTime(300) })
    expect(api.workspaceRequest).not.toHaveBeenCalledWith('terminal.read', expect.anything())

    view.rerender(<TerminalTab root="/project" visible />)
    await act(async () => { vi.advanceTimersByTime(151) })
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.read', {id: 'pty-1', after: 0, waitMs: 1000})
    expect(terminalMock.writes[0]).toEqual(new Uint8Array([72, 105]))
    await act(async () => { terminalMock.input?.('echo hi\r'); await Promise.resolve() })
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.write', {id: 'pty-1', data: 'echo hi\r'})
    expect(screen.getByLabelText('Terminal')).toBeInTheDocument()
  })

  it('passes resolved font and canvas-safe theme colors to xterm', async () => {
    render(<TerminalTab root="/project" visible={false} />)
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(terminalMock.options?.fontFamily).toBe('ui-monospace, SFMono-Regular, Menlo, monospace')
    expect(terminalMock.options?.theme?.background).toBe('#000000')
    expect(terminalMock.options?.theme?.foreground).toBe('#ffffff')
    expect(terminalMock.options?.theme?.cursor).toBe('#ffffff')
  })

  it('updates xterm colors when the application theme changes', async () => {
    render(<TerminalTab root="/project" visible={false} />)
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(terminalMock.options?.theme?.cursor).toBe('#ffffff')
    await act(async () => {
      document.documentElement.style.setProperty('--accent', 'oklch(0% 0 0)')
      await Promise.resolve()
    })
    expect(terminalMock.options?.theme?.cursor).toBe('#000000')
  })

  it('does not overlap PTY reads and ignores pending output after unmount', async () => {
    let resolveRead!: (value: {chunks: {seq: number; data: number[]}[]; cursor: number; exited: boolean}) => void
    api.workspaceRequest.mockImplementation((method: string) => {
      if (method === 'terminal.create') return Promise.resolve({id: 'pty-1'})
      if (method === 'terminal.read') return new Promise(resolve => { resolveRead = resolve })
      return Promise.resolve(undefined)
    })
    const view = render(<TerminalTab root="/project" visible />)
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    await act(async () => { vi.advanceTimersByTime(151) })
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.read')).toHaveLength(1)
    await act(async () => { vi.advanceTimersByTime(600) })
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.read')).toHaveLength(1)
    view.unmount()
    await act(async () => { resolveRead({chunks: [{seq: 1, data: [88]}], cursor: 1, exited: false}); await Promise.resolve() })
    expect(terminalMock.writes).toHaveLength(0)
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.close')).toHaveLength(1)
  })

  it('survives StrictMode effect replay without duplicate PTYs or premature close', async () => {
    const view = render(<StrictMode><TerminalTab root="/project" visible={false} /></StrictMode>)
    await act(async () => { await Promise.resolve(); await Promise.resolve(); vi.runAllTicks() })
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.create')).toHaveLength(1)
    expect(api.workspaceRequest).not.toHaveBeenCalledWith('terminal.close', expect.anything())
    view.unmount()
    await act(async () => { vi.runAllTicks(); await Promise.resolve() })
    expect(api.workspaceRequest.mock.calls.filter(([method]) => method === 'terminal.close')).toHaveLength(1)
  })

  it('closes a PTY created after its component has unmounted', async () => {
    let resolveCreate!: (result: {id: string}) => void
    api.workspaceRequest.mockImplementation((method: string) => method === 'terminal.create'
      ? new Promise(resolve => { resolveCreate = resolve })
      : Promise.resolve(undefined))
    const view = render(<TerminalTab root="/project" visible={false} />)
    view.unmount()
    await act(async () => { resolveCreate({id: 'late-pty'}); await Promise.resolve(); await Promise.resolve() })
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.close', {id: 'late-pty'})
  })

  it('closes the old PTY and starts a fresh terminal when the project root changes', async () => {
    let created = 0
    api.workspaceRequest.mockImplementation(async (method: string, params: {root?: string} = {}) => {
      if (method === 'terminal.create') return {id: `pty-${++created}`, root: params.root}
      return undefined
    })
    const view = render(<TerminalTab root="/first" visible={false} />)
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    view.rerender(<TerminalTab root="/second" visible={false} />)
    await act(async () => { await Promise.resolve(); await Promise.resolve(); vi.runAllTicks() })
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.close', {id: 'pty-1'})
    expect(api.workspaceRequest).toHaveBeenCalledWith('terminal.create', expect.objectContaining({root: '/second'}))
    expect(terminalMock.opens).toBe(2)
  })
})
