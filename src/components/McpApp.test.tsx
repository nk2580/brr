import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const mocks = vi.hoisted(() => ({
  status: vi.fn(), readApp: vi.fn(), createSandbox: vi.fn(), destroySandbox: vi.fn(), callTool: vi.fn(), readResource: vi.fn(), openUrl: vi.fn(),
  bridge: null as any,
}))
vi.mock('../lib/extensions', () => ({
  listMcpServerStatus: mocks.status, callMcpTool: mocks.callTool, readMcpResource: mocks.readResource,
}))
vi.mock('../lib/mcp-app', () => ({
  readMcpAppResource: mocks.readApp, createMcpSandbox: mocks.createSandbox, destroyMcpSandbox: mocks.destroySandbox,
  createExactOriginTransport: vi.fn(() => ({ start: vi.fn(), close: vi.fn(), send: vi.fn() })),
}))
vi.mock('../lib/external-url', () => ({
  openExternalUrl: mocks.openUrl,
  isExternalUrl: (value: unknown) => typeof value === 'string' && (value.startsWith('https://') || value.startsWith('http://localhost')),
}))
vi.mock('@modelcontextprotocol/ext-apps/app-bridge', () => ({
  AppBridge: class {
    oncalltool: any; onlistresources: any; onreadresource: any; onopenlink: any; onmessage: any; onsandboxready: any; oninitialized: any
    connect = vi.fn(async () => undefined)
    sendSandboxResourceReady = vi.fn(async () => undefined)
    sendToolInput = vi.fn(async () => undefined)
    sendToolResult = vi.fn(async () => undefined)
    teardownResource = vi.fn(async () => undefined)
    close = vi.fn(async () => undefined)
    constructor() { mocks.bridge = this }
  },
}))

import { McpApp } from './McpApp'

const invocation = {
  itemId: 'item-1', server: 'calendar', tool: 'today', threadId: 'thread-1', resourceUri: 'ui://calendar/today',
  arguments: { date: 'today' }, result: { content: [{ type: 'text', text: 'Initial result' }] },
}

