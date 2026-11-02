import { useCallback, useEffect, useMemo, useState } from 'react'
import { openExternalUrl } from '../lib/external-url'
import {
  addMarketplace, installPlugin, listMcpServerStatus, listPlugins, readEffectiveConfig, readPlugin,
  callMcpTool, completeMcpArgument, getMcpPrompt, listHooks, listMcpPrompts, readMcpResource, refreshAppAuthStatus,
  removeMarketplace, removeMcpServer, saveMcpServer, searchPlugins, setMcpServerEnabled, setPluginEnabled,
  startMcpOAuth, trustHook, uninstallPlugin, upgradeMarketplaces,
  type HookMetadata, type McpPromptPayload, type McpPromptSummary, type McpServerStatus, type PluginList, type PluginMarketplace, type PluginSearchResult, type PluginSearchScope, type PluginSummary,
} from '../lib/extensions'
import { Button } from './ui/button'
import { PluginCredentialSetup } from './PluginCredentialSetup'
import './ExtensionsPanel.css'

type Props = { cwd?: string; threadId?: string; onUsePrompt?: (text: string) => void; onClose: () => void }
type SelectedPlugin = { plugin: PluginSummary; marketplace: PluginMarketplace }
type PluginMcpServerError = { serverName: string; message: string; authDescriptor?: Record<string, any> | null }
type PluginMcpServerAuth = { serverName: string; authDescriptor?: Record<string, any> | null; credentialConfigured?: boolean }
const hiddenSecret = '[[stored secret hidden]]'
const secretKey = /(token|secret|password|authorization|credential|api.?key|private.?key)/i

function safeEditorValue(value: unknown, key = ''): unknown {
  if (/^(env|env_?http_?headers|http_?headers|headers)$/i.test(key)) {
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      return Object.fromEntries(Object.entries(value).map(([name, entry]) => [name, typeof entry === 'string' ? hiddenSecret : safeEditorValue(entry, name)]))
    }
  }
  if (secretKey.test(key) && value !== null && value !== undefined) return hiddenSecret
  if (key === 'url' && typeof value === 'string' && urlContainsSecrets(value)) return hiddenSecret
  if (Array.isArray(value)) return value.map(item => safeEditorValue(item))
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([name, entry]) => [name, safeEditorValue(entry, name)]))
  return value
}

function urlContainsSecrets(value: string) {
  try {
    const url = new URL(value)
    return Boolean(url.username || url.password || [...url.searchParams.keys()].some(key => secretKey.test(key)))
  } catch { return false }
}

function promptMessageText(prompt: McpPromptPayload) {
  return prompt.messages.map(message => {
    const content = Array.isArray(message.content) ? message.content : [message.content]
    const text = content.flatMap((block: any) => {
      if (typeof block === 'string') return [block]
      if (block?.type === 'text' && typeof block.text === 'string') return [block.text]
      return []
    }).join('\n')
    return text ? `${message.role}: ${text}` : ''
  }).filter(Boolean).join('\n\n')
}

const promptArgumentKey = (server: string, promptName: string) => JSON.stringify([server, promptName])

function restoreHiddenSecrets(original: unknown, edited: unknown): unknown {
  if (edited === hiddenSecret) return original
  if (Array.isArray(edited)) {
    return edited.map((entry, index) => restoreHiddenSecrets(Array.isArray(original) ? original[index] : undefined, entry))
  }
  if (!edited || typeof edited !== 'object') return edited
  const before = original && typeof original === 'object' && !Array.isArray(original) ? original as Record<string, unknown> : {}
  return Object.fromEntries(Object.entries(edited as Record<string, unknown>).map(([key, value]) => [key, restoreHiddenSecrets(before[key], value)]))
}

const errorText = (reason: unknown) => reason instanceof Error ? reason.message : String(reason)
const marketplaceTitle = (marketplace: PluginMarketplace) => marketplace.interface?.displayName || marketplace.name
function supportsPluginCredential(auth: Record<string, any>) {
  if (auth.type === 'api_key') return ['basic', 'bearer', 'custom_header'].includes(auth.headerScheme)
  return ['oauth', 'mixed'].includes(auth.type) && auth.client?.mode === 'provided'
    && ['client_secret_basic', 'client_secret_post', 'private_key_jwt'].includes(auth.client?.tokenEndpointAuthMethod)
}

