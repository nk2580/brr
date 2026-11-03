import React from 'react'
import { invoke } from '@tauri-apps/api/core'
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react'
import { intelligentUiClient, interpolateUiText, validateUiDocument, type JsonValue, type UiActionReview, type UiComponent, type UiDocument, type UiIdentity, type UiLoaded, type UiState } from '../lib/intelligent-ui'
import './IntelligentUi.css'

export type UiAppMountRequest = {
  threadId: string; callId: string; documentHash: string; document: UiDocument; state: UiState
  onStateChange(state: UiState): Promise<void>; onAction(actionId: string): Promise<void>; onResize(height: number): void
}
export type IntelligentUiProps = {
  identity: UiIdentity
  document?: UiDocument
  preview?: boolean
  active?: boolean
  onExpand?(): void
  onReview?(review: UiActionReview): void
  onMountApp?(request: UiAppMountRequest, container: HTMLElement): void | (() => void)
}

function useController(identity: UiIdentity) {
  const controller = useMemo(() => intelligentUiClient(identity), [identity.threadId, identity.callId, identity.documentHash])
  const loaded = useSyncExternalStore(controller.subscribe, controller.snapshot, controller.snapshot)
  return { controller, loaded }
}
const scalar = (v: unknown) => v === null || ['string', 'number', 'boolean'].includes(typeof v)
function actionById(document: UiDocument, id?: string) { return id ? document.actions.find(action => action.id === id) : undefined }

export class IntelligentUiErrorBoundary extends React.Component<{ children: React.ReactNode; alternative: string }, { failed: boolean }> {
  state = { failed: false }
  static getDerivedStateFromError() { return { failed: true } }
  render() {
    if (this.state.failed) return <section className="intelligent-ui" aria-label="Interactive content error"><p role="alert">The interactive view could not be displayed. Use its text alternative:</p><div className="intelligent-ui-text-panel">{this.props.alternative || 'Interactive response text is unavailable.'}</div></section>
    return this.props.children
  }
}
export function IntelligentUi(props: IntelligentUiProps) {
  const controller = useMemo(() => intelligentUiClient(props.identity), [props.identity.threadId, props.identity.callId, props.identity.documentHash])
  const loaded = useSyncExternalStore(controller.subscribe, controller.snapshot, controller.snapshot)
  const text = props.preview ? props.document?.text ?? '' : loaded?.document.text ?? props.document?.text ?? ''
  const state = props.preview ? props.document?.initialState ?? {} : loaded?.state ?? props.document?.initialState ?? {}
  const alternative = text ? interpolateUiText(text, state) : 'Interactive response text is unavailable.'
  return <IntelligentUiErrorBoundary key={`${props.identity.threadId}/${props.identity.callId}/${props.identity.documentHash ?? ''}`} alternative={alternative}><IntelligentUiContent {...props} /></IntelligentUiErrorBoundary>
}

