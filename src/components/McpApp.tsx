import { useEffect, useRef, useState } from 'react'
import type { AppBridge } from '@modelcontextprotocol/ext-apps/app-bridge'
import { isExternalUrl, openExternalUrl } from '../lib/external-url'
import { callMcpTool, listMcpServerStatus, readMcpResource } from '../lib/extensions'
import {
  createExactOriginTransport, createMcpSandbox, destroyMcpSandbox, readMcpAppResource,
  type McpAppInvocation, type McpAppResource, type McpSandbox,
} from '../lib/mcp-app'
import './McpApp.css'

type PendingRequest = { kind: 'tool'; name: string; args: unknown } | { kind: 'link'; url: string }
type PendingMessage = { text: string; preview: string }

export type McpAppProps = { invocation: McpAppInvocation; onUsePrompt?: (text: string) => void }

function allowedAppTool(tool: Record<string, any>): boolean {
  const meta = tool._meta && typeof tool._meta === 'object' ? tool._meta : {}
  const ui = meta.ui && typeof meta.ui === 'object' ? meta.ui : meta['ui']
  const visibility = ui?.visibility ?? meta['ui/visibility']
  return !Array.isArray(visibility) || visibility.includes('app')
}

function textFromContent(content: unknown): string {
  if (typeof content === 'string') return content
  if (!Array.isArray(content)) return ''
  return content.filter(part => part && typeof part === 'object' && (part as any).type === 'text' && typeof (part as any).text === 'string').map(part => (part as any).text as string).join('\n')
}

async function matchesResourceTemplate(uri: string, template: string): Promise<boolean> {
  if (/[\u0000-\u0020\u007f]/.test(uri)) return false
  try {
    const { UriTemplate } = await import('@modelcontextprotocol/client')
    return new UriTemplate(template).match(uri) !== null
  } catch { return false }
}

