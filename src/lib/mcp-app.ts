import { invoke } from '@tauri-apps/api/core'
import type { JSONRPCMessage, Transport, TransportSendOptions } from '@modelcontextprotocol/client'
import { readMcpResource } from './extensions'

export type McpAppInvocation = {
  itemId: string
  server: string
  tool: string
  threadId: string
  resourceUri: string
  arguments: Record<string, unknown>
  result?: unknown
}

export type McpAppCsp = {
  connectDomains: string[]
  resourceDomains: string[]
  frameDomains: string[]
  baseUriDomains: string[]
}

export type McpAppResource = {
  html: string
  csp: McpAppCsp
  requestedPermissions?: Record<string, unknown>
}

export type McpSandbox = { url: string; token: string }

const supportedHostOrigins = new Set([
  'http://127.0.0.1:1420',
  'http://localhost:1420',
  'tauri://localhost',
  'http://tauri.localhost',
  'https://tauri.localhost',
])
const tokenPattern = /^[a-f\d]{64}$/i

function objectValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : null
}

export function extractMcpAppInvocation(item: unknown, threadId?: string): McpAppInvocation | null {
  const record = objectValue(item)
  if (!record || record.type !== 'mcpToolCall' || typeof record.id !== 'string' || !threadId) return null
  if (typeof record.server !== 'string' || !record.server || typeof record.tool !== 'string' || !record.tool) return null
  const ui = objectValue(record.mcpAppUi)
  const resourceUri = typeof ui?.resourceUri === 'string'
    ? ui.resourceUri
    : typeof record.mcpAppResourceUri === 'string'
      ? record.mcpAppResourceUri
      : ''
  if (!resourceUri.startsWith('ui://') || /[\u0000-\u0020\u007f]/.test(resourceUri)) return null
  const args = objectValue(record.arguments) ?? {}
  return {
    itemId: record.id,
    server: record.server,
    tool: record.tool,
    threadId,
    resourceUri,
    arguments: args,
    ...(Object.prototype.hasOwnProperty.call(record, 'result') ? { result: record.result } : {}),
  }
}

function normalizeDomains(value: unknown, label: keyof McpAppCsp): string[] {
  if (value === undefined) return []
  if (!Array.isArray(value) || !value.every(domain => typeof domain === 'string')) {
    throw new Error(`MCP App ${label} must be a list of HTTP(S) origins.`)
  }
  return [...new Set(value.map(domain => {
    if (/[\u0000-\u0020\u007f;]/.test(domain)) throw new Error(`MCP App ${label} contains an invalid CSP origin.`)
    try {
      const parsed = new URL(domain)
      const allowedSchemes = label === 'connectDomains' ? ['http:', 'https:', 'ws:', 'wss:'] : ['http:', 'https:']
      const wildcardHost = parsed.hostname.startsWith('*.')
      const hostname = wildcardHost ? parsed.hostname.slice(2) : parsed.hostname
      // URL parsing validates bracketed IPv6 literals; DNS label checks apply
      // to hostnames and wildcard suffixes only.
      const ipv6Literal = !wildcardHost && hostname.startsWith('[') && hostname.endsWith(']')
      const validDnsName = ipv6Literal || (hostname.length <= 253 && hostname.split('.').length >= (wildcardHost ? 2 : 1)
        && hostname.split('.').every(part => part.length > 0 && part.length <= 63 && /^[a-z\d](?:[a-z\d-]*[a-z\d])?$/i.test(part))
      )
      if (!allowedSchemes.includes(parsed.protocol) || parsed.origin !== domain || parsed.username || parsed.password
        || parsed.pathname !== '/' || parsed.search || parsed.hash || parsed.hostname.includes('*') !== wildcardHost
        || !validDnsName) {
        throw new Error()
      }
      return parsed.origin
    } catch {
      throw new Error(`MCP App ${label} contains an invalid HTTP(S) origin.`)
    }
  }))]
}

export function normalizeMcpAppCsp(value: unknown): McpAppCsp {
  const record = objectValue(value) ?? {}
  return {
    connectDomains: normalizeDomains(record.connectDomains, 'connectDomains'),
    resourceDomains: normalizeDomains(record.resourceDomains, 'resourceDomains'),
    frameDomains: normalizeDomains(record.frameDomains, 'frameDomains'),
    baseUriDomains: normalizeDomains(record.baseUriDomains, 'baseUriDomains'),
  }
}

