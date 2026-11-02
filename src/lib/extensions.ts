import { invoke } from '@tauri-apps/api/core'

const rpc = <T = any>(method: string, params: Record<string, unknown> = {}) =>
  invoke<T>('engine_request', { method, params })

export type PluginSummary = {
  id: string
  name: string
  installed: boolean
  enabled: boolean
  source: { type: string; path?: string; url?: string; refName?: string }
  installPolicy?: string
  authPolicy?: string
  availability?: string
  description?: string | null
  [key: string]: unknown
}

export type PluginMarketplace = {
  name: string
  path: string | null
  interface?: { displayName?: string | null } | null
  plugins: PluginSummary[]
}

export type PluginList = {
  marketplaces: PluginMarketplace[]
  marketplaceLoadErrors: { marketplacePath: string; message: string }[]
  featuredPluginIds?: string[]
}

export type PluginSearchScope = 'global' | 'workspace' | 'personal'
export type PluginSearchResult = { plugin: PluginSummary; marketplaceName: string; marketplacePath: string | null }

export type McpServerStatus = {
  name: string
  runtimeStatus: string | null
  tools: Record<string, unknown>
  toolsError?: string | null
  resources: unknown[]
  resourceTemplates?: unknown[]
  authStatus?: string
  serverInfo?: { name?: string; version?: string } | null
}

export type HookMetadata = {
  key: string
  eventName: string
  handlerType: string
  command?: string
  server?: string
  tool?: string
  matcher?: string | null
  timeoutSec: number
  sourcePath: string
  source: string
  pluginId?: string | null
  enabled: boolean
  isManaged: boolean
  currentHash: string
  trustStatus: string
}

const marketplaceKinds = ['local', 'vertical', 'workspace-directory', 'shared-with-me', 'created-by-me-remote']

export const listPlugins = (cwd?: string, forceRefetch = false) => rpc<PluginList>('plugin/list', {
  ...(cwd ? { cwds: [cwd] } : {}),
  marketplaceKinds,
  forceRefetch,
})

export async function searchPlugins(searchTerm: string, scope: PluginSearchScope, cwd?: string): Promise<PluginSearchResult[]> {
  const all: PluginSearchResult[] = []
  let cursor: string | undefined
  do {
    const page = await rpc<{ data: PluginSearchResult[]; nextCursor: string | null }>('plugin/search', {
      searchTerm,
      scope,
      ...(cwd ? { cwds: [cwd] } : {}),
      limit: 100,
      ...(cursor ? { cursor } : {}),
    })
    all.push(...page.data)
    cursor = page.nextCursor ?? undefined
  } while (cursor)
  return all
}

export type RefreshedApp = { id: string; name?: string; isAccessible?: boolean; isEnabled?: boolean }
export type InstalledApp = { id: string; runtimeName?: string | null; enabled: boolean; callable: boolean }

export async function refreshAppAuthStatus(): Promise<{ apps: RefreshedApp[]; installedApps: InstalledApp[] }> {
  const apps: RefreshedApp[] = []
  let cursor: string | undefined
  do {
    const page = await rpc<{ data: RefreshedApp[]; nextCursor: string | null }>('app/list', {
      limit: 100,
      forceRefetch: true,
      ...(cursor ? { cursor } : {}),
    })
    apps.push(...page.data)
    cursor = page.nextCursor ?? undefined
  } while (cursor)
  const installed = await rpc<{ apps: InstalledApp[] }>('app/installed', { forceRefresh: true })
  await rpc('config/mcpServer/reload')
  return { apps, installedApps: installed.apps }
}

export const readPlugin = (selector: { marketplacePath?: string | null; remoteMarketplaceName?: string | null; pluginName: string }) =>
  rpc<{ plugin: Record<string, any> }>('plugin/read', selector)

export const addMarketplace = (source: string, refName?: string, sparsePaths?: string[]) =>
  rpc<{ marketplaceName: string; installedRoot: string; alreadyAdded: boolean }>('marketplace/add', {
    source,
    ...(refName ? { refName } : {}),
    ...(sparsePaths?.length ? { sparsePaths } : {}),
  })

export const upgradeMarketplaces = (marketplaceName?: string) => rpc<{
  selectedMarketplaces: string[]; upgradedRoots: string[]; errors: { marketplaceName: string; message: string }[]
}>('marketplace/upgrade', marketplaceName ? { marketplaceName } : {})

export const removeMarketplace = (marketplaceName: string) => rpc('marketplace/remove', { marketplaceName })

export const installPlugin = (selector: {
  marketplacePath?: string | null
  remoteMarketplaceName?: string | null
  pluginName: string
}) => rpc<{ authPolicy: string; appsNeedingAuth: { id?: string; name?: string; installUrl?: string | null; [key: string]: unknown }[] }>('plugin/install', selector)

export const uninstallPlugin = (pluginId: string) => rpc('plugin/uninstall', { pluginId })

export type PluginMcpCredential =
  | { kind: 'token'; token: string }
  | { kind: 'basic'; username: string; password: string }
  | { kind: 'oauth_client_secret'; secret: string }
  | { kind: 'oauth_private_key_jwt'; privateKey: string; algorithm: string; keyId?: string | null }
