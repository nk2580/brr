import { beforeEach, describe, expect, it, vi } from 'vitest'

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }))

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }))

import { callMcpTool, completeMcpArgument, getMcpPrompt, installPlugin, listHooks, listMcpPrompts, listMcpServerStatus, listPlugins, readMcpResource, refreshAppAuthStatus, saveMcpServer, searchPlugins, startMcpOAuth, trustHook, writePluginMcpCredential } from './extensions'

describe('extensions client', () => {
  beforeEach(() => invokeMock.mockReset())

  it('requests all plugin marketplace kinds for the selected workspace', async () => {
    invokeMock.mockResolvedValue({ marketplaces: [], marketplaceLoadErrors: [], featuredPluginIds: [] })

    await listPlugins('/workspace/project', true)

    expect(invokeMock).toHaveBeenCalledWith('engine_request', {
      method: 'plugin/list',
      params: {
        cwds: ['/workspace/project'],
        marketplaceKinds: ['local', 'vertical', 'workspace-directory', 'shared-with-me', 'created-by-me-remote'],
        forceRefetch: true,
      },
    })
  })

  it('loads MCP server status through every cursor page', async () => {
    invokeMock
      .mockResolvedValueOnce({ data: [{ name: 'first', tools: {}, resources: [] }], nextCursor: 'next' })
      .mockResolvedValueOnce({ data: [{ name: 'second', tools: {}, resources: [] }], nextCursor: null })

    const result = await listMcpServerStatus()

    expect(result.map(server => server.name)).toEqual(['first', 'second'])
    expect(invokeMock.mock.calls.map(([, args]) => args.params)).toEqual([
      { limit: 100, detail: 'full' },
      { limit: 100, detail: 'full', cursor: 'next' },
    ])
  })

  it('installs a marketplace plugin using the source selector and reports auth requirements', async () => {
    const response = { authPolicy: 'ON_INSTALL', appsNeedingAuth: [{ id: 'app-1', name: 'Calendar' }] }
    invokeMock.mockResolvedValue(response)

    await expect(installPlugin({ marketplacePath: '/marketplace.json', pluginName: 'calendar' })).resolves.toEqual(response)

    expect(invokeMock).toHaveBeenCalledWith('engine_request', {
      method: 'plugin/install',
      params: { marketplacePath: '/marketplace.json', pluginName: 'calendar' },
    })
  })

  it('saves an MCP server and reloads runtime configuration', async () => {
    invokeMock.mockResolvedValue({ status: 'ok' })

    await saveMcpServer('local-api', { command: 'node', args: ['server.js'], env: { API_TOKEN: 'secret' } })

    expect(invokeMock.mock.calls.map(([, args]) => args)).toEqual([
      {
        method: 'config/value/write',
        params: {
          keyPath: 'mcp_servers."local-api"',
          value: { command: 'node', args: ['server.js'], env: { API_TOKEN: 'secret' } },
          mergeStrategy: 'replace',
        },
      },
      { method: 'config/mcpServer/reload', params: {} },
    ])
  })

  it('writes private_key_jwt credentials through the scoped plugin credential RPC and reloads MCP', async () => {
    invokeMock.mockResolvedValue({ status: 'ok' })
    const credential = {
      kind: 'oauth_private_key_jwt' as const,
      privateKey: '-----BEGIN PRIVATE KEY-----\nopaque-pem\n-----END PRIVATE KEY-----',
      algorithm: 'ES384',
      keyId: 'provider-key-2',
    }

    await writePluginMcpCredential('plugin@example', 'calendar-api', credential)

    expect(invokeMock.mock.calls.map(([, args]) => args)).toEqual([
      { method: 'plugin/mcpCredential/write', params: { pluginId: 'plugin@example', serverName: 'calendar-api', credential } },
      { method: 'config/mcpServer/reload', params: {} },
    ])
  })

  it('loads workspace hooks and records the reviewed source hash as trusted', async () => {
    invokeMock.mockResolvedValue({ data: [{ cwd: '/workspace', hooks: [], warnings: [], errors: [] }] })

    await listHooks('/workspace')
    await trustHook('plugin.hook.with.dots', 'sha256:current')

    expect(invokeMock.mock.calls.map(([, args]) => args)).toEqual([
      { method: 'hooks/list', params: { cwds: ['/workspace'] } },
      {
        method: 'config/batchWrite',
        params: {
          edits: [{
            keyPath: 'hooks.state."plugin.hook.with.dots".trusted_hash',
            value: 'sha256:current',
            mergeStrategy: 'replace',
          }],
          reloadUserConfig: true,
        },
      },
    ])
  })

  it('searches remote plugins in scope and follows every result cursor', async () => {
    invokeMock
      .mockResolvedValueOnce({ data: [{ marketplaceName: 'remote-one', marketplacePath: null, plugin: { id: 'one', name: 'one' } }], nextCursor: 'page-2' })
      .mockResolvedValueOnce({ data: [{ marketplaceName: 'remote-two', marketplacePath: null, plugin: { id: 'two', name: 'two' } }], nextCursor: null })

    const results = await searchPlugins('calendar', 'personal', '/workspace')

    expect(results.map(result => result.plugin.name)).toEqual(['one', 'two'])
    expect(invokeMock.mock.calls.map(([, args]) => args)).toEqual([
      { method: 'plugin/search', params: { searchTerm: 'calendar', scope: 'personal', cwds: ['/workspace'], limit: 100 } },
      { method: 'plugin/search', params: { searchTerm: 'calendar', scope: 'personal', cwds: ['/workspace'], limit: 100, cursor: 'page-2' } },
    ])
  })

  it('refreshes app metadata and installed state before reloading MCP connections', async () => {
    invokeMock
      .mockResolvedValueOnce({ data: [{ id: 'calendar', name: 'Calendar', isAccessible: true }], nextCursor: 'next' })
      .mockResolvedValueOnce({ data: [{ id: 'docs', name: 'Docs', isAccessible: false }], nextCursor: null })
      .mockResolvedValueOnce({ apps: [{ id: 'calendar', enabled: true, callable: true }] })
      .mockResolvedValueOnce({})

    const result = await refreshAppAuthStatus()

    expect(result.apps.map(app => app.id)).toEqual(['calendar', 'docs'])
    expect(result.installedApps[0].callable).toBe(true)
    expect(invokeMock.mock.calls.map(([, args]) => args)).toEqual([
      { method: 'app/list', params: { limit: 100, forceRefetch: true } },
      { method: 'app/list', params: { limit: 100, forceRefetch: true, cursor: 'next' } },
      { method: 'app/installed', params: { forceRefresh: true } },
      { method: 'config/mcpServer/reload', params: {} },
    ])
  })

  it('pages MCP prompts and preserves arbitrary prompt, completion, resource, and tool JSON payloads', async () => {
    const promptPayload = { description: 'Use prompt data', messages: [{ role: 'user', content: [{ type: 'text', text: 'Hello' }, { type: 'image', data: 'abc', mimeType: 'image/png' }] }] }
    const completionPayload = { completion: { values: ['one', 'two'], total: 2, hasMore: false } }
    const resourcePayload = { contents: [{ uri: 'file://x', mimeType: 'application/json', text: '{"value":1}', extension: { raw: true } }], originCallId: null }
    const toolPayload = { content: [{ type: 'text', text: 'done' }, { type: 'resource', resource: { uri: 'x', mimeType: 'x/custom', blob: 'AA==' } }], structuredContent: { id: 4 }, isError: false }
    invokeMock
      .mockResolvedValueOnce({ prompts: [{ name: 'first' }], nextCursor: 'next' })
      .mockResolvedValueOnce({ prompts: [{ name: 'second' }], nextCursor: null })
      .mockResolvedValueOnce(promptPayload)
      .mockResolvedValueOnce(completionPayload)
      .mockResolvedValueOnce(resourcePayload)
      .mockResolvedValueOnce(toolPayload)

    const prompts = await listMcpPrompts('workspace', 'thread-1')
    const prompt = await getMcpPrompt('workspace', 'first', { query: 'hello' }, 'thread-1')
    const completion = await completeMcpArgument('workspace', { type: 'ref/prompt', name: 'first' }, { name: 'query', value: 'h' }, { arguments: { limit: '3' } }, 'thread-1')
    const resource = await readMcpResource('workspace', 'file://x', 'thread-1')
    const tool = await callMcpTool('workspace', 'lookup', { query: 'h' }, 'thread-1')

    expect(prompts.map(item => item.name)).toEqual(['first', 'second'])
    expect(prompt).toEqual(promptPayload)
    expect(completion).toEqual(completionPayload)
    expect(resource).toEqual(resourcePayload)
    expect(tool).toEqual(toolPayload)
    expect(invokeMock.mock.calls.map(([, args]) => args)).toEqual([
      { method: 'mcpServer/prompts/list', params: { server: 'workspace', threadId: 'thread-1' } },
      { method: 'mcpServer/prompts/list', params: { server: 'workspace', threadId: 'thread-1', cursor: 'next' } },
      { method: 'mcpServer/prompt/get', params: { server: 'workspace', name: 'first', arguments: { query: 'hello' }, threadId: 'thread-1' } },
      { method: 'mcpServer/completion/complete', params: { server: 'workspace', ref: { type: 'ref/prompt', name: 'first' }, argument: { name: 'query', value: 'h' }, context: { arguments: { limit: '3' } }, threadId: 'thread-1' } },
      { method: 'mcpServer/resource/read', params: { server: 'workspace', uri: 'file://x', threadId: 'thread-1' } },
      { method: 'mcpServer/tool/call', params: { server: 'workspace', tool: 'lookup', arguments: { query: 'h' }, threadId: 'thread-1' } },
    ])
  })

  it('scopes MCP status and OAuth requests to the active thread when provided', async () => {
    invokeMock.mockResolvedValueOnce({ data: [], nextCursor: null }).mockResolvedValueOnce({ authorizationUrl: 'https://auth.example' })

    await listMcpServerStatus('thread-1')
    await startMcpOAuth('workspace-server', 'thread-1')

    expect(invokeMock.mock.calls.map(([, args]) => args)).toEqual([
      { method: 'mcpServerStatus/list', params: { limit: 100, detail: 'full', threadId: 'thread-1' } },
      { method: 'mcpServer/oauth/login', params: { name: 'workspace-server', threadId: 'thread-1' } },
    ])
  })
})