function decodeHtmlBase64(value: string): string {
  try {
    const bytes = Uint8Array.from(atob(value), character => character.charCodeAt(0))
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes)
  } catch {
    throw new Error('MCP App resource contains invalid base64 HTML.')
  }
}

function isMcpAppHtmlMimeType(value: unknown): value is string {
  return typeof value === 'string' && /^text\/html\s*;\s*profile\s*=\s*"?mcp-app"?\s*$/i.test(value)
}

export async function readMcpAppResource(server: string, resourceUri: string, threadId: string): Promise<McpAppResource> {
  if (!resourceUri.startsWith('ui://')) throw new Error('MCP App resource URI must use the ui:// scheme.')
  const result = await readMcpResource(server, resourceUri, threadId)
  const contents = Array.isArray(result.contents) ? result.contents as Record<string, unknown>[] : []
  const entry = contents.find(content => content.uri === resourceUri && isMcpAppHtmlMimeType(content.mimeType))
  if (!entry) throw new Error('The MCP server did not return a matching MCP Apps HTML resource.')
  const html = typeof entry.text === 'string' ? entry.text : typeof entry.blob === 'string' ? decodeHtmlBase64(entry.blob) : ''
  if (!html.trim()) throw new Error('The MCP Apps HTML resource is empty.')
  const metadata = objectValue(objectValue(entry._meta)?.ui) ?? {}
  const permissions = objectValue(metadata.permissions) ?? undefined
  return {
    html,
    csp: normalizeMcpAppCsp(metadata.csp),
    ...(permissions ? { requestedPermissions: permissions } : {}),
  }
}

export async function createMcpSandbox(input: { html: string; csp: McpAppCsp; hostOrigin: string }): Promise<McpSandbox> {
  if (!supportedHostOrigins.has(input.hostOrigin)) throw new Error('MCP App host origin is not supported by the isolated sandbox.')
  if (!input.html.trim() || new TextEncoder().encode(input.html).byteLength > 5_000_000) {
    throw new Error('MCP App HTML must be non-empty and no larger than 5 MB.')
  }
  const csp = normalizeMcpAppCsp(input.csp)
  const result = await invoke<McpSandbox>('create_mcp_sandbox', { html: input.html, csp, hostOrigin: input.hostOrigin })
  if (!result || typeof result.token !== 'string' || !tokenPattern.test(result.token) || typeof result.url !== 'string') {
    throw new Error('The isolated MCP App sandbox returned an invalid address.')
  }
  try {
    const url = new URL(result.url)
    if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || !url.port || url.username || url.password || !url.pathname.endsWith(`/${result.token}`)) throw new Error()
  } catch {
    throw new Error('The isolated MCP App sandbox must use a tokenized loopback sandbox origin.')
  }
  return result
}

export async function destroyMcpSandbox(token: string): Promise<void> {
  if (!tokenPattern.test(token)) throw new Error('Invalid MCP App sandbox token.')
  await invoke('destroy_mcp_sandbox', { token })
}

/** A PostMessageTransport with an exact target origin and event source. */
export function createExactOriginTransport(target: Window, targetOrigin: string): Transport {
  let started = false
  const listener = (event: MessageEvent) => {
    if (!started || event.source !== target || event.origin !== targetOrigin) return
    try {
      if (JSON.stringify(event.data).length > 1_000_000) throw new Error('MCP App message exceeded the host size limit.')
    } catch (error) {
      transport.onerror?.(error instanceof Error ? error : new Error('Invalid MCP App message.'))
      return
    }
    transport.onmessage?.(event.data as JSONRPCMessage)
  }
  const transport: Transport = {
    onclose: undefined,
    onerror: undefined,
    onmessage: undefined,
    async start() {
      if (started) return
      started = true
      window.addEventListener('message', listener)
    },
    async send(message: JSONRPCMessage, _options?: TransportSendOptions) {
      if (!started) throw new Error('MCP App transport is not connected.')
      target.postMessage(message, targetOrigin)
    },
    async close() {
      if (!started) return
      started = false
      window.removeEventListener('message', listener)
      transport.onclose?.()
    },
  }
  return transport
}
