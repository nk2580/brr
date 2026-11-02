import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

const api = vi.hoisted(() => ({
  addMarketplace: vi.fn(),
  installPlugin: vi.fn(),
  listPlugins: vi.fn(),
  readPlugin: vi.fn(),
  uninstallPlugin: vi.fn(),
  setPluginEnabled: vi.fn(),
  upgradeMarketplaces: vi.fn(),
  removeMarketplace: vi.fn(),
  readEffectiveConfig: vi.fn(),
  listMcpServerStatus: vi.fn(),
  saveMcpServer: vi.fn(),
  removeMcpServer: vi.fn(),
  setMcpServerEnabled: vi.fn(),
  startMcpOAuth: vi.fn(),
  listHooks: vi.fn(),
  trustHook: vi.fn(),
  searchPlugins: vi.fn(),
  refreshAppAuthStatus: vi.fn(),
  listMcpPrompts: vi.fn(),
  getMcpPrompt: vi.fn(),
  completeMcpArgument: vi.fn(),
  readMcpResource: vi.fn(),
  callMcpTool: vi.fn(),
  writePluginMcpCredential: vi.fn(),
}))
const { openExternalUrl } = vi.hoisted(() => ({ openExternalUrl: vi.fn() }))

vi.mock('../lib/extensions', () => api)
vi.mock('../lib/external-url', () => ({ openExternalUrl }))

import { ExtensionsPanel } from './ExtensionsPanel'