export function ExtensionsPanel({ cwd, threadId, onUsePrompt, onClose }: Props) {
  const [tab, setTab] = useState<'plugins' | 'mcp'>('plugins')
  const [plugins, setPlugins] = useState<PluginList | null>(null)
  const [selectedPlugin, setSelectedPlugin] = useState<SelectedPlugin | null>(null)
  const [marketplaceSource, setMarketplaceSource] = useState('')
  const [marketplaceRef, setMarketplaceRef] = useState('')
  const [marketplaceSparse, setMarketplaceSparse] = useState('')
  const [pluginSearchTerm, setPluginSearchTerm] = useState('')
  const [pluginSearchScope, setPluginSearchScope] = useState<PluginSearchScope>('global')
  const [pluginSearchResults, setPluginSearchResults] = useState<PluginSearchResult[]>([])
  const [pendingAuth, setPendingAuth] = useState<{ id?: string; name: string; installUrl?: string | null; status?: string }[]>([])
  const [pluginHooks, setPluginHooks] = useState<HookMetadata[]>([])
  const [servers, setServers] = useState<Record<string, Record<string, unknown>>>({})
  const [serverStatuses, setServerStatuses] = useState<McpServerStatus[]>([])
  const [serverName, setServerName] = useState('')
  const [serverJson, setServerJson] = useState('{}')
  const [oauthUrls, setOauthUrls] = useState<Record<string, string>>({})
  const [explorerServer, setExplorerServer] = useState('')
  const [mcpPrompts, setMcpPrompts] = useState<McpPromptSummary[]>([])
  const [selectedPrompt, setSelectedPrompt] = useState<McpPromptSummary | null>(null)
  const [promptArguments, setPromptArguments] = useState<Record<string, Record<string, string>>>({})
  const [retrievedPrompt, setRetrievedPrompt] = useState<McpPromptPayload | null>(null)
  const [completionArgument, setCompletionArgument] = useState('')
  const [completionValue, setCompletionValue] = useState('')
  const [completionContext, setCompletionContext] = useState('{}')
  const [completionResult, setCompletionResult] = useState<unknown>(null)
  const [selectedResource, setSelectedResource] = useState('')
  const [resourceResult, setResourceResult] = useState<unknown>(null)
  const [selectedTool, setSelectedTool] = useState('')
  const [toolArguments, setToolArguments] = useState('{}')
  const [toolResult, setToolResult] = useState<unknown>(null)
  const [busy, setBusy] = useState('')
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const [mcpLoaded, setMcpLoaded] = useState(false)

  const refreshPlugins = useCallback(async (force = false) => {
    const result = await listPlugins(cwd, force)
    setPlugins(result)
    setSelectedPlugin(current => current ? findPlugin(result, current.plugin.id) : null)
    return result
  }, [cwd])

  const refreshMcp = useCallback(async () => {
    const [configResult, statusResult] = await Promise.allSettled([readEffectiveConfig(cwd), listMcpServerStatus(threadId)])
    if (configResult.status === 'fulfilled') {
      const config = configResult.value.config
      const loaded = (config.mcpServers ?? config.mcp_servers ?? {}) as Record<string, Record<string, unknown>>
      setServers(loaded)
      setMcpLoaded(true)
      if (serverName && loaded[serverName]) setServerJson(JSON.stringify(safeEditorValue(loaded[serverName]), null, 2))
    }
    if (statusResult.status === 'fulfilled') {
      setServerStatuses(statusResult.value)
      setExplorerServer(current => current || statusResult.value[0]?.name || '')
    }
    const failures = [configResult, statusResult].filter((result): result is PromiseRejectedResult => result.status === 'rejected')
    if (failures.length) setError(failures.map(failure => errorText(failure.reason)).join('\n'))
    return { configResult, statusResult }
  }, [cwd, serverName, threadId])

  useEffect(() => {
    void refreshPlugins().catch(reason => setError(errorText(reason)))
  }, [refreshPlugins])

  const run = async (label: string, action: () => Promise<void>) => {
    setBusy(label); setError(''); setMessage('')
    try { await action() }
    catch (reason) { setError(errorText(reason)) }
    finally { setBusy('') }
  }

  const selectPlugin = async (plugin: PluginSummary, marketplace: PluginMarketplace) => {
    setSelectedPlugin({ plugin, marketplace })
    await run(`read:${plugin.id}`, async () => {
      const [detailResult, hooksResult] = await Promise.allSettled([readPlugin({
        ...(marketplace.path ? { marketplacePath: marketplace.path } : { remoteMarketplaceName: marketplace.name }),
        pluginName: plugin.name,
      }), listHooks(cwd)])
      if (detailResult.status === 'fulfilled') {
        setSelectedPlugin({ plugin: { ...plugin, ...detailResult.value.plugin.summary, ...detailResult.value.plugin }, marketplace })
      } else setError(errorText(detailResult.reason))
      if (hooksResult.status === 'fulfilled') {
        const allHooks = hooksResult.value.data.flatMap(entry => entry.hooks)
        const detail = detailResult.status === 'fulfilled' ? detailResult.value.plugin : null
        const detailKeys = new Set((detail?.hooks ?? []).map((hook: any) => hook.key))
        setPluginHooks(allHooks.filter(hook => hook.pluginId === plugin.id || detailKeys.has(hook.key)))
      } else setError(current => [current, errorText(hooksResult.reason)].filter(Boolean).join('\n'))
    })
  }

  const install = (plugin: PluginSummary, marketplace: PluginMarketplace) => run(`install:${plugin.id}`, async () => {
    const result = await installPlugin({
      ...(marketplace.path ? { marketplacePath: marketplace.path } : { remoteMarketplaceName: marketplace.name }),
      pluginName: plugin.name,
    })
    const apps = result.appsNeedingAuth.map(app => ({ id: app.id, name: app.name || app.id || 'An app', installUrl: app.installUrl, status: 'Authentication still pending' }))
    setPendingAuth(apps)
    setMessage(apps.length
      ? `Installation complete. ${apps.map(app => app.name).join(', ')} authentication is still needed before the plugin is ready.`
      : 'Plugin installation completed.')
    await refreshPlugins()
  })

  const searchRemotePlugins = () => run('search:plugins', async () => {
    const results = await searchPlugins(pluginSearchTerm, pluginSearchScope, cwd)
    setPluginSearchResults(results)
    setMessage(results.length ? `Found ${results.length} plugins.` : 'No plugins matched this search.')
  })

  const refreshAppStatus = (app: { id?: string; name: string }) => run(`refresh-app:${app.name}`, async () => {
    const snapshot = await refreshAppAuthStatus()
    const metadata = snapshot.apps.find(candidate => candidate.id === app.id)
    const installed = snapshot.installedApps.find(candidate => candidate.id === app.id)
    const status = !metadata?.isAccessible
      ? 'Authentication still pending'
      : installed?.callable
        ? 'Authenticated and callable'
        : installed?.enabled === false
          ? 'Access is available, but this app is disabled in effective configuration'
          : 'Access is available; the runtime does not report a callable app tool yet'
    setPendingAuth(current => current.map(candidate => candidate.id === app.id ? { ...candidate, status } : candidate))
    await refreshMcp()
    setMessage(`${app.name}: ${status}. MCP connections were reloaded.`)
  })

  const remoteMarketplace = (result: PluginSearchResult): PluginMarketplace => ({
    name: result.marketplaceName,
    path: result.marketplacePath,
    plugins: [result.plugin],
  })

  const loadMcpTab = async () => {
    setTab('mcp')
    if (!mcpLoaded) await run('load:mcp', async () => { await refreshMcp() })
  }

  const chooseServer = (name: string) => {
    setServerName(name)
    setServerJson(JSON.stringify(safeEditorValue(servers[name] ?? {}), null, 2))
    setError(''); setMessage('')
  }

  const saveServer = () => run('save:mcp', async () => {
    const parsed: unknown = JSON.parse(serverJson)
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('MCP server configuration must be a JSON object.')
    if (!serverName.trim()) throw new Error('Enter an MCP server name.')
    const restored = restoreHiddenSecrets(servers[serverName], parsed) as Record<string, unknown>
    await saveMcpServer(serverName, restored)
    setMessage(`Saved and reloaded MCP server “${serverName}”.`)
    setServers(current => ({ ...current, [serverName]: restored }))
    setMcpLoaded(false)
    await refreshMcp()
  })

  const statusByName = useMemo(() => new Map(serverStatuses.map(status => [status.name, status])), [serverStatuses])
  const explorerStatus = statusByName.get(explorerServer)
  const onboardingSkill = selectedPlugin?.plugin.onboardingSkill as { name?: string; description?: string; enabled?: boolean } | undefined
  const pluginMcpServerErrors = Array.isArray(selectedPlugin?.plugin.mcpServerErrors)
    ? selectedPlugin.plugin.mcpServerErrors as PluginMcpServerError[]
    : []
  const pluginMcpServerAuth = Array.isArray(selectedPlugin?.plugin.mcpServerAuth)
    ? selectedPlugin.plugin.mcpServerAuth as PluginMcpServerAuth[]
    : []
  const pluginCredentialSetupsByServer = new Map<string, PluginMcpServerAuth>()
  for (const auth of pluginMcpServerAuth) {
    if (auth && typeof auth.serverName === 'string' && auth.authDescriptor && typeof auth.authDescriptor === 'object') {
      pluginCredentialSetupsByServer.set(auth.serverName, auth)
    }
  }
  for (const issue of pluginMcpServerErrors) {
    if (issue.authDescriptor && !pluginCredentialSetupsByServer.has(issue.serverName)) {
      pluginCredentialSetupsByServer.set(issue.serverName, { serverName: issue.serverName, authDescriptor: issue.authDescriptor, credentialConfigured: false })
    }
  }
  const pluginCredentialSetups = [...pluginCredentialSetupsByServer.values()].filter(entry =>
    entry.authDescriptor && typeof entry.authDescriptor.serverUrl === 'string' && supportsPluginCredential(entry.authDescriptor))

  const loadPrompts = () => run('load:prompts', async () => {
    if (!explorerServer) throw new Error('Select a connected MCP server first.')
    const prompts = await listMcpPrompts(explorerServer, threadId)
    setMcpPrompts(prompts)
    setSelectedPrompt(null)
    setRetrievedPrompt(null)
    setMessage(`Loaded ${prompts.length} MCP prompts from ${explorerServer}.`)
  })

  const getPrompt = (prompt: McpPromptSummary) => run(`get-prompt:${prompt.name}`, async () => {
    const enteredArguments = promptArguments[promptArgumentKey(explorerServer, prompt.name)] ?? {}
    const args = Object.fromEntries((prompt.arguments ?? []).flatMap(argument => {
      const wasEntered = Object.prototype.hasOwnProperty.call(enteredArguments, argument.name)
      return argument.required || wasEntered
        ? [[argument.name, wasEntered ? enteredArguments[argument.name] : '']]
        : []
    }))
    const payload = await getMcpPrompt(explorerServer, prompt.name, args, threadId)
    setSelectedPrompt(prompt)
    setRetrievedPrompt(payload)
    setCompletionArgument(prompt.arguments?.[0]?.name ?? '')
    setCompletionResult(null)
  })

  const completeArgument = () => run('complete:argument', async () => {
    if (!selectedPrompt) throw new Error('Get a prompt before requesting argument completions.')
    if (!completionArgument.trim()) throw new Error('Choose a prompt argument to complete.')
    const context: unknown = JSON.parse(completionContext)
    if (!context || typeof context !== 'object' || Array.isArray(context)) throw new Error('Completion context must be a JSON object.')
    const result = await completeMcpArgument(
      explorerServer,
      { type: 'ref/prompt', name: selectedPrompt.name },
      { name: completionArgument, value: completionValue },
      context as Record<string, unknown>,
      threadId,
    )
    setCompletionResult(result)
  })

  const readResource = () => run('read:resource', async () => {
    if (!selectedResource) throw new Error('Select an MCP resource first.')
    setResourceResult(await readMcpResource(explorerServer, selectedResource, threadId))
  })

  const callTool = () => run('call:tool', async () => {
    if (!selectedTool) throw new Error('Select an MCP tool first.')
    const args: unknown = JSON.parse(toolArguments)
    if (!args || typeof args !== 'object' || Array.isArray(args)) throw new Error('Tool arguments must be a JSON object.')
    setToolResult(await callMcpTool(explorerServer, selectedTool, args, threadId))
  })

  return <aside className="extensions-panel" aria-label="Plugins and MCP connections">
    <header className="extensions-header">
      <div><p className="extensions-kicker">Workspace settings</p><h2>Extensions</h2></div>
      <Button variant="ghost" aria-label="Close extensions" onClick={onClose}>×</Button>
    </header>
    <div id="plugins-sidebar" className="extensions-tabs" role="tablist" aria-label="Extension type">
      <button role="tab" aria-selected={tab === 'plugins'} onClick={() => setTab('plugins')}>Plugins</button>
      <button role="tab" aria-selected={tab === 'mcp'} onClick={() => void loadMcpTab()}>MCP connections</button>
    </div>
    {error && <div className="extensions-error" role="alert"><strong>Action failed</strong><span>{error}</span></div>}
    {message && <div className="extensions-notice" role="status">{message}</div>}
    {pendingAuth.length > 0 && <div className="extensions-notice pending-auth-panel" aria-label="Plugin app authentication status">
      {pendingAuth.map(app => <div className="pending-app-auth" key={`${app.id}:${app.name}`}>
        <span>{app.name}: {app.status}</span>
        {app.installUrl && <Button variant="outline" onClick={() => void run(`auth:${app.name}`, () => openExternalUrl(app.installUrl!))}>Set up {app.name}</Button>}
        <Button variant="ghost" disabled={Boolean(busy)} onClick={() => void refreshAppStatus(app)}>Refresh {app.name} auth status</Button>
      </div>)}
    </div>}
    {busy && <p className="extensions-busy" role="status">Working…</p>}

    {tab === 'plugins' ? <section className="extensions-content" aria-label="Plugins">
      <div className="extensions-section-heading"><div><h3>Plugin marketplaces</h3><p>Add a Git URL, shorthand, or local marketplace path.</p></div>
        <Button variant="outline" disabled={Boolean(busy)} onClick={() => void run('refresh:plugins', async () => { await refreshPlugins(true); setMessage('Plugin marketplaces refreshed.') })}>Refresh</Button>
      </div>
      <div className="marketplace-add">
        <label>Source<input value={marketplaceSource} onChange={event => setMarketplaceSource(event.target.value)} placeholder="owner/repo, Git URL, or local path" /></label>
        <div className="extensions-form-row">
          <label>Git ref (optional)<input value={marketplaceRef} onChange={event => setMarketplaceRef(event.target.value)} placeholder="branch or tag" /></label>
          <label>Sparse paths (optional)<input value={marketplaceSparse} onChange={event => setMarketplaceSparse(event.target.value)} placeholder="plugins, examples" /></label>
        </div>
        <Button disabled={Boolean(busy) || !marketplaceSource.trim()} onClick={() => void run('add:marketplace', async () => {
          await addMarketplace(marketplaceSource.trim(), marketplaceRef.trim() || undefined, marketplaceSparse.split(',').map(path => path.trim()).filter(Boolean))
          setMarketplaceSource(''); setMessage('Marketplace added.'); await refreshPlugins()
        })}>Add marketplace</Button>
      </div>
      {plugins?.marketplaceLoadErrors.map(loadError => <div className="extensions-error" role="status" key={`${loadError.marketplacePath}:${loadError.message}`}><strong>Marketplace could not load</strong><span>{loadError.marketplacePath}: {loadError.message}</span></div>)}
      {!plugins && !busy && <p className="extensions-muted">Loading plugin marketplaces…</p>}
      {plugins?.marketplaces.map(marketplace => <section className="marketplace-card" key={`${marketplace.name}:${marketplace.path}`}>
        <div className="marketplace-title"><h3>{marketplaceTitle(marketplace)}</h3><div>
          <Button variant="ghost" disabled={Boolean(busy)} onClick={() => void run(`upgrade:${marketplace.name}`, async () => {
            const result = await upgradeMarketplaces(marketplace.name)
            if (result.errors.length) setError(result.errors.map(item => `${item.marketplaceName}: ${item.message}`).join('\n'))
            else setMessage('Marketplace updated.')
            await refreshPlugins()
          })}>Update</Button>
          {marketplace.path && <Button variant="ghost" disabled={Boolean(busy)} onClick={() => void run(`remove-marketplace:${marketplace.name}`, async () => {
            await removeMarketplace(marketplace.name); setMessage('Marketplace removed.'); await refreshPlugins()
          })}>Remove</Button>}
        </div></div>
        {marketplace.plugins.map(plugin => <div className="plugin-row" key={plugin.id}>
          <button className="plugin-summary" onClick={() => void selectPlugin(plugin, marketplace)}>
            <strong>{plugin.name}</strong><span>{plugin.source.type} · {plugin.installed ? (plugin.enabled ? 'Enabled' : 'Disabled') : 'Available'}</span>
          </button>
          <div className="plugin-actions">
            {plugin.installed ? <>
              <Button variant="ghost" disabled={Boolean(busy)} onClick={() => void run(`toggle:${plugin.id}`, async () => {
                await setPluginEnabled(plugin.id, !plugin.enabled); await refreshPlugins(); setMessage(`Plugin ${plugin.enabled ? 'disabled' : 'enabled'}.`)
              })}>{plugin.enabled ? 'Disable' : 'Enable'}</Button>
              <Button variant="ghost" disabled={Boolean(busy)} onClick={() => void run(`uninstall:${plugin.id}`, async () => {
                await uninstallPlugin(plugin.id); await refreshPlugins(); setMessage('Plugin uninstalled.')
              })}>Uninstall</Button>
            </> : <Button variant="outline" disabled={Boolean(busy) || plugin.installPolicy === 'NOT_AVAILABLE' || plugin.availability === 'DISABLED_BY_ADMIN'} aria-label={`Install ${plugin.name}`} onClick={() => void install(plugin, marketplace)}>
              {plugin.installPolicy === 'NOT_AVAILABLE' || plugin.availability === 'DISABLED_BY_ADMIN' ? 'Unavailable' : `Install ${plugin.name}`}
            </Button>}
          </div>
        </div>)}
        {marketplace.plugins.length === 0 && <p className="extensions-muted">No plugins in this marketplace.</p>}
      </section>)}
      <section className="remote-plugin-search" aria-label="Remote plugin search">
        <div className="extensions-section-heading"><div><h3>Find plugins</h3><p>Search available catalogs by scope. Results include every source allowed by the runtime.</p></div></div>
        <form className="remote-plugin-search-form" onSubmit={event => { event.preventDefault(); void searchRemotePlugins() }}>
          <label>Search plugins<input value={pluginSearchTerm} onChange={event => setPluginSearchTerm(event.target.value)} placeholder="Calendar, GitHub, …" /></label>
          <label>Catalog scope<select value={pluginSearchScope} onChange={event => setPluginSearchScope(event.target.value as PluginSearchScope)}>
            <option value="global">Global</option><option value="workspace">Workspace</option><option value="personal">Personal</option>
          </select></label>
          <Button type="submit" disabled={Boolean(busy) || !pluginSearchTerm.trim()}>Search</Button>
        </form>
        {pluginSearchResults.map(result => <div className="plugin-row remote-plugin-row" key={`${result.marketplaceName}:${result.plugin.id}`}>
          <button className="plugin-summary" onClick={() => void selectPlugin(result.plugin, remoteMarketplace(result))}>
            <strong>{result.plugin.name}</strong><span>{result.marketplaceName} · {result.plugin.source.type}{result.plugin.installed ? ' · Installed' : ''}</span>
          </button>
          {!result.plugin.installed && <Button variant="outline" disabled={Boolean(busy) || result.plugin.installPolicy === 'NOT_AVAILABLE' || result.plugin.availability === 'DISABLED_BY_ADMIN'} aria-label={`Install ${result.plugin.name}`} onClick={() => void install(result.plugin, remoteMarketplace(result))}>
            {result.plugin.installPolicy === 'NOT_AVAILABLE' || result.plugin.availability === 'DISABLED_BY_ADMIN' ? 'Unavailable' : `Install ${result.plugin.name}`}
          </Button>}
        </div>)}
      </section>
      {selectedPlugin && <section className="plugin-detail" aria-label={`${selectedPlugin.plugin.name} details`}>
        <div className="extensions-section-heading"><div><p className="extensions-kicker">Plugin details</p><h3>{selectedPlugin.plugin.name}</h3></div>
          <Button variant="ghost" aria-label="Close plugin details" onClick={() => setSelectedPlugin(null)}>×</Button></div>
        <p>{String(selectedPlugin.plugin.description ?? (selectedPlugin.plugin.interface as { shortDescription?: string } | null)?.shortDescription ?? 'No description provided.')}</p>
        <dl><dt>Marketplace</dt><dd>{selectedPlugin.marketplace.name}</dd><dt>Plugin ID</dt><dd>{selectedPlugin.plugin.id}</dd><dt>Install policy</dt><dd>{String(selectedPlugin.plugin.installPolicy ?? 'Available')}</dd></dl>
        {Array.isArray(selectedPlugin.plugin.skills) && <p>Skills: {selectedPlugin.plugin.skills.map((skill: any) => skill.name).join(', ') || 'None'}</p>}
        {Array.isArray(selectedPlugin.plugin.mcpServers) && <p>MCP servers: {selectedPlugin.plugin.mcpServers.join(', ') || 'None'}</p>}
        {pluginMcpServerErrors.length > 0 && <section className="plugin-mcp-errors" aria-label="Plugin MCP setup issues">
          <h4>MCP server setup needs attention</h4>
          <p>Some plugin MCP servers could not be enabled by the current runtime configuration.</p>
          <ul>{pluginMcpServerErrors.map((item, index) => {
            const authType = typeof item.authDescriptor?.type === 'string' ? item.authDescriptor.type : null
            return <li key={`${item.serverName}:${index}`}>
              <strong>{item.serverName}</strong>: {item.message}
              {authType && <span> (Authentication type: {authType})</span>}
            </li>
          })}</ul>
        </section>}
        {selectedPlugin.plugin.installed && pluginCredentialSetups.length > 0 && <section className="plugin-mcp-auth" aria-label="Plugin MCP authentication">
          <h4>Credentials for plugin MCP servers</h4>
          {pluginCredentialSetups.map(entry => (
            <PluginCredentialSetup key={entry.serverName} pluginId={selectedPlugin.plugin.id}
              serverName={entry.serverName} auth={entry.authDescriptor!} credentialConfigured={entry.credentialConfigured === true}
              onSaved={async () => {
                await selectPlugin(selectedPlugin.plugin, selectedPlugin.marketplace)
                await refreshMcp()
              }}/>
          ))}
        </section>}
        {onboardingSkill && <div className="plugin-onboarding">
          <strong>Onboarding skill: {onboardingSkill.name}</strong>
          <p>{onboardingSkill.description}</p>
          <p>After required app authentication, run this skill from the composer to follow the plugin setup steps.</p>
          <Button variant="outline" disabled={!onUsePrompt || !onboardingSkill.name} onClick={() => onboardingSkill.name && onUsePrompt?.(`$${onboardingSkill.name}`)}>Start setup skill</Button>
        </div>}
        {pluginHooks.length > 0 && <section className="plugin-hooks" aria-label="Plugin hooks"><h4>Hooks · review before trusting</h4>
          {pluginHooks.map(hook => <details key={hook.key} className="plugin-hook">
            <summary>{hook.eventName} · {hook.trustStatus}{!hook.enabled ? ' · disabled' : ''}</summary>
            <dl><dt>Handler</dt><dd>{hook.handlerType === 'command' ? hook.command : hook.handlerType === 'mcpTool' ? `${hook.server} / ${hook.tool}` : hook.handlerType}</dd>
              <dt>Source</dt><dd>{hook.sourcePath}</dd><dt>Matcher</dt><dd>{hook.matcher || 'All events'}</dd><dt>SHA</dt><dd>{hook.currentHash}</dd></dl>
            {hook.trustStatus !== 'trusted' && hook.trustStatus !== 'managed' && <Button variant="outline" disabled={Boolean(busy)} onClick={() => void run(`trust-hook:${hook.key}`, async () => {
              await trustHook(hook.key, hook.currentHash)
              const updated = await listHooks(cwd)
              setPluginHooks(updated.data.flatMap(entry => entry.hooks).filter(candidate => candidate.pluginId === selectedPlugin.plugin.id || candidate.key === hook.key))
              setMessage(`Reviewed hook ${hook.eventName} and trusted its current source hash.`)
            })}>Trust this reviewed version</Button>}
          </details>)}
        </section>}
      </section>}
    </section> : <section className="extensions-content" aria-label="MCP connections">
      <div className="extensions-section-heading"><div><h3>MCP connections</h3><p>Configure the complete Codex MCP server object as JSON. Stored secret values stay hidden in the editor.</p></div>
        <Button variant="outline" disabled={Boolean(busy)} onClick={() => void run('refresh:mcp', async () => { await refreshMcp(); setMessage('MCP status refreshed.') })}>Refresh</Button>
      </div>
      {mcpLoaded && Object.keys(servers).length === 0 && <p className="extensions-muted">No configured MCP servers.</p>}
      <div className="mcp-server-list" aria-label="Configured MCP servers">
        {Object.entries(servers).map(([name, config]) => {
          const status = statusByName.get(name)
          const enabled = config.enabled !== false
          return <article className="mcp-server-card" key={name}>
            <div className="mcp-server-heading"><button className="plugin-summary" onClick={() => chooseServer(name)}><strong>{name}</strong><span>{status?.runtimeStatus ?? 'Status unavailable'}{status?.serverInfo?.name ? ` · ${status.serverInfo.name}` : ''}</span></button>
              <div className="plugin-actions">
                <Button variant="ghost" disabled={Boolean(busy)} onClick={() => void run(`mcp-toggle:${name}`, async () => {
                  await setMcpServerEnabled(name, !enabled); setMessage(`MCP server ${enabled ? 'disabled' : 'enabled'} and reloaded.`); await refreshMcp()
                })}>{enabled ? 'Disable' : 'Enable'}</Button>
                <Button variant="ghost" disabled={Boolean(busy)} onClick={() => void run(`mcp-remove:${name}`, async () => {
                  await removeMcpServer(name); setServerName(''); setServerJson('{}'); setMessage('MCP server removed and configuration reloaded.'); await refreshMcp()
                })}>Remove</Button>
                <Button variant="ghost" disabled={Boolean(busy)} onClick={() => void run(`oauth:${name}`, async () => {
                  const result = await startMcpOAuth(name, threadId)
                  setOauthUrls(current => ({ ...current, [name]: result.authorizationUrl }))
                  setMessage(`Authorization started for ${name}. Open the returned authorization URL to continue.`)
                })}>Sign in</Button>
              </div>
            </div>
            {oauthUrls[name] && <Button variant="outline" onClick={() => void run(`oauth-open:${name}`, () => openExternalUrl(oauthUrls[name]))}>Open authorization page</Button>}
            {status?.toolsError && <p className="extensions-error">Tool discovery failed: {status.toolsError}</p>}
            {status && <details className="mcp-inventory"><summary>{Object.keys(status.tools).length} tools · {status.resources.length} resources · {status.resourceTemplates?.length ?? 0} resource templates</summary>
              <p>Auth: {status.authStatus ?? 'Unknown'}</p>
              <p>Tools: {Object.keys(status.tools).join(', ') || 'None reported'}</p>
              <p>Resources: {status.resources.length}</p>
            </details>}
          </article>
        })}
      </div>
      <section className="mcp-explorer" aria-label="MCP protocol explorer">
        <div className="extensions-section-heading"><div><h3>MCP protocol explorer</h3><p>Inspect prompts and resources, review tool schemas, and send tool calls to the active conversation.</p></div></div>
        <label className="mcp-explorer-server">Server<select aria-label="Explorer MCP server" value={explorerServer} onChange={event => {
          setExplorerServer(event.target.value); setMcpPrompts([]); setRetrievedPrompt(null); setResourceResult(null); setToolResult(null)
        }}><option value="">Select a server…</option>{serverStatuses.map(status => <option value={status.name} key={status.name}>{status.name}</option>)}</select></label>
        {!threadId && <p className="extensions-muted">Prompt and resource reads can run without a conversation. Tool calls require an active conversation.</p>}
        <div className="mcp-explorer-block">
          <div className="mcp-explorer-title"><h4>Prompts</h4><Button variant="outline" disabled={Boolean(busy) || !explorerServer} onClick={() => void loadPrompts()}>Load MCP prompts</Button></div>
          {mcpPrompts.map(prompt => <details className="mcp-prompt-card" key={prompt.name}>
            <summary>{prompt.title || prompt.name}</summary>
            {prompt.description && <p>{prompt.description}</p>}
            {prompt.arguments?.map(argument => <label className="mcp-argument-field" key={argument.name}>
              <span>{argument.name}{argument.required ? ' · required' : ''}</span>
              <input aria-label={`Prompt argument ${argument.name}`} value={promptArguments[promptArgumentKey(explorerServer, prompt.name)]?.[argument.name] ?? ''} onChange={event => setPromptArguments(current => {
                const key = promptArgumentKey(explorerServer, prompt.name)
                return { ...current, [key]: { ...current[key], [argument.name]: event.target.value } }
              })} placeholder={argument.description || ''} />
            </label>)}
            <Button variant="outline" disabled={Boolean(busy)} aria-label={`Get prompt ${prompt.name}`} onClick={() => void getPrompt(prompt)}>Get prompt {prompt.name}</Button>
          </details>)}
          {retrievedPrompt && <div className="mcp-result-card">
            <div className="mcp-explorer-title"><strong>Retrieved prompt payload</strong>
              <Button variant="outline" disabled={!onUsePrompt || !promptMessageText(retrievedPrompt)} onClick={() => {
                const text = promptMessageText(retrievedPrompt)
                if (text) onUsePrompt?.(text)
              }}>Use prompt as message</Button>
            </div>
            <pre aria-label="Retrieved MCP prompt payload">{JSON.stringify(retrievedPrompt, null, 2)}</pre>
            {!onUsePrompt && <p className="extensions-muted">The composer is unavailable here. Retrieved content stays in this panel.</p>}
          </div>}
          {selectedPrompt?.arguments?.length ? <div className="mcp-completion-card">
            <h4>Complete a prompt argument</h4>
            <div className="extensions-form-row"><label>Argument<select aria-label="Completion argument" value={completionArgument} onChange={event => setCompletionArgument(event.target.value)}>{selectedPrompt.arguments.map(argument => <option value={argument.name} key={argument.name}>{argument.name}</option>)}</select></label>
              <label>Current value<input aria-label="Current completion value" value={completionValue} onChange={event => setCompletionValue(event.target.value)} /></label></div>
            <label>Completion context JSON<textarea aria-label="Completion context JSON" value={completionContext} onChange={event => setCompletionContext(event.target.value)} /></label>
            <Button variant="outline" disabled={Boolean(busy)} onClick={() => void completeArgument()}>Get completions</Button>
            {completionResult !== null && <pre aria-label="MCP completion result">{JSON.stringify(completionResult, null, 2)}</pre>}
          </div> : null}
        </div>
        <div className="mcp-explorer-block">
          <h4>Resources</h4>
          <div className="extensions-form-row"><label>Resource<select aria-label="MCP resource" value={selectedResource} onChange={event => setSelectedResource(event.target.value)}><option value="">Select a resource…</option>{(explorerStatus?.resources ?? []).map((resource: any) => <option value={resource.uri} key={resource.uri}>{resource.name || resource.uri}</option>)}</select></label>
            <Button variant="outline" disabled={Boolean(busy) || !selectedResource} onClick={() => void readResource()}>Read MCP resource</Button></div>
          {resourceResult !== null && <pre aria-label="MCP resource result">{JSON.stringify(resourceResult, null, 2)}</pre>}
          {(explorerStatus?.resourceTemplates?.length ?? 0) > 0 && <details><summary>Resource templates</summary><pre>{JSON.stringify(explorerStatus?.resourceTemplates, null, 2)}</pre></details>}
        </div>
        <div className="mcp-explorer-block">
          <h4>Tools</h4>
          <div className="extensions-form-row"><label>Tool<select aria-label="MCP tool" value={selectedTool} onChange={event => setSelectedTool(event.target.value)}><option value="">Select a tool…</option>{Object.entries(explorerStatus?.tools ?? {}).map(([name, tool]) => <option value={name} key={name}>{name}</option>)}</select></label>
            <Button variant="outline" disabled={Boolean(busy) || !threadId || !selectedTool} onClick={() => void callTool()}>Call MCP tool</Button></div>
          {selectedTool && <details className="mcp-tool-schema"><summary>Selected tool schema</summary><pre>{JSON.stringify(explorerStatus?.tools[selectedTool], null, 2)}</pre></details>}
          <label className="mcp-argument-field">Tool arguments JSON<textarea aria-label="MCP tool arguments JSON" value={toolArguments} onChange={event => setToolArguments(event.target.value)} /></label>
          {!threadId && <p className="extensions-muted">Open a conversation before calling an MCP tool.</p>}
          {toolResult !== null && <pre aria-label="MCP tool result">{JSON.stringify(toolResult, null, 2)}</pre>}
        </div>
      </section>
      <section className="mcp-editor">
        <div className="extensions-form-row"><label>Server name<input aria-label="Server name" value={serverName} onChange={event => setServerName(event.target.value)} placeholder="local-api" /></label>
          <label className="mcp-existing-select">Select existing<select aria-label="Select existing MCP server" value={servers[serverName] ? serverName : ''} onChange={event => chooseServer(event.target.value)}><option value="">New server…</option>{Object.keys(servers).map(name => <option value={name} key={name}>{name}</option>)}</select></label></div>
        <label>MCP server configuration JSON<textarea aria-label="MCP server configuration JSON" spellCheck={false} value={serverJson} onChange={event => setServerJson(event.target.value)} /></label>
        <p className="extensions-muted">Keep every field supported by Codex, including <code>command</code>, <code>url</code>, <code>env</code>, headers, auth, and timeout settings. {hiddenSecret} values are restored when saved unchanged.</p>
        <Button disabled={Boolean(busy)} onClick={() => void saveServer()}>Save MCP server</Button>
      </section>
      {serverStatuses.filter(status => !servers[status.name]).map(status => <article className="mcp-server-card runtime-only-server" key={status.name}>
        <div className="mcp-server-heading"><div className="plugin-summary"><strong>{status.name} (plugin provided)</strong><span>{status.runtimeStatus ?? 'Status unavailable'}{status.toolsError ? ` · ${status.toolsError}` : ''}</span></div>
          <Button variant="outline" disabled={Boolean(busy)} onClick={() => void run(`oauth:${status.name}`, async () => {
            const result = await startMcpOAuth(status.name, threadId)
            setOauthUrls(current => ({ ...current, [status.name]: result.authorizationUrl }))
            setMessage(`Authorization started for ${status.name}. Open the returned authorization URL to continue.`)
          })}>Sign in {status.name}</Button>
        </div>
        {oauthUrls[status.name] && <Button variant="outline" onClick={() => void run(`oauth-open:${status.name}`, () => openExternalUrl(oauthUrls[status.name]))}>Open authorization page</Button>}
      </article>)}
    </section>}
  </aside>
}

function findPlugin(result: PluginList, pluginId: string): SelectedPlugin | null {
  for (const marketplace of result.marketplaces) {
    const plugin = marketplace.plugins.find(item => item.id === pluginId)
    if (plugin) return { plugin, marketplace }
  }
  return null
}

export default ExtensionsPanel