export async function writePluginMcpCredential(pluginId: string, serverName: string, credential: PluginMcpCredential | null) {
  await rpc('plugin/mcpCredential/write', { pluginId, serverName, credential })
  await rpc('config/mcpServer/reload')
}

export const setPluginEnabled = (pluginId: string, enabled: boolean) => rpc('config/value/write', {
  keyPath: `plugins.${tomlKey(pluginId)}.enabled`,
  value: enabled,
  mergeStrategy: 'replace',
})

function tomlKey(value: string) {
  return `"${value.replaceAll('\\', '\\\\').replaceAll('"', '\\"')}"`
}

export const readEffectiveConfig = (cwd?: string) => rpc<{ config: Record<string, any>; origins?: Record<string, unknown> }>('config/read', {
  includeLayers: false,
  ...(cwd ? { cwd } : {}),
})

export const writeConfigValue = (keyPath: string, value: unknown) => rpc('config/value/write', {
  keyPath,
  value,
  mergeStrategy: 'replace',
})

export async function saveMcpServer(name: string, value: Record<string, unknown>) {
  await writeConfigValue(`mcp_servers.${tomlKey(name)}`, value)
  return rpc('config/mcpServer/reload')
}

export async function removeMcpServer(name: string) {
  await writeConfigValue(`mcp_servers.${tomlKey(name)}`, null)
  return rpc('config/mcpServer/reload')
}

export async function setMcpServerEnabled(name: string, enabled: boolean) {
  await writeConfigValue(`mcp_servers.${tomlKey(name)}.enabled`, enabled)
  return rpc('config/mcpServer/reload')
}

export async function listMcpServerStatus(threadId?: string): Promise<McpServerStatus[]> {
  const all: McpServerStatus[] = []
  let cursor: string | undefined
  do {
    const page = await rpc<{ data: McpServerStatus[]; nextCursor: string | null }>('mcpServerStatus/list', {
      limit: 100,
      detail: 'full',
      ...(threadId ? { threadId } : {}),
      ...(cursor ? { cursor } : {}),
    })
    all.push(...page.data)
    cursor = page.nextCursor ?? undefined
  } while (cursor)
  return all
}

export type McpPromptSummary = { name: string; title?: string; description?: string; arguments?: { name: string; description?: string; required?: boolean }[]; [key: string]: unknown }

export async function listMcpPrompts(server: string, threadId?: string): Promise<McpPromptSummary[]> {
  const all: McpPromptSummary[] = []
  let cursor: string | undefined
  do {
    const page = await rpc<{ prompts: McpPromptSummary[]; nextCursor: string | null }>('mcpServer/prompts/list', {
      server,
      ...(threadId ? { threadId } : {}),
      ...(cursor ? { cursor } : {}),
    })
    all.push(...page.prompts)
    cursor = page.nextCursor ?? undefined
  } while (cursor)
  return all
}

export type McpPromptPayload = { description?: string; messages: { role: string; content: unknown }[]; [key: string]: unknown }

export const getMcpPrompt = (server: string, name: string, args?: Record<string, string>, threadId?: string) =>
  rpc<McpPromptPayload>('mcpServer/prompt/get', {
    server,
    name,
    ...(args && Object.keys(args).length ? { arguments: args } : {}),
    ...(threadId ? { threadId } : {}),
  })

export const completeMcpArgument = (
  server: string,
  ref: Record<string, unknown>,
  argument: { name: string; value: string },
  context?: Record<string, unknown>,
  threadId?: string,
) => rpc<{ completion: { values: string[]; total?: number; hasMore?: boolean; [key: string]: unknown } }>('mcpServer/completion/complete', {
  server,
  ref,
  argument,
  ...(context ? { context } : {}),
  ...(threadId ? { threadId } : {}),
})

export const readMcpResource = (server: string, uri: string, threadId?: string) =>
  rpc<{ contents: unknown[]; originCallId?: string | null; [key: string]: unknown }>('mcpServer/resource/read', {
    server,
    uri,
    ...(threadId ? { threadId } : {}),
  })

export const callMcpTool = (server: string, tool: string, argumentsValue: unknown, threadId?: string) => {
  if (!threadId) throw new Error('Open an active conversation before calling an MCP tool.')
  return rpc<{ content: unknown[]; structuredContent?: unknown; isError?: boolean; [key: string]: unknown }>('mcpServer/tool/call', {
    threadId,
    server,
    tool,
    ...(argumentsValue !== undefined ? { arguments: argumentsValue } : {}),
  })
}

export const startMcpOAuth = (name: string, threadId?: string) => rpc<{ authorizationUrl: string; loginId?: string | null }>('mcpServer/oauth/login', {
  name,
  ...(threadId ? { threadId } : {}),
})

export const listHooks = (cwd?: string) => rpc<{ data: { cwd: string; hooks: HookMetadata[]; warnings: string[]; errors: { path: string; message: string }[] }[] }>('hooks/list', cwd ? { cwds: [cwd] } : {})

export const trustHook = (key: string, currentHash: string) => rpc('config/batchWrite', {
  edits: [{
    keyPath: `hooks.state.${JSON.stringify(key)}.trusted_hash`,
    value: currentHash,
    mergeStrategy: 'replace',
  }],
  reloadUserConfig: true,
})