export function McpApp({ invocation, onUsePrompt }: McpAppProps) {
  const frameRef = useRef<HTMLIFrameElement>(null)
  const bridgeRef = useRef<AppBridge | null>(null)
  const initializedRef = useRef(false)
  const invocationRef = useRef(invocation)
  invocationRef.current = invocation
  const pendingResolverRef = useRef<((allowed: boolean) => void) | null>(null)
  const [resource, setResource] = useState<McpAppResource | null>(null)
  const [sandbox, setSandbox] = useState<McpSandbox | null>(null)
  const [availableTools, setAvailableTools] = useState<Record<string, Record<string, any>>>({})
  const [availableResources, setAvailableResources] = useState<Record<string, any>>({})
  const [availableResourceTemplates, setAvailableResourceTemplates] = useState<Record<string, any>>({})
  const [pending, setPending] = useState<PendingRequest | null>(null)
  const [message, setMessage] = useState<PendingMessage | null>(null)
  const [busy, setBusy] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)
  const toolRef = useRef(availableTools)
  const resourceListRef = useRef(availableResources)
  const resourceTemplateRef = useRef(availableResourceTemplates)
  toolRef.current = availableTools
  resourceListRef.current = availableResources
  resourceTemplateRef.current = availableResourceTemplates

  useEffect(() => {
    let cancelled = false
    let created: McpSandbox | null = null
    setBusy(true); setError(null); setSandbox(null)
    Promise.all([
      readMcpAppResource(invocation.server, invocation.resourceUri, invocation.threadId),
      listMcpServerStatus(invocation.threadId),
    ]).then(async ([appResource, statuses]) => {
      if (cancelled) return
      const status = statuses.find(server => server.name === invocation.server)
      if (!status) throw new Error(`MCP server “${invocation.server}” is not available in this conversation.`)
      const tools: Record<string, Record<string, any>> = Object.fromEntries(Object.entries(status.tools ?? {}).filter(([, tool]) => allowedAppTool(tool as Record<string, any>))) as Record<string, Record<string, any>>
      const resources = Object.fromEntries((status.resources ?? []).filter((entry: any) => typeof entry?.uri === 'string').map((entry: any) => [entry.uri, entry]))
      const resourceTemplates = Object.fromEntries((status.resourceTemplates ?? []).filter((entry: any) => typeof entry?.uriTemplate === 'string').map((entry: any) => [entry.uriTemplate, entry]))
      const hostOrigin = window.location.origin
      const address = await createMcpSandbox({ html: appResource.html, csp: appResource.csp, hostOrigin })
      if (cancelled) { await destroyMcpSandbox(address.token); return }
      setResource(appResource); setAvailableTools(tools); setAvailableResources(resources); setAvailableResourceTemplates(resourceTemplates)
      created = address; setSandbox(address)
    }).catch(cause => { if (!cancelled) setError(cause instanceof Error ? cause.message : String(cause)) })
      .finally(() => { if (!cancelled) setBusy(false) })
    return () => {
      cancelled = true
      if (created) void destroyMcpSandbox(created.token).catch(() => undefined)
    }
  }, [invocation.itemId, invocation.server, invocation.resourceUri, invocation.threadId])

  useEffect(() => {
    if (!sandbox || !resource || !frameRef.current) return
    let disposed = false
    const frame = frameRef.current
    let bridge: AppBridge | null = null
    const settle = (allowed: boolean) => { pendingResolverRef.current?.(allowed); pendingResolverRef.current = null; setPending(null) }
    const requestConsent = (next: PendingRequest) => new Promise<boolean>(resolve => {
      if (pendingResolverRef.current) { resolve(false); return }
      pendingResolverRef.current = resolve
      setPending(next)
    })

    void import('@modelcontextprotocol/ext-apps/app-bridge').then(({ AppBridge }) => {
      if (disposed) return
      try {
      bridge = new AppBridge(null, { name: 'Brr', version: '0.1.0' }, {
        openLinks: {}, serverTools: {}, serverResources: {}, message: { text: {} },
        sandbox: { csp: resource.csp, permissions: {} },
      }, { hostContext: { platform: 'desktop' } })
      bridgeRef.current = bridge
      bridge.oncalltool = async ({ name, arguments: args }) => {
        const knownTool = toolRef.current[name]
        if (!knownTool) return { isError: true, content: [{ type: 'text', text: 'This tool is not available to this MCP App.' }] }
        const allowed = await requestConsent({ kind: 'tool', name, args: args ?? {} })
        if (!allowed || disposed) return { isError: true, content: [{ type: 'text', text: 'Tool call was not approved.' }] }
        try { return await callMcpTool(invocation.server, name, args ?? {}, invocation.threadId) as any }
        catch (cause) { return { isError: true, content: [{ type: 'text', text: cause instanceof Error ? cause.message : String(cause) }] } }
      }
      bridge.onlistresources = async () => ({ resources: Object.values(resourceListRef.current) as any })
      bridge.onlistresourcetemplates = async () => ({ resourceTemplates: Object.values(availableResourceTemplates) as any })
      bridge.onreadresource = async ({ uri }) => {
        const advertised = uri === invocation.resourceUri || Boolean(resourceListRef.current[uri])
          || await Promise.all(Object.keys(resourceTemplateRef.current).map(template => matchesResourceTemplate(uri, template))).then(matches => matches.some(Boolean))
        if (!advertised) throw new Error('This resource is not available to this MCP App.')
        return await readMcpResource(invocation.server, uri, invocation.threadId) as any
      }
      bridge.onopenlink = async ({ url }) => {
        if (!isExternalUrl(url)) return { isError: true }
        const allowed = await requestConsent({ kind: 'link', url })
        if (!allowed || disposed) return { isError: true }
        try { await openExternalUrl(url); return {} } catch (cause) { setActionError(cause instanceof Error ? cause.message : String(cause)); return { isError: true } }
      }
      bridge.onmessage = async ({ content }) => {
        const text = textFromContent(content)
        setMessage({ text, preview: JSON.stringify(content, null, 2) })
        return {}
      }
      bridge.onsandboxready = () => {
        void bridge?.sendSandboxResourceReady({
          html: resource.html,
          csp: resource.csp,
          sandbox: 'allow-scripts allow-same-origin allow-forms allow-downloads',
          permissions: {},
        }).catch(cause => setActionError(cause instanceof Error ? cause.message : String(cause)))
      }
      bridge.oninitialized = () => {
        initializedRef.current = true
        const current = invocationRef.current
        void bridge?.sendToolInput({ arguments: current.arguments as any })
        if (current.result !== undefined) void bridge?.sendToolResult(current.result as any)
      }
      const transport = createExactOriginTransport(frame.contentWindow!, new URL(sandbox.url).origin)
      void bridge.connect(transport).catch(cause => { if (!disposed) setActionError(cause instanceof Error ? cause.message : String(cause)) })
      frame.src = sandbox.url
      } catch (cause) {
        setActionError(cause instanceof Error ? cause.message : String(cause))
      }
    }).catch(cause => { if (!disposed) setActionError(cause instanceof Error ? cause.message : String(cause)) })
    return () => {
      disposed = true
      initializedRef.current = false
      if (pendingResolverRef.current) { pendingResolverRef.current(false); pendingResolverRef.current = null }
      setPending(null)
      if (bridge) {
        void (async () => {
          try { await bridge?.teardownResource({}, { timeout: 500 }) } catch { /* app may not have initialized */ }
          finally { try { await bridge?.close() } catch { /* transport may already be closed */ } }
        })()
      }
      bridgeRef.current = null
    }
  }, [sandbox, resource, availableResourceTemplates, invocation.server, invocation.threadId, invocation.resourceUri])

  useEffect(() => {
    if (!initializedRef.current || invocation.result === undefined) return
    void bridgeRef.current?.sendToolResult(invocation.result as any).catch(cause => setActionError(cause instanceof Error ? cause.message : String(cause)))
  }, [invocation.result])

  const approvePending = async (allowed: boolean) => {
    if (!pending) return
    pendingResolverRef.current?.(allowed)
    pendingResolverRef.current = null
    setPending(null)
  }

  return <section className="mcp-app-host" aria-label={`MCP App from ${invocation.server}`}>
    <header><strong>{invocation.server} app</strong><span>Sandboxed MCP App</span></header>
    {busy && <p role="status">Loading MCP App…</p>}
    {error && <p className="mcp-app-error" role="alert">MCP App unavailable: {error}</p>}
    {actionError && <p className="mcp-app-error" role="alert">{actionError}</p>}
    {Object.keys(resource?.requestedPermissions ?? {}).length > 0 && <p role="note">This app requested {Object.keys(resource?.requestedPermissions ?? {}).join(', ')}. Brr does not grant device permissions; the app runs without them.</p>}
    {!error && <iframe ref={frameRef} title={`MCP App: ${invocation.tool}`} sandbox="allow-scripts allow-same-origin allow-forms allow-downloads" aria-label={`Interactive app from ${invocation.server}`} />}
    {pending?.kind === 'tool' && <div className="mcp-app-consent" role="dialog" aria-label="Approve MCP App tool call">
      <strong>Allow this app to call {pending.name}?</strong><pre>{JSON.stringify(pending.args, null, 2)}</pre>
      <button type="button" onClick={() => void approvePending(false)}>Deny</button><button type="button" onClick={() => void approvePending(true)}>Allow once</button>
    </div>}
    {pending?.kind === 'link' && <div className="mcp-app-consent" role="dialog" aria-label="Approve external link">
      <strong>Open link in your browser?</strong><code>{pending.url}</code>
      <button type="button" onClick={() => void approvePending(false)}>Cancel</button><button type="button" onClick={() => void approvePending(true)}>Open link</button>
    </div>}
    {message && <details className="mcp-app-message"><summary>App requested to send a message</summary><pre>{message.preview}</pre>
      {message.text && <button type="button" onClick={() => onUsePrompt?.(message.text)}>Review and insert text</button>}
      {!message.text && <p>This message has no text content that can be inserted.</p>}
    </details>}
  </section>
}

export default McpApp
