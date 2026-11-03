import { lazy, Suspense, useCallback, useEffect, useRef, useState, type CSSProperties, type PointerEvent, type ReactNode } from 'react'
import { listen } from '@tauri-apps/api/event'
import { FileCode2, FolderTree, Globe, GitCompareArrows, Maximize2, Minimize2, Plus, TerminalSquare, X } from 'lucide-react'
import { Button } from '../ui/button'
import { BrowserTab, browserRequest } from './BrowserTab'
import { FilesTab } from './FilesTab'
import { GitDiffTab } from './GitDiffTab'
const TerminalTab = lazy(() => import('./TerminalTab').then(module => ({default: module.TerminalTab})))
import './WorkspacePanel.css'

type Kind = 'new' | 'browser' | 'terminal' | 'git' | 'files' | 'ui'
export type WorkspaceFileOpenRequest = {id: string; root: string; path: string; line?: number; threadId: string | null}
type Tab = {id: string; kind: Kind; title: string; root: string; url?: string; nativeId?: string; threadId: string | null; draftKey: number; filePath?: string; fileLine?: number}
type Props = {draftKey: number; promotion: {draftKey: number; threadId: string} | null; open: boolean; visible: boolean; request: number; root: string; roots?: string[]; threadId: string | null; desktop: boolean; onOpen: () => void; onClose: () => void; onWidth: (width: number) => void; onSendNotes: (text: string) => void; expanded?: boolean; onExpandedChange?: (expanded: boolean) => void; uiView?: {id: string; title: string; content: ReactNode}; onCloseUi?: () => void; fileOpen?: WorkspaceFileOpenRequest}
const names: Record<Kind, string> = {new: 'New tab', browser: 'Browser', terminal: 'Terminal', git: 'Git diff', files: 'Files', ui: 'Interactive response'}
const icons = {new: FileCode2, browser: Globe, terminal: TerminalSquare, git: GitCompareArrows, files: FolderTree, ui: FileCode2}
const storedWidth = () => { try { return Math.max(300, Math.min(1000, Number(localStorage.getItem('brr.workspace.width')) || 460)) } catch { return 460 } }

