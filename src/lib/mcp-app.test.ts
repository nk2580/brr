import { beforeEach, describe, expect, it, vi } from 'vitest'

const invokeMock = vi.hoisted(() => vi.fn())
const readResourceMock = vi.hoisted(() => vi.fn())
vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }))
vi.mock('./extensions', () => ({ readMcpResource: readResourceMock }))

import {
  createExactOriginTransport, createMcpSandbox, destroyMcpSandbox,
  extractMcpAppInvocation, normalizeMcpAppCsp, readMcpAppResource,
} from './mcp-app'

describe('MCP Apps host boundaries', () => {
  beforeEach(() => {
    invokeMock.mockReset()
    readResourceMock.mockReset()
  })

  it('extracts nested and legacy MCP App UI resource metadata only from tool items', () => {
    const nested = extractMcpAppInvocation({
      id: 'tool-1', type: 'mcpToolCall', server: 'calendar', tool: 'today',
      arguments: { timezone: 'UTC' }, result: { content: [{ type: 'text', text: 'raw result' }] },
      mcpAppUi: { resourceUri: 'ui://calendar/today' },
    }, 'thread-1')
    expect(nested).toEqual({
      itemId: 'tool-1', server: 'calendar', tool: 'today', threadId: 'thread-1',
      resourceUri: 'ui://calendar/today', arguments: { timezone: 'UTC' },
      result: { content: [{ type: 'text', text: 'raw result' }] },
    })
    expect(extractMcpAppInvocation({ id: 'tool-2', type: 'mcpToolCall', server: 's', tool: 't', mcpAppResourceUri: 'ui://s/legacy' }, 'thread-1')?.resourceUri).toBe('ui://s/legacy')
    expect(extractMcpAppInvocation({ id: 'message', type: 'agentMessage', mcpAppResourceUri: 'ui://s/legacy' }, 'thread-1')).toBeNull()
    expect(extractMcpAppInvocation({ id: 'tool-3', type: 'mcpToolCall', server: 's', tool: 't', mcpAppResourceUri: 'https://example.test/app' }, 'thread-1')).toBeNull()
    expect(extractMcpAppInvocation({ id: 'tool-4', type: 'mcpToolCall', server: 's', tool: 't', mcpAppResourceUri: 'ui://s/app' }, undefined)).toBeNull()
  })

  it('loads the matching HTML resource and normalizes safe exact and wildcard CSP origins', async () => {
    readResourceMock.mockResolvedValue({ contents: [{
      uri: 'ui://calendar/today', mimeType: 'text/html;profile=mcp-app',
      text: '<!doctype html><script>window.appReady=true</script>',
      _meta: { ui: { csp: { connectDomains: ['https://api.example.test'], resourceDomains: ['https://cdn.example.test'] }, permissions: { camera: {} } } },
    }] })
    await expect(readMcpAppResource('calendar', 'ui://calendar/today', 'thread-1')).resolves.toEqual({
      html: '<!doctype html><script>window.appReady=true</script>',
      csp: { connectDomains: ['https://api.example.test'], resourceDomains: ['https://cdn.example.test'], frameDomains: [], baseUriDomains: [] },
      requestedPermissions: { camera: {} },
    })
    expect(readResourceMock).toHaveBeenCalledWith('calendar', 'ui://calendar/today', 'thread-1')

    readResourceMock.mockResolvedValue({ contents: [{ uri: 'ui://calendar/today', mimeType: 'text/html;profile=mcp-app', text: '<html></html>', _meta: { ui: { csp: { connectDomains: ['wss://*.example.test'], resourceDomains: ['https://*.cdn.example.test'] } } } }] })
    await expect(readMcpAppResource('calendar', 'ui://calendar/today', 'thread-1')).resolves.toMatchObject({
      csp: { connectDomains: ['wss://*.example.test'], resourceDomains: ['https://*.cdn.example.test'] },
    })
    expect(normalizeMcpAppCsp({ connectDomains: ['wss://[::1]:8080'], resourceDomains: ['http://[::1]:8080'] })).toMatchObject({
      connectDomains: ['wss://[::1]:8080'], resourceDomains: ['http://[::1]:8080'],
    })
    for (const unsafe of ['https://*', 'https://*.test', 'https://*.example.test/path', 'https://name:password@example.test', 'javascript:alert(1)']) {
      readResourceMock.mockResolvedValueOnce({ contents: [{ uri: 'ui://calendar/today', mimeType: 'text/html;profile=mcp-app', text: '<html></html>', _meta: { ui: { csp: { resourceDomains: [unsafe] } } } }] })
      await expect(readMcpAppResource('calendar', 'ui://calendar/today', 'thread-1')).rejects.toThrow(/invalid|origin/i)
    }
  })

  it('decodes valid base64 HTML and rejects wrong-source or non-HTML responses', async () => {
    const html = '<html>✓</html>'
    const blob = btoa(String.fromCharCode(...new TextEncoder().encode(html)))
    readResourceMock.mockResolvedValueOnce({ contents: [{ uri: 'ui://server/app', mimeType: 'text/html;profile=mcp-app', blob }] })
    await expect(readMcpAppResource('server', 'ui://server/app', 'thread-1')).resolves.toMatchObject({ html: '<html>✓</html>' })

    readResourceMock.mockResolvedValueOnce({ contents: [{ uri: 'ui://other/app', mimeType: 'text/html;profile=mcp-app', text: '<html></html>' }] })
    await expect(readMcpAppResource('server', 'ui://server/app', 'thread-1')).rejects.toThrow(/matching MCP Apps HTML resource/i)
    readResourceMock.mockResolvedValueOnce({ contents: [{ uri: 'ui://server/app', mimeType: 'text/plain', text: 'not an app' }] })
    await expect(readMcpAppResource('server', 'ui://server/app', 'thread-1')).rejects.toThrow(/MCP Apps HTML/i)
  })

  it('creates and destroys one native sandbox with the host origin and normalized CSP', async () => {
    const token = 'a'.repeat(64)
    invokeMock.mockResolvedValueOnce({ url: `http://127.0.0.1:49152/widget/${token}`, token }).mockResolvedValueOnce(undefined)
    const sandbox = await createMcpSandbox({
      html: '<!doctype html><p>App</p>',
      csp: { connectDomains: [], resourceDomains: [], frameDomains: [], baseUriDomains: [] },
      hostOrigin: 'http://127.0.0.1:1420',
    })
    expect(invokeMock).toHaveBeenNthCalledWith(1, 'create_mcp_sandbox', {
      html: '<!doctype html><p>App</p>',
      csp: { connectDomains: [], resourceDomains: [], frameDomains: [], baseUriDomains: [] },
      hostOrigin: 'http://127.0.0.1:1420',
    })
    await destroyMcpSandbox(sandbox.token)
    expect(invokeMock).toHaveBeenNthCalledWith(2, 'destroy_mcp_sandbox', { token })

    invokeMock.mockResolvedValueOnce({ url: `https://evil.example/widget/${token}`, token })
    await expect(createMcpSandbox({ html: '<p>app</p>', csp: normalizeMcpAppCsp({}), hostOrigin: 'http://127.0.0.1:1420' })).rejects.toThrow(/loopback sandbox/i)
  })

  it('allows only exact app origins and bounds HTML before native sandbox creation', async () => {
    await expect(createMcpSandbox({ html: '<p>app</p>', csp: normalizeMcpAppCsp({}), hostOrigin: 'https://evil.example' })).rejects.toThrow(/host origin/i)
    await expect(createMcpSandbox({ html: 'x'.repeat(5_000_001), csp: normalizeMcpAppCsp({}), hostOrigin: 'http://localhost:1420' })).rejects.toThrow(/5 MB/i)
    expect(invokeMock).not.toHaveBeenCalled()
  })

  it('sends to one exact sandbox origin and ignores messages from wrong origins or windows', async () => {
    const sandboxWindow = { postMessage: vi.fn() } as unknown as Window
    const transport = createExactOriginTransport(sandboxWindow, 'http://127.0.0.1:49152')
    const onMessage = vi.fn()
    transport.onmessage = onMessage
    await transport.start()
    const trusted = new MessageEvent('message', { data: { jsonrpc: '2.0', method: 'ui/notifications/sandbox-proxy-ready' }, origin: 'http://127.0.0.1:49152' })
    Object.defineProperty(trusted, 'source', { value: sandboxWindow })
    window.dispatchEvent(trusted)
    const wrongOrigin = new MessageEvent('message', { data: { jsonrpc: '2.0', method: 'spoof' }, origin: 'https://evil.example' })
    Object.defineProperty(wrongOrigin, 'source', { value: sandboxWindow })
    window.dispatchEvent(wrongOrigin)
    const wrongWindow = new MessageEvent('message', { data: { jsonrpc: '2.0', method: 'spoof' }, origin: 'http://127.0.0.1:49152' })
    Object.defineProperty(wrongWindow, 'source', { value: window })
    window.dispatchEvent(wrongWindow)
    expect(onMessage).toHaveBeenCalledTimes(1)
    await transport.send({ jsonrpc: '2.0', method: 'ui/notifications/sandbox-resource-ready', params: { html: 'untrusted' } } as any)
    expect(sandboxWindow.postMessage).toHaveBeenCalledWith(expect.objectContaining({ method: 'ui/notifications/sandbox-resource-ready' }), 'http://127.0.0.1:49152')
    await transport.close()
  })
})