describe('ExtensionsPanel', () => {
  beforeEach(() => {
    Object.values(api).forEach(mock => mock.mockReset())
    api.listPlugins.mockResolvedValue({
      marketplaces: [{ name: 'local', path: '/plugins/marketplace.json', plugins: [{ id: 'calendar@local', name: 'calendar', installed: false, enabled: false, source: { type: 'local', path: '/plugins/calendar' } }] }],
      marketplaceLoadErrors: [],
    })
    api.installPlugin.mockResolvedValue({ authPolicy: 'ON_INSTALL', appsNeedingAuth: [{ id: 'calendar-app', name: 'Calendar', installUrl: 'https://example.com/install' }] })
    api.listHooks.mockResolvedValue({ data: [{ cwd: '/workspace', hooks: [], warnings: [], errors: [] }] })
  })

  it('installs a selected plugin and keeps its app authentication visibly pending', async () => {
    const user = userEvent.setup()
    render(<ExtensionsPanel onClose={vi.fn()} cwd="/workspace" />)

    await user.click(await screen.findByRole('button', { name: 'Install calendar' }))

    await waitFor(() => expect(api.installPlugin).toHaveBeenCalledWith({
      marketplacePath: '/plugins/marketplace.json',
      pluginName: 'calendar',
    }))
    expect(await screen.findByText(/Calendar.*authentication is still needed/i)).toBeInTheDocument()
    expect(screen.getByText(/installation complete/i)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Set up Calendar' }))
    expect(openExternalUrl).toHaveBeenCalledWith('https://example.com/install')
    api.refreshAppAuthStatus.mockResolvedValue({
      apps: [{ id: 'calendar-app', isAccessible: true }],
      installedApps: [{ id: 'calendar-app', enabled: true, callable: true }],
    })
    await user.click(screen.getByRole('button', { name: 'Refresh Calendar auth status' }))
    expect(await screen.findByText(/Calendar: Authenticated and callable/)).toBeInTheDocument()
  })

  it('shows plugin hooks for source review and trusts only the displayed source hash', async () => {
    const user = userEvent.setup()
    api.listHooks.mockResolvedValue({ data: [{ cwd: '/workspace', hooks: [{
      key: 'calendar.on-open', eventName: 'sessionStart', handlerType: 'command', command: 'node hook.js',
      sourcePath: '/plugins/calendar/hooks.json', pluginId: 'calendar@local', enabled: true, isManaged: false,
      currentHash: 'sha256:current', trustStatus: 'untrusted', timeoutSec: 10,
    }], warnings: [], errors: [] }] })
    api.readPlugin.mockResolvedValue({ plugin: { summary: { id: 'calendar@local', name: 'calendar' }, description: 'Calendar access', skills: [], hooks: [{ key: 'calendar.on-open' }], apps: [], mcpServers: [], onboardingSkill: null } })

    render(<ExtensionsPanel onClose={vi.fn()} cwd="/workspace" />)
    await user.click(await screen.findByRole('button', { name: /calendar.*available/i }))
    await user.click(await screen.findByText(/sessionStart · untrusted/i))
    expect(screen.getByText('node hook.js')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Trust this reviewed version' }))

    await waitFor(() => expect(api.trustHook).toHaveBeenCalledWith('calendar.on-open', 'sha256:current'))
  })

  it('starts the declared onboarding skill only after an explicit user click', async () => {
    const user = userEvent.setup()
    const onUsePrompt = vi.fn()
    api.readPlugin.mockResolvedValue({ plugin: {
      summary: { id: 'calendar@local', name: 'calendar' }, description: 'Calendar access',
      skills: [], hooks: [], apps: [], mcpServers: [],
      onboardingSkill: { name: 'calendar-setup', description: 'Connect the calendar account.' },
    } })
    render(<ExtensionsPanel onClose={vi.fn()} onUsePrompt={onUsePrompt} cwd="/workspace" />)

    await user.click(await screen.findByRole('button', { name: /calendar.*available/i }))
    const start = await screen.findByRole('button', { name: 'Start setup skill' })
    expect(onUsePrompt).not.toHaveBeenCalled()
    await user.click(start)
    expect(onUsePrompt).toHaveBeenCalledWith('$calendar-setup')
  })

  it('shows portable MCP setup errors returned with plugin details', async () => {
    const user = userEvent.setup()
    api.readPlugin.mockResolvedValue({ plugin: {
      summary: { id: 'calendar@local', name: 'calendar' }, description: 'Calendar access',
      skills: [], hooks: [], apps: [], mcpServers: [], onboardingSkill: null,
      mcpServerErrors: [{
        serverName: 'calendar-api', message: 'Unsupported auth configuration: OAuth proxy setup is required.',
        authDescriptor: { type: 'oauth', clientId: 'must-not-be-rendered' },
      }],
    } })

    render(<ExtensionsPanel onClose={vi.fn()} cwd="/workspace" />)
    await user.click(await screen.findByRole('button', { name: /calendar.*available/i }))

    expect(await screen.findByRole('region', { name: 'Plugin MCP setup issues' })).toHaveTextContent('calendar-api')
    expect(screen.getByRole('region', { name: 'Plugin MCP setup issues' })).toHaveTextContent('Unsupported auth configuration: OAuth proxy setup is required.')
    expect(screen.getByRole('region', { name: 'Plugin MCP setup issues' })).toHaveTextContent('Authentication type: oauth')
    expect(screen.getByRole('region', { name: 'Plugin MCP setup issues' })).not.toHaveTextContent('must-not-be-rendered')
    expect(screen.queryByText(/calendar-api.*ready/i)).not.toBeInTheDocument()
  })

  it('shows plugin MCP credentials from current auth declarations and refreshes stored state after save and removal', async () => {
    const user = userEvent.setup()
    api.listPlugins.mockResolvedValue({
      marketplaces: [{ name: 'local', path: '/plugins/marketplace.json', plugins: [{ id: 'calendar@local', name: 'calendar', installed: true, enabled: true, source: { type: 'local', path: '/plugins/calendar' } }] }],
      marketplaceLoadErrors: [],
    })
    let credentialConfigured = false
    api.readPlugin.mockImplementation(async () => ({ plugin: {
      summary: { id: 'calendar@local', name: 'calendar' }, description: 'Calendar access',
      skills: [], hooks: [], apps: [], mcpServers: [],
      mcpServerErrors: credentialConfigured ? [] : [{
        serverName: 'calendar-api', message: 'Provider credential is missing.',
        authDescriptor: { type: 'api_key', headerScheme: 'bearer', serverUrl: 'https://calendar.test/mcp' },
      }],
      mcpServerAuth: [{
        serverName: 'calendar-api', credentialConfigured,
        authDescriptor: { type: 'api_key', headerScheme: 'bearer', serverUrl: 'https://calendar.test/mcp' },
      }],
    } }))
    api.writePluginMcpCredential.mockImplementation(async (_pluginId: string, _serverName: string, value: unknown) => {
      credentialConfigured = value !== null
    })
    api.readEffectiveConfig.mockResolvedValue({ config: { mcpServers: {} } })
    api.listMcpServerStatus.mockResolvedValue([])

    render(<ExtensionsPanel onClose={vi.fn()} cwd="/workspace" />)
    await user.click(await screen.findByRole('button', { name: /calendar.*enabled/i }))
    const auth = await screen.findByRole('region', { name: 'Plugin MCP authentication' })
    expect(within(auth).getByRole('form', { name: 'Authentication setup for calendar-api' })).toBeInTheDocument()
    expect(within(auth).getAllByRole('form', { name: 'Authentication setup for calendar-api' })).toHaveLength(1)
    expect(within(auth).queryByRole('status')).not.toBeInTheDocument()

    await user.type(within(auth).getByLabelText('API key for calendar-api'), 'plugin-key')
    await user.click(within(auth).getByRole('button', { name: 'Store credential' }))
    await waitFor(() => expect(api.writePluginMcpCredential).toHaveBeenCalledWith('calendar@local', 'calendar-api', { kind: 'token', token: 'plugin-key' }))
    expect(await within(auth).findByRole('note')).toHaveTextContent(/credential is stored/i)
    expect(screen.queryByRole('region', { name: 'Plugin MCP setup issues' })).not.toBeInTheDocument()

    await user.click(within(auth).getByRole('button', { name: 'Remove stored credential' }))
    await waitFor(() => expect(api.writePluginMcpCredential).toHaveBeenLastCalledWith('calendar@local', 'calendar-api', null))
    await waitFor(() => expect(within(auth).queryByRole('note')).not.toBeInTheDocument())
    expect(await within(auth).findByRole('status')).toHaveTextContent(/stored credential removed/i)
  })

  it('searches a selected plugin scope and installs a remote result from its own marketplace', async () => {
    const user = userEvent.setup()
    api.searchPlugins.mockResolvedValue([{ marketplaceName: 'personal-catalog', marketplacePath: null, plugin: {
      id: 'calendar@personal-catalog', name: 'calendar', installed: false, enabled: false,
      source: { type: 'remote' }, installPolicy: 'AVAILABLE',
    } }])
    render(<ExtensionsPanel onClose={vi.fn()} cwd="/workspace" />)

    await user.type(screen.getByLabelText('Search plugins'), 'calendar')
    await user.selectOptions(screen.getByLabelText('Catalog scope'), 'personal')
    await user.click(screen.getByRole('button', { name: 'Search' }))
    await user.click(await within(screen.getByRole('region', { name: 'Remote plugin search' })).findByRole('button', { name: 'Install calendar' }))

    expect(api.searchPlugins).toHaveBeenCalledWith('calendar', 'personal', '/workspace')
    expect(api.installPlugin).toHaveBeenCalledWith({ remoteMarketplaceName: 'personal-catalog', pluginName: 'calendar' })
  })

  it('shows MCP save failures and does not claim a successful reload', async () => {
    const user = userEvent.setup()
    api.readEffectiveConfig.mockResolvedValue({ config: { mcpServers: {} } })
    api.listMcpServerStatus.mockResolvedValue([])
    api.saveMcpServer.mockRejectedValue(new Error('invalid MCP configuration'))
    render(<ExtensionsPanel onClose={vi.fn()} />)

    await user.click(await screen.findByRole('tab', { name: 'MCP connections' }))
    await user.type(screen.getByLabelText('Server name'), 'local-api')
    await user.clear(screen.getByLabelText('MCP server configuration JSON'))
    fireEvent.change(screen.getByLabelText('MCP server configuration JSON'), { target: { value: '{"command":"node","args":["server.js"]}' } })
    await user.click(screen.getByRole('button', { name: 'Save MCP server' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('invalid MCP configuration')
    expect(screen.queryByText(/reloaded/i)).not.toBeInTheDocument()
  })

  it('hides stored MCP secrets in the editor and preserves them when unchanged', async () => {
    const user = userEvent.setup()
    api.readEffectiveConfig.mockResolvedValue({ config: { mcpServers: {
      'local-api': {
        command: 'node', env: { API_TOKEN: 'do-not-render-this' },
        httpHeaders: { Authorization: 'Bearer do-not-render-this-either' },
        http_headers: { 'X-Custom-Auth': 'stored-secret' },
        url: 'https://user:password@example.test/mcp?access_token=url-secret',
        customRuntimeField: { keep: true },
      },
    } } })
    api.listMcpServerStatus.mockResolvedValue([])
    api.saveMcpServer.mockResolvedValue(undefined)
    render(<ExtensionsPanel onClose={vi.fn()} />)

    await user.click(await screen.findByRole('tab', { name: 'MCP connections' }))
    await user.selectOptions(screen.getByLabelText('Select existing MCP server'), 'local-api')
    const editor = screen.getByLabelText('MCP server configuration JSON') as HTMLTextAreaElement
    expect(editor.value).toContain('[[stored secret hidden]]')
    expect(editor.value).toContain('customRuntimeField')
    expect(editor.value).not.toContain('do-not-render-this')
    expect(editor.value).not.toContain('stored-secret')
    expect(editor.value).not.toContain('url-secret')
    await user.click(screen.getByRole('button', { name: 'Save MCP server' }))

    await waitFor(() => expect(api.saveMcpServer).toHaveBeenCalledWith('local-api', {
      command: 'node', env: { API_TOKEN: 'do-not-render-this' },
      httpHeaders: { Authorization: 'Bearer do-not-render-this-either' },
      http_headers: { 'X-Custom-Auth': 'stored-secret' },
      url: 'https://user:password@example.test/mcp?access_token=url-secret',
      customRuntimeField: { keep: true },
    }))
  })

  it('keeps retrieved MCP prompt content raw until the user explicitly sends its text', async () => {
    const user = userEvent.setup()
    const onUsePrompt = vi.fn()
    api.readEffectiveConfig.mockResolvedValue({ config: { mcpServers: { 'prompt-server': { command: 'node' } } } })
    api.listMcpServerStatus.mockResolvedValue([{
      name: 'prompt-server', runtimeStatus: 'connected', tools: {},
      resources: [], resourceTemplates: [],
    }])
    api.listMcpPrompts.mockResolvedValue([{ name: 'summarize', description: 'A sample prompt', arguments: [{ name: 'query', required: true }] }])
    api.getMcpPrompt.mockResolvedValue({ messages: [{ role: 'user', content: [
      { type: 'text', text: 'Please summarize this file.' },
      { type: 'image', data: 'AAECAw==', mimeType: 'image/png' },
    ] }] })
    api.completeMcpArgument.mockResolvedValue({ completion: { values: ['query one', 'query two'], total: 2, hasMore: false } })

    render(<ExtensionsPanel onClose={vi.fn()} threadId="thread-1" onUsePrompt={onUsePrompt} />)
    await user.click(await screen.findByRole('tab', { name: 'MCP connections' }))
    await user.click(await screen.findByRole('button', { name: 'Load MCP prompts' }))
    await user.click(screen.getByText('summarize'))
    await user.type(screen.getByLabelText('Prompt argument query'), 'hello')
    await user.click(await screen.findByRole('button', { name: 'Get prompt summarize' }))

    expect(await screen.findByLabelText('Retrieved MCP prompt payload')).toHaveTextContent('AAECAw==')
    expect(onUsePrompt).not.toHaveBeenCalled()
    await user.click(screen.getByRole('button', { name: 'Use prompt as message' }))
    expect(onUsePrompt).toHaveBeenCalledWith('user: Please summarize this file.')
    await user.click(screen.getByRole('button', { name: 'Get completions' }))
    expect(await screen.findByLabelText('MCP completion result')).toHaveTextContent('query two')
    expect(api.completeMcpArgument).toHaveBeenCalledWith(
      'prompt-server', { type: 'ref/prompt', name: 'summarize' }, { name: 'query', value: '' }, {}, 'thread-1',
    )
  })

  it('preserves explicit empty and whitespace prompt arguments and isolates arguments by prompt', async () => {
    const user = userEvent.setup()
    api.readEffectiveConfig.mockResolvedValue({ config: { mcpServers: { 'prompt-server': { command: 'node' } } } })
    api.listMcpServerStatus.mockResolvedValue([{ name: 'prompt-server', runtimeStatus: 'connected', tools: {}, resources: [], resourceTemplates: [] }])
    api.listMcpPrompts.mockResolvedValue([
      { name: 'first', arguments: [{ name: 'required', required: true }, { name: 'optional' }] },
      { name: 'second', arguments: [{ name: 'secondOnly' }, { name: 'untouchedOptional' }] },
    ])
    api.getMcpPrompt.mockResolvedValue({ messages: [] })

    render(<ExtensionsPanel onClose={vi.fn()} threadId="thread-args" />)
    await user.click(await screen.findByRole('tab', { name: 'MCP connections' }))
    await user.click(await screen.findByRole('button', { name: 'Load MCP prompts' }))
    await user.click(screen.getByText('first'))
    fireEvent.change(screen.getByLabelText('Prompt argument optional'), { target: { value: '  ' } })
    await user.click(screen.getByRole('button', { name: 'Get prompt first' }))
    await waitFor(() => expect(api.getMcpPrompt).toHaveBeenNthCalledWith(1, 'prompt-server', 'first', { required: '', optional: '  ' }, 'thread-args'))

    await user.click(screen.getByText('second'))
    await user.type(screen.getByLabelText('Prompt argument secondOnly'), 'isolated')
    await user.click(screen.getByRole('button', { name: 'Get prompt second' }))
    await waitFor(() => expect(api.getMcpPrompt).toHaveBeenNthCalledWith(2, 'prompt-server', 'second', { secondOnly: 'isolated' }, 'thread-args'))
  })

  it('reads MCP resources and calls tools with the active thread while displaying raw results', async () => {
    const user = userEvent.setup()
    api.readEffectiveConfig.mockResolvedValue({ config: { mcpServers: { 'prompt-server': { command: 'node' } } } })
    api.listMcpServerStatus.mockResolvedValue([{
      name: 'prompt-server', runtimeStatus: 'connected',
      tools: { lookup: { name: 'lookup', inputSchema: { type: 'object', properties: { query: { type: 'string' } } } } },
      resources: [{ uri: 'file://docs', name: 'docs' }], resourceTemplates: [],
    }])
    api.readMcpResource.mockResolvedValue({ contents: [{ type: 'resource', blob: 'AAECAw==', custom: { nested: true } }] })
    api.callMcpTool.mockResolvedValue({ content: [{ type: 'image', data: 'raw-image-data' }], structuredContent: { count: 3 }, isError: false })
    render(<ExtensionsPanel onClose={vi.fn()} threadId="thread-9" />)

    await user.click(await screen.findByRole('tab', { name: 'MCP connections' }))
    await user.selectOptions(await screen.findByLabelText('MCP resource'), 'file://docs')
    await user.click(screen.getByRole('button', { name: 'Read MCP resource' }))
    expect(await screen.findByLabelText('MCP resource result')).toHaveTextContent('AAECAw==')
    await user.selectOptions(screen.getByLabelText('MCP tool'), 'lookup')
    await user.click(screen.getByRole('button', { name: 'Call MCP tool' }))
    expect(await screen.findByLabelText('MCP tool result')).toHaveTextContent('raw-image-data')
    expect(api.callMcpTool).toHaveBeenCalledWith('prompt-server', 'lookup', {}, 'thread-9')
  })

  it('lets a user retry OAuth for a plugin-provided runtime server without adding local config', async () => {
    const user = userEvent.setup()
    api.readEffectiveConfig.mockResolvedValue({ config: { mcpServers: {} } })
    api.listMcpServerStatus.mockResolvedValue([{
      name: 'plugin-calendar', runtimeStatus: 'authenticationRequired', tools: {}, resources: [], resourceTemplates: [],
    }])
    api.startMcpOAuth.mockResolvedValue({ authorizationUrl: 'https://auth.example/plugin-calendar' })
    render(<ExtensionsPanel onClose={vi.fn()} threadId="thread-9" />)

    await user.click(await screen.findByRole('tab', { name: 'MCP connections' }))
    await user.click(await screen.findByRole('button', { name: 'Sign in plugin-calendar' }))
    expect(api.startMcpOAuth).toHaveBeenCalledWith('plugin-calendar', 'thread-9')
    await user.click(await screen.findByRole('button', { name: 'Open authorization page' }))
    expect(openExternalUrl).toHaveBeenCalledWith('https://auth.example/plugin-calendar')
    expect(api.saveMcpServer).not.toHaveBeenCalled()
  })
})