describe('McpApp host bridge', () => {
  beforeEach(() => {
    mocks.bridge = null
    mocks.status.mockReset().mockResolvedValue([{ name: 'calendar', tools: { lookup: { inputSchema: { type: 'object' } }, hidden: { _meta: { ui: { visibility: ['model'] } } } }, resources: [{ uri: 'calendar://current' }], resourceTemplates: [{ uriTemplate: 'calendar://events/{eventId}', name: 'Event' }] }])
    mocks.readApp.mockReset().mockResolvedValue({ html: '<html>app</html>', csp: { connectDomains: [], resourceDomains: [], frameDomains: [], baseUriDomains: [] }, requestedPermissions: { camera: {} } })
    mocks.createSandbox.mockReset().mockResolvedValue({ url: 'http://127.0.0.1:49888/widget/' + 'a'.repeat(64), token: 'a'.repeat(64) })
    mocks.destroySandbox.mockReset().mockResolvedValue(undefined)
    mocks.callTool.mockReset().mockResolvedValue({ content: [{ type: 'text', text: 'approved result' }] })
    mocks.readResource.mockReset().mockResolvedValue({ contents: [{ uri: 'calendar://current', text: 'details' }] })
    mocks.openUrl.mockReset().mockResolvedValue(undefined)
  })

  it('loads a distinct sandbox, sends tool input and result after initialization, then requires approval for same-server tool calls', async () => {
    const user = userEvent.setup()
    const rendered = render(<McpApp invocation={invocation} />)
    await waitFor(() => expect(mocks.bridge).not.toBeNull())
    expect(mocks.createSandbox).toHaveBeenCalledWith(expect.objectContaining({ html: '<html>app</html>', hostOrigin: window.location.origin }))
    expect(await screen.findByRole('note')).toHaveTextContent(/requested camera.*does not grant device permissions/i)
    expect(mocks.bridge.connect).toHaveBeenCalled()
    mocks.bridge.onsandboxready({})
    await waitFor(() => expect(mocks.bridge.sendSandboxResourceReady).toHaveBeenCalledWith(expect.objectContaining({ html: '<html>app</html>', permissions: {} })))
    mocks.bridge.oninitialized()
    expect(mocks.bridge.sendToolInput).toHaveBeenCalledWith({ arguments: { date: 'today' } })
    expect(mocks.bridge.sendToolResult).toHaveBeenCalledWith(invocation.result)

    const call = mocks.bridge.oncalltool({ name: 'lookup', arguments: { id: 4 } })
    expect(await mocks.bridge.oncalltool({ name: 'hidden', arguments: {} })).toMatchObject({ isError: true })
    expect(mocks.callTool).not.toHaveBeenCalled()
    expect(await screen.findByRole('dialog', { name: 'Approve MCP App tool call' })).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Allow once' }))
    await expect(call).resolves.toEqual({ content: [{ type: 'text', text: 'approved result' }] })
    expect(mocks.callTool).toHaveBeenCalledWith('calendar', 'lookup', { id: 4 }, 'thread-1')
    rendered.unmount()
    await waitFor(() => expect(mocks.destroySandbox).toHaveBeenCalledWith('a'.repeat(64)))
    await waitFor(() => expect(mocks.bridge.close).toHaveBeenCalled())
  })

  it('rejects unlisted resource reads and prompts before external links and message insertion', async () => {
    const user = userEvent.setup()
    const onUsePrompt = vi.fn()
    render(<McpApp invocation={invocation} onUsePrompt={onUsePrompt} />)
    await waitFor(() => expect(mocks.bridge).not.toBeNull())
    await expect(mocks.bridge.onreadresource({ uri: 'file:///etc/passwd' })).rejects.toThrow(/not available/i)
    await expect(mocks.bridge.onreadresource({ uri: 'calendar://current' })).resolves.toMatchObject({ contents: [{ uri: 'calendar://current' }] })
    await expect(mocks.bridge.onreadresource({ uri: 'calendar://events/event-42' })).resolves.toMatchObject({ contents: [{ uri: 'calendar://current' }] })

    const link = mocks.bridge.onopenlink({ url: 'https://example.test/path' })
    expect(await screen.findByRole('dialog', { name: 'Approve external link' })).toBeInTheDocument()
    expect(mocks.openUrl).not.toHaveBeenCalled()
    await user.click(screen.getByRole('button', { name: 'Open link' }))
    await expect(link).resolves.toEqual({})
    expect(mocks.openUrl).toHaveBeenCalledTimes(1)

    await expect(mocks.bridge.onopenlink({ url: 'javascript:alert(1)' })).resolves.toEqual({ isError: true })
    mocks.bridge.onmessage({ content: [{ type: 'text', text: 'Check this response' }, { type: 'image', data: 'opaque' }] })
    expect(await screen.findByText('App requested to send a message')).toBeInTheDocument()
    expect(screen.getByText(/opaque/)).toBeInTheDocument()
    expect(onUsePrompt).not.toHaveBeenCalled()
    await user.click(screen.getByRole('button', { name: 'Review and insert text' }))
    expect(onUsePrompt).toHaveBeenCalledWith('Check this response')
    expect(mocks.readResource).toHaveBeenCalledWith('calendar', 'calendar://current', 'thread-1')
    expect(mocks.readResource).toHaveBeenCalledWith('calendar', 'calendar://events/event-42', 'thread-1')
  })

  it('forwards a later tool result without recreating the sandbox or restarting the app', async () => {
    const { rerender } = render(<McpApp invocation={{ ...invocation, result: undefined }} />)
    await waitFor(() => expect(mocks.bridge).not.toBeNull())
    mocks.bridge.oninitialized()
    const bridge = mocks.bridge
    const updated = { content: [{ type: 'text', text: 'completed later' }] }
    rerender(<McpApp invocation={{ ...invocation, result: updated }} />)
    await waitFor(() => expect(bridge.sendToolResult).toHaveBeenCalledWith(updated))
    expect(mocks.createSandbox).toHaveBeenCalledTimes(1)
    expect(bridge.connect).toHaveBeenCalledTimes(1)
  })

  it('shows resource or status errors instead of rendering an unverified app', async () => {
    mocks.status.mockResolvedValueOnce([])
    render(<McpApp invocation={invocation} />)
    expect(await screen.findByRole('alert')).toHaveTextContent(/not available in this conversation/i)
    expect(screen.queryByTitle('MCP App: today')).not.toBeInTheDocument()
    expect(mocks.createSandbox).not.toHaveBeenCalled()
  })
})