function IntelligentUiContent({ identity, document: previewDocument, preview = false, active = true, onExpand, onReview, onMountApp }: IntelligentUiProps) {
  const { controller, loaded } = useController(identity)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [showText, setShowText] = useState(false)
  const appRef = useRef<HTMLDivElement>(null)
  const [appError, setAppError] = useState<string | null>(null)
  const [appClaimed, setAppClaimed] = useState(false)
  const [appEnabled, setAppEnabled] = useState(true)
  const [inViewport, setInViewport] = useState(() => typeof IntersectionObserver === 'undefined')
  const onReviewRef = useRef(onReview)
  onReviewRef.current = onReview
  useEffect(() => { setAppEnabled(true); setAppClaimed(false) }, [identity.threadId, identity.callId, identity.documentHash])
  const previewResult = useMemo(() => preview && previewDocument ? validateUiDocument(previewDocument) : null, [preview, previewDocument])
  const authoritative = preview ? null : loaded
  const view = preview ? (previewResult?.ok ? { document: previewResult.document, state: previewResult.document.initialState } : null) : authoritative
  const readOnly = preview || !active || !authoritative

  useEffect(() => {
    if (preview || loaded || !active) return
    let alive = true
    void controller.load().catch(cause => { if (alive) setError(cause instanceof Error ? cause.message : String(cause)) })
    return () => { alive = false }
  }, [controller, preview, loaded, active])

  const update = useCallback(async (key: string, value: JsonValue) => {
    if (!authoritative || readOnly) return
    setBusy(true); setError(null)
    try { await controller.setValue(key, value) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }, [authoritative, controller, readOnly])
  const runAction = useCallback(async (actionId: string) => {
    if (readOnly) throw new Error('This UI action is not available right now.')
    setBusy(true); setError(null)
    try { const review = await controller.prepareAction(actionId); onReviewRef.current?.(review); return review }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); throw cause }
    finally { setBusy(false) }
  }, [controller, readOnly])


  useEffect(() => {
    const root = appRef.current?.parentElement?.parentElement
    if (!root || authoritative?.document.kind !== 'app' || !active || typeof IntersectionObserver === 'undefined') { setInViewport(typeof IntersectionObserver === 'undefined'); return }
    const observer = new IntersectionObserver(entries => setInViewport(entries.some(entry => entry.isIntersecting && entry.intersectionRatio > 0)), { threshold: 0.01 })
    observer.observe(root)
    return () => observer.disconnect()
  }, [active, authoritative?.document.kind, authoritative?.documentHash])

  useEffect(() => {
    if (!authoritative || authoritative.document.kind !== 'app' || !active || !appEnabled || !inViewport || !appRef.current) return
    const owner = Symbol('intelligent-ui-app')
    if (!controller.claimApp(owner)) { setAppClaimed(false); return }
    setAppClaimed(true); setAppError(null)
    let cancelled = false
    let dispose: void | (() => void)
    let iframe: HTMLIFrameElement | null = null
    let sandboxToken: string | null = null
    const destroy = () => { if (sandboxToken) void invoke('intelligent_ui_request', { method: 'sandbox.destroy', params: { token: sandboxToken } }).catch(() => undefined); sandboxToken = null }
    const mount = async () => {
      try {
        if (onMountApp) {
          dispose = onMountApp({
            threadId: identity.threadId, callId: identity.callId, documentHash: authoritative.documentHash,
            document: authoritative.document, state: authoritative.state,
            onStateChange: state => controller.setState(state), onAction: async actionId => { await runAction(actionId) },
            onResize: height => { if (appRef.current) appRef.current.style.height = `${Math.max(120, Math.min(1200, Math.round(height)))}px` },
          }, appRef.current!)
          return
        }
        const result = await invoke<{ url: string; token: string }>('intelligent_ui_request', { method: 'sandbox.create', params: { threadId: identity.threadId, callId: identity.callId, documentHash: authoritative.documentHash, hostOrigin: window.location.origin } })
        sandboxToken = result.token
        if (cancelled) { destroy(); return }
        const address = new URL(result.url)
        const frame = window.document.createElement('iframe')
        iframe = frame
        frame.title = `${authoritative.document.title} interactive app`
        frame.setAttribute('sandbox', 'allow-scripts allow-same-origin')
        frame.referrerPolicy = 'no-referrer'
        frame.src = address.href
        frame.style.width = '100%'; frame.style.height = '360px'; frame.style.border = '0'; frame.style.display = 'block'
        const onMessage = (event: MessageEvent) => {
          if (!iframe || event.source !== iframe.contentWindow || event.origin !== address.origin) return
          const message = event.data
          if (!message || typeof message !== 'object' || message.channel !== 'brr-ui' || message.token !== result.token || typeof message.id !== 'string') return
          const reply = (ok: boolean, value?: unknown, messageError?: string) => iframe?.contentWindow?.postMessage({ channel: 'brr-ui-result', token: result.token, id: message.id, ok, ...(ok ? { value } : { error: messageError ?? 'Request failed' }) }, event.origin)
          void (async () => {
            try {
              switch (message.method) {
                case 'getState': reply(true, controller.snapshot()?.state ?? authoritative.state); break
                case 'setState':
                  if (!message.args || typeof message.args !== 'object' || Array.isArray(message.args)) throw new Error('App state must be a JSON object.')
                  await controller.setState(message.args as UiState)
                  reply(true, controller.snapshot()?.state ?? message.args)
                  break
                case 'requestAction':
                  if (typeof message.args !== 'string') throw new Error('Invalid app action request.')
                  await runAction(message.args)
                  reply(true, null)
                  break
                case 'resize':
                  if (typeof message.args !== 'number' || !Number.isFinite(message.args)) throw new Error('Invalid app size.')
                  frame.style.height = `${Math.max(120, Math.min(1200, Math.round(message.args)))}px`
                  reply(true, null)
                  break
                default: throw new Error('Unsupported app operation.')
              }
            } catch (cause) { reply(false, undefined, cause instanceof Error ? cause.message : String(cause)) }
          })()
        }
        window.addEventListener('message', onMessage)
        appRef.current?.append(frame)
        let sentRevision = -1
        const pushState = () => {
          const saved = controller.snapshot()
          if (!saved || saved.revision <= sentRevision || !frame.contentWindow) return
          sentRevision = saved.revision
          frame.contentWindow.postMessage({ channel: 'brr-ui-state', token: result.token, state: saved.state, revision: saved.revision }, address.origin)
        }
        const unsubscribe = controller.subscribe(pushState)
        frame.onload = () => { sentRevision = -1; pushState() }
        dispose = () => { unsubscribe(); frame.onload = null; window.removeEventListener('message', onMessage); frame.remove(); destroy() }
      } catch (cause) { if (!cancelled) setAppError(cause instanceof Error ? cause.message : String(cause)); destroy() }
    }
    void mount()
    return () => { cancelled = true; try { dispose?.() } finally { if (!dispose) destroy(); controller.releaseApp(owner) } }
  }, [active, appEnabled, authoritative?.documentHash, authoritative?.document.kind, controller, identity.callId, identity.threadId, inViewport, onMountApp, runAction])

  const reset = async () => { setBusy(true); setError(null); try { await controller.reset() } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) } finally { setBusy(false) } }
  if (preview && previewDocument && !previewResult?.ok) return <section className="intelligent-ui" aria-label="Intelligent UI preview"><p role="alert">{previewResult?.error ?? 'Invalid UI document.'}</p><p>{previewDocument.text}</p></section>
  if (!view) return <section className="intelligent-ui" aria-label="Intelligent UI"><p role={error ? 'alert' : undefined}>{error ?? (active ? 'Loading interactive content…' : 'Interactive content is inactive.')}</p></section>
  const { document, state } = view
  const textAlt = interpolateUiText(document.text, state)
  const dispatch = (id?: string) => { if (id && actionById(document, id)) void runAction(id).catch(() => undefined) }
  return <section className={`intelligent-ui${preview ? ' intelligent-ui-preview' : ''}`} aria-label={document.title}>
    <header className="intelligent-ui-header"><div><strong>{document.title}</strong>{preview && <span className="intelligent-ui-preview-label">Preview</span>}</div><nav aria-label="UI controls"><button type="button" onClick={() => setShowText(v => !v)} aria-expanded={showText}>{showText ? 'Hide text' : 'View text'}</button>{!preview && <>{document.kind === 'app' && <button type="button" onClick={() => { if (appEnabled) { setAppEnabled(false); setAppClaimed(false) } else setAppEnabled(true) }} disabled={!active}>{appEnabled && appClaimed ? 'Stop app' : 'Run app'}</button>}<button type="button" onClick={() => void reset()} disabled={!authoritative || busy}>Reset</button></>}{onExpand && <button type="button" onClick={onExpand}>Expand</button>}</nav></header>
    <p className={showText ? 'intelligent-ui-text-alt' : 'intelligent-ui-sr-only'}>{textAlt}</p>
    {showText && <div className="intelligent-ui-text-panel" aria-label="Text alternative">{textAlt}</div>}
    {document.kind === 'native' && <div className="intelligent-ui-content">{document.components?.map(c => <Component key={c.id} component={c} document={document} state={state} disabled={readOnly} actionsDisabled={readOnly || busy} onChange={update} onAction={dispatch} />)}</div>}
    {document.kind === 'app' && <div className="intelligent-ui-app-wrap">{preview ? <p>Interactive app preview is disabled. {textAlt}</p> : <><div ref={appRef} className="intelligent-ui-app" aria-label={`${document.title} application`} />{!appClaimed && active && inViewport && <p>{appEnabled ? 'Interactive app is active in another view.' : 'Interactive app is stopped.'} {textAlt}</p>}{appError && <p role="alert">{appError}. {textAlt}</p>}</>}</div>}
    {error && <p className="intelligent-ui-error" role="alert">{error}</p>}
  </section>
}