export function WorkspacePanel({draftKey, promotion, open, visible, request, root, roots, threadId, desktop, onOpen, onClose, onWidth, onSendNotes, expanded = false, onExpandedChange, uiView, onCloseUi, fileOpen}: Props) {
  const [tabs, setTabs] = useState<Tab[]>([])
  const [active, setActive] = useState('')
  const [width, setWidth] = useState(storedWidth)
  const [resizing, setResizing] = useState(false)
  const [dirty, setDirty] = useState<Record<string, boolean>>({})
  const [closingTab, setClosingTab] = useState('')
  const previousRequest = useRef(0)
  const previousUi = useRef('')
  const previousFileOpen = useRef('')
  const pane = useRef<HTMLElement>(null)
  const availableRoots = roots === undefined ? (root ? [root] : []) : [...new Set(roots.filter(Boolean))]
  const defaultRoot = roots === undefined ? root : (roots.includes(root) ? root : availableRoots[0] ?? '')
  const latest = useRef({root, threadId, draftKey, onOpen, tabs})
  latest.current = {root: defaultRoot, threadId, draftKey, onOpen, tabs}
  const addTab = useCallback((kind: Kind = 'new', details: Partial<Tab> = {}) => {
    const tab = {id: crypto.randomUUID(), kind, title: names[kind], root: latest.current.root, threadId: latest.current.threadId, draftKey: latest.current.draftKey, ...details}
    setTabs(current => [...current, tab]); setActive(tab.id); latest.current.onOpen()
  }, [])
  useEffect(() => { if (request !== previousRequest.current) { previousRequest.current = request; addTab() } }, [request, addTab])
  useEffect(() => {
    if (!fileOpen) { previousFileOpen.current = ''; return }
    const requestKey = JSON.stringify(fileOpen)
    if (previousFileOpen.current === requestKey) return
    previousFileOpen.current = requestKey
    const candidates = latest.current.tabs.filter(tab => tab.kind === 'files' && tab.root === fileOpen.root && tab.threadId === fileOpen.threadId && (fileOpen.threadId !== null || tab.draftKey === latest.current.draftKey))
    const reusable = candidates.find(tab => !dirty[tab.id])
    if (reusable) {
      setTabs(current => current.map(tab => tab.id === reusable.id ? {...tab, filePath: fileOpen.path, fileLine: fileOpen.line} : tab))
      setActive(reusable.id)
      onOpen()
    } else addTab('files', {root: fileOpen.root, threadId: fileOpen.threadId, filePath: fileOpen.path, fileLine: fileOpen.line})
  }, [fileOpen, addTab, onOpen, dirty])
  useEffect(() => {
    if (!uiView) { previousUi.current = ''; return }
    if (previousUi.current === uiView.id) return
    previousUi.current = uiView.id
    const existing = latest.current.tabs.find(tab => tab.kind === 'ui')
    if (existing) { setTabs(current => current.map(tab => tab.id === existing.id ? {...tab, title: uiView.title, threadId} : tab)); setActive(existing.id); onOpen() }
    else addTab('ui', {title: uiView.title, threadId})
  }, [uiView?.id, uiView?.title, threadId, addTab, onOpen])
  useEffect(() => {
    if (promotion) setTabs(current => current.map(tab => tab.threadId === null && tab.draftKey === promotion.draftKey ? {...tab, threadId: promotion.threadId} : tab))
  }, [promotion])
  useEffect(() => { onWidth(width) }, [width, onWidth])
  useEffect(() => {
    if (!desktop) return
    let stopOpen: (() => void) | undefined, stopNew: (() => void) | undefined, disposed = false
    void listen<{id: string; url: string; title?: string; threadId?: string}>('workspace-browser-open', event => {
      const existing = latest.current.tabs.find(tab => tab.nativeId === event.payload.id)
      if (existing) { setActive(existing.id); latest.current.onOpen(); return }
      addTab('browser', {url: event.payload.url, nativeId: event.payload.id, title: event.payload.title || 'Browser', threadId: event.payload.threadId ?? latest.current.threadId})
    }).then(stop => { if (disposed) stop(); else stopOpen = stop }).catch(() => {})
    void listen<{url: string; sourceId: string}>('workspace-browser-new-tab', event => {
      const source = latest.current.tabs.find(tab => tab.nativeId === event.payload.sourceId)
      if (source && source.threadId === latest.current.threadId && (source.threadId !== null || source.draftKey === latest.current.draftKey)) addTab('browser', {url: event.payload.url, threadId: source.threadId, draftKey: source.draftKey})
    }).then(stop => { if (disposed) stop(); else stopNew = stop }).catch(() => {})
    return () => { disposed = true; stopOpen?.(); stopNew?.() }
  }, [desktop, addTab])
  const activeTab = tabs.find(tab => tab.id === active)
  const activeBrowserId = open && visible && activeTab?.kind === 'browser' ? activeTab.nativeId : undefined
  useEffect(() => {
    if (desktop) void browserRequest('browser.context', {threadId: visible ? threadId : null, tabId: activeBrowserId ?? null, ownerThreadId: activeTab?.threadId ?? null}).catch(() => {})
  }, [desktop, threadId, visible, activeBrowserId, activeTab?.threadId])

  const resizeTo = (value: number) => {
    const available = pane.current?.parentElement?.getBoundingClientRect().width ?? window.innerWidth - 52
    const next = Math.max(300, Math.min(value, Math.max(300, available - 360)))
    setWidth(next)
    try { localStorage.setItem('brr.workspace.width', String(next)) } catch { /* Session resizing still works. */ }
  }
  useEffect(() => {
    if (!open || !visible || typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(() => {
      if (expanded) return
      const available = pane.current?.parentElement?.getBoundingClientRect().width
      if (available) setWidth(current => Math.max(300, Math.min(current, available - 360)))
    })
    if (pane.current?.parentElement) observer.observe(pane.current.parentElement)
    return () => observer.disconnect()
  }, [open, visible, expanded])
  const startResize = (event: PointerEvent<HTMLDivElement>) => {
    event.preventDefault(); event.currentTarget.setPointerCapture(event.pointerId); setResizing(true)
    const initialX = event.clientX, initialWidth = width
    const move = (next: globalThis.PointerEvent) => resizeTo(initialWidth + initialX - next.clientX)
    const finish = () => { setResizing(false); window.removeEventListener('pointermove', move); window.removeEventListener('pointerup', finish); window.removeEventListener('pointercancel', finish) }
    window.addEventListener('pointermove', move); window.addEventListener('pointerup', finish); window.addEventListener('pointercancel', finish)
  }
  const closeTab = (id: string, discard = false) => {
    if (dirty[id] && !discard) { setClosingTab(id); return }
    const next = tabs.filter(tab => tab.id !== id)
    if (tabs.find(tab => tab.id === id)?.kind === 'ui') onCloseUi?.()
    setTabs(next)
    if (active === id) setActive(next.at(-1)?.id ?? '')
    if (!next.length) onClose()
    dirtyCallbacks.current.delete(id)
    setDirty(current => { const next = {...current}; delete next[id]; return next }); setClosingTab('')
  }
  const patch = (id: string, changes: Partial<Tab>) => setTabs(current => current.map(tab => tab.id === id && Object.entries(changes).some(([key, value]) => tab[key as keyof Tab] !== value) ? {...tab, ...changes} : tab))
  const dirtyCallbacks = useRef(new Map<string, (value: boolean) => void>())
  const dirtyCallback = (id: string) => {
    if (!dirtyCallbacks.current.has(id)) dirtyCallbacks.current.set(id, value => setDirty(current => current[id] === value ? current : {...current, [id]: value}))
    return dirtyCallbacks.current.get(id)!
  }
  return <aside ref={pane} className="workspace-panel" data-expanded={expanded} aria-label="Workspace tools" hidden={!open || !visible} style={{'--workspace-panel-width': `${width}px`} as CSSProperties}>
    {!expanded && <div className="workspace-resizer" role="separator" aria-label="Resize workspace panel" aria-orientation="vertical" aria-valuemin={300} aria-valuemax={1000} aria-valuenow={Math.round(width)} tabIndex={0} onPointerDown={startResize} onKeyDown={event => { if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); resizeTo(width + (event.key === 'ArrowLeft' ? 32 : -32)) } }} />}
    <header className="workspace-tabs-header">
      <div className="workspace-tabs" role="tablist" aria-label="Workspace tabs">
        {tabs.map(tab => { const Icon = icons[tab.kind]; return <div className={`workspace-tab${active === tab.id ? ' is-active' : ''}`} key={tab.id}>
          <button type="button" role="tab" id={`workspace-tab-button-${tab.id}`} tabIndex={active === tab.id ? 0 : -1} aria-selected={active === tab.id} aria-controls={`workspace-tab-${tab.id}`} title={tab.root ? `${tab.title} · ${tab.root}` : tab.title} onClick={() => setActive(tab.id)} onKeyDown={event => {
            if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
            event.preventDefault()
            const index = tabs.findIndex(item => item.id === tab.id)
            const next = tabs[event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : (index + (event.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length]
            setActive(next.id); document.getElementById(`workspace-tab-button-${next.id}`)?.focus()
          }}><Icon size={14}/><span>{tab.title}</span>{dirty[tab.id] && <i aria-label="Unsaved changes" />}</button>
          <button className="workspace-tab-close" type="button" aria-label={`Close ${tab.title} tab`} onClick={() => closeTab(tab.id)}><X size={12}/></button>
        </div> })}
      </div>
      <Button variant="ghost" size="icon" aria-label={expanded ? 'Restore split view' : 'Expand workspace panel'} title={expanded ? 'Restore split view' : 'Expand workspace panel'} aria-pressed={expanded} onClick={() => onExpandedChange?.(!expanded)}>{expanded ? <Minimize2 size={16}/> : <Maximize2 size={16}/>}</Button>
      <Button variant="ghost" size="icon" aria-label="Add workspace tab" onClick={() => addTab()}><Plus size={16}/></Button>
      <Button variant="ghost" size="icon" aria-label="Close workspace panel" onClick={onClose}><X size={16}/></Button>
    </header>
    {closingTab && <div className="workspace-close-confirm" role="alert"><span>This tab has unsent notes or unsaved edits.</span><Button size="sm" variant="destructive" onClick={() => closeTab(closingTab, true)}>Discard and close</Button><Button size="sm" variant="ghost" onClick={() => setClosingTab('')}>Keep tab</Button></div>}
    {tabs.map(tab => <div className="workspace-tab-content" id={`workspace-tab-${tab.id}`} role="tabpanel" aria-label={tab.title} key={tab.id} hidden={active !== tab.id}>
      {tab.kind === 'new' ? <div className="workspace-new-tab"><h2>Open a tool</h2>{availableRoots.length > 1 && <label className="workspace-folder-selector">Folder<select aria-label="Folder" value={tab.root} onChange={event => patch(tab.id, {root: event.target.value})}>{availableRoots.map(folder => <option key={folder} value={folder}>{folder}</option>)}</select></label>}{(['browser', 'terminal', 'git', 'files'] as const).map(kind => { const Icon = icons[kind]; return <button type="button" key={kind} disabled={kind !== 'browser' && (!tab.root || !desktop)} onClick={() => patch(tab.id, {kind, title: names[kind]})}><Icon size={18}/><span>{names[kind]}</span></button> })}<p>{tab.root || 'Terminal, Git diff and Files need a project folder.'}</p></div> : <>
        {tab.root && tab.kind !== 'browser' && tab.kind !== 'ui' && <div className="workspace-project-label" title={tab.root}>{tab.root}</div>}
        {tab.kind === 'ui' && open && visible && active === tab.id && uiView?.content}
        {tab.kind === 'terminal' && <Suspense fallback={<p className="workspace-empty">Opening terminal…</p>}><TerminalTab root={tab.root} visible={open && visible && active === tab.id} /></Suspense>}
        {tab.kind === 'files' && <FilesTab root={tab.root} visible={open && visible && active === tab.id} initialPath={tab.filePath} line={tab.fileLine} onFilePath={path => patch(tab.id, {filePath: path, ...(tab.filePath !== path ? {fileLine: undefined} : {})})} onDirtyChange={dirtyCallback(tab.id)} />}
        {tab.kind === 'git' && <GitDiffTab root={tab.root} visible={open && visible && active === tab.id} />}
        {tab.kind === 'browser' && <BrowserTab initialUrl={tab.url} nativeId={tab.nativeId} desktop={desktop} visible={open && visible && active === tab.id && !resizing && !closingTab} ownerThreadId={tab.threadId} canSendNotes={tab.threadId === threadId && (threadId !== null || tab.draftKey === draftKey)} onNativeId={id => patch(tab.id, {nativeId: id})} onTitle={title => patch(tab.id, {title})} onDirtyChange={dirtyCallback(tab.id)} onSendNotes={onSendNotes} />}
      </>}
    </div>)}
  </aside>
}