function Component({ component: c, document, state, disabled, actionsDisabled, onChange, onAction }: { component: UiComponent; document: UiDocument; state: UiState; disabled: boolean; actionsDisabled: boolean; onChange(key: string, value: JsonValue): void; onAction(id?: string): void }) {
  if (c.visibleWhen && state[c.visibleWhen.key] !== c.visibleWhen.equals) return null
  const tx = (text: string) => interpolateUiText(text, state)
  const nested = (children: UiComponent[]) => children.map(child => <Component key={child.id} component={child} document={document} state={state} disabled={disabled} actionsDisabled={actionsDisabled} onChange={onChange} onAction={onAction} />)
  switch (c.type) {
    case 'text': return <p className="intelligent-ui-paragraph">{tx(c.text)}</p>
    case 'group': return <div className="intelligent-ui-group">{nested(c.children)}</div>
    case 'form': return <form className="intelligent-ui-form" aria-label={c.label} onSubmit={event => { event.preventDefault(); onAction(c.actionId) }}>{c.label && <h3>{tx(c.label)}</h3>}{nested(c.children)}{c.actionId && <button type="submit" disabled={actionsDisabled}>{actionById(document, c.actionId)?.label ?? 'Continue'}</button>}</form>
    case 'comparison': return <div className="intelligent-ui-comparison" role="group" aria-label="Comparison">{c.columns.map(col => <section key={col.label}><h3>{tx(col.label)}</h3><p>{tx(col.text)}</p></section>)}</div>
    case 'table': return <div className="intelligent-ui-table-wrap"><table><thead><tr>{c.columns.map((col, i) => <th key={`${i}-${col}`} scope="col">{tx(col)}</th>)}</tr></thead><tbody>{c.rows.map((row, i) => <tr key={i}>{row.map((cell, j) => <td key={j}>{cell === null ? '—' : tx(String(cell))}</td>)}</tr>)}</tbody></table></div>
    case 'timeline': return <ol className="intelligent-ui-timeline">{c.entries.map((entry, i) => <li key={i}><strong>{tx(entry.label)}</strong><span>{tx(entry.text)}</span></li>)}</ol>
    case 'chart': return <Chart component={c} />
    case 'diagram': return <Diagram component={c} />
    case 'tabs': {
      const selected = typeof state[c.id] === 'string' && c.tabs.some(tab => tab.id === state[c.id]) ? state[c.id] as string : c.tabs[0]?.id
      const active = c.tabs.find(tab => tab.id === selected)!
      return <div className="intelligent-ui-tabs"><div role="tablist" aria-label="Sections">{c.tabs.map(tab => <button type="button" role="tab" key={tab.id} id={`${c.id}-${tab.id}-tab`} aria-selected={selected === tab.id} aria-controls={`${c.id}-panel`} disabled={disabled} onClick={() => onChange(c.id, tab.id)}>{tx(tab.label)}</button>)}</div><div role="tabpanel" id={`${c.id}-panel`} aria-labelledby={`${c.id}-${active.id}-tab`}>{nested(active.children)}</div></div>
    }
    case 'select': return <label className="intelligent-ui-control">{tx(c.label)}<select value={typeof state[c.stateKey] === 'string' ? state[c.stateKey] as string : ''} disabled={disabled} onChange={e => onChange(c.stateKey, e.currentTarget.value)}>{c.options.map(o => <option key={o.value} value={o.value}>{tx(o.label)}</option>)}</select></label>
    case 'slider': return <label className="intelligent-ui-control">{tx(c.label)}<span>{String(typeof state[c.stateKey] === 'number' ? state[c.stateKey] : c.min)}</span><input type="range" min={c.min} max={c.max} step={c.step ?? 1} value={typeof state[c.stateKey] === 'number' ? state[c.stateKey] as number : c.min} disabled={disabled} onChange={e => onChange(c.stateKey, Number(e.currentTarget.value))} /></label>
    case 'input': return <label className="intelligent-ui-control">{tx(c.label)}<input type={c.inputType ?? 'text'} value={scalar(state[c.stateKey]) && state[c.stateKey] !== null ? String(state[c.stateKey]) : ''} disabled={disabled} onChange={e => { if (c.inputType === 'number') { if (e.currentTarget.value !== '') onChange(c.stateKey, Number(e.currentTarget.value)) } else onChange(c.stateKey, e.currentTarget.value) }} /></label>
    case 'button': return <button type="button" className="intelligent-ui-action" disabled={actionsDisabled} onClick={() => onAction(c.actionId)}>{tx(c.label)}</button>
  }
}
function Chart({ component }: { component: Extract<UiComponent, { type: 'chart' }> }) {
  const { points, chartType } = component
  const min = Math.min(0, ...points.map(p => p.value))
  const max = Math.max(0, ...points.map(p => p.value))
  const range = max - min || 1
  const yFor = (value: number) => 36 + ((max - value) / range) * 142
  const baseline = yFor(0)
  const coords = points.map((p, i) => ({ x: points.length < 2 ? 50 : 36 + i * (528 / (points.length - 1)), y: yFor(p.value), ...p }))
  const description = points.map(p => `${p.label}: ${p.value}`).join('; ')
  return <figure className="intelligent-ui-chart"><svg viewBox="0 0 600 230" role="img" aria-label={`${chartType === 'bar' ? 'Bar' : 'Line'} chart. ${description}`}><line x1="30" y1={baseline} x2="580" y2={baseline} className="chart-axis" />{chartType === 'bar' ? coords.map((p, i) => <g key={i}><rect x={p.x - 12} y={Math.min(baseline, p.y)} width="24" height={Math.max(2, Math.abs(baseline - p.y))} rx="3" className="chart-mark"/><text x={p.x} y="202" textAnchor="middle">{p.label}</text></g>) : <><polyline points={coords.map(p => `${p.x},${p.y}`).join(' ')} className="chart-line" />{coords.map((p, i) => <g key={i}><circle cx={p.x} cy={p.y} r="4" className="chart-mark"/><text x={p.x} y="202" textAnchor="middle">{p.label}</text></g>)}</>}</svg><figcaption>{description || 'No chart data'}</figcaption></figure>
}
function Diagram({ component }: { component: Extract<UiComponent, { type: 'diagram' }> }) {
  const nodes = component.nodes
  const minX = Math.min(0, ...nodes.map(n => n.x)), maxX = Math.max(100, ...nodes.map(n => n.x)), minY = Math.min(0, ...nodes.map(n => n.y)), maxY = Math.max(100, ...nodes.map(n => n.y))
  const point = (id: string) => { const n = nodes.find(v => v.id === id)!; return { x: 26 + ((n.x - minX) / (maxX - minX || 1)) * 448, y: 24 + ((n.y - minY) / (maxY - minY || 1)) * 202, label: n.label } }
  const desc = nodes.map(n => n.label).join(', ')
  return <figure className="intelligent-ui-diagram"><svg viewBox="0 0 500 250" role="img" aria-label={`Diagram: ${desc}`}><defs><marker id={`arrow-${component.id}`} markerWidth="8" markerHeight="8" refX="6" refY="3" orient="auto"><path d="M0,0 L0,6 L7,3 z" /></marker></defs>{component.edges.map((e, i) => { const a = point(e.from), b = point(e.to); return <line key={i} x1={a.x} y1={a.y} x2={b.x} y2={b.y} markerEnd={`url(#arrow-${component.id})`} className="diagram-edge" /> })}{nodes.map(n => { const p = point(n.id); return <g key={n.id}><rect x={p.x - 48} y={p.y - 16} width="96" height="32" rx="8" className="diagram-node"/><text x={p.x} y={p.y + 4} textAnchor="middle">{n.label}</text></g>})}</svg><figcaption>{desc}. Connections: {component.edges.map(e => `${point(e.from).label} to ${point(e.to).label}`).join('; ') || 'none'}</figcaption></figure>
}
