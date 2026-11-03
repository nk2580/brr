import { useEffect, useRef, useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { ArrowLeft, ArrowRight, MessageSquarePlus, MousePointer2, RotateCw, Trash2, X } from 'lucide-react'
import { Button } from '../ui/button'

export function browserRequest<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  return invoke<T>('workspace_browser_request', {method, params})
}
type Selection = {url: string; title: string; selector: string; quote: string; x?: number; y?: number}
type Note = Selection & {id: string; note: string}
type Snapshot = {url?: string; title?: string; ready?: boolean; annotating?: boolean; selection?: Selection | null}
type Props = {initialUrl?: string; nativeId?: string; visible: boolean; desktop: boolean; ownerThreadId?: string | null; canSendNotes: boolean; onNativeId: (id: string) => void; onTitle: (title: string) => void; onSendNotes: (text: string) => void; onDirtyChange: (dirty: boolean) => void}

export function BrowserTab({initialUrl = 'about:blank', nativeId, visible, desktop, ownerThreadId, canSendNotes, onNativeId, onTitle, onSendNotes, onDirtyChange}: Props) {
  const [id, setId] = useState(nativeId ?? '')
  const [address, setAddress] = useState(initialUrl === 'about:blank' ? '' : initialUrl)
  const [currentUrl, setCurrentUrl] = useState(initialUrl)
  const [error, setError] = useState('')
  const [annotating, setAnnotating] = useState(false)
  const [selection, setSelection] = useState<Selection | null>(null)
  const [note, setNote] = useState('')
  const [notes, setNotes] = useState<Note[]>([])
  const [showNotes, setShowNotes] = useState(false)
  const addressEditing = useRef(false)
  const surface = useRef<HTMLDivElement>(null)
  const pending = useRef<Promise<{id: string}> | null>(null)
  const currentId = useRef(id)
  const generation = useRef(0)
  const callbacks = useRef({onNativeId, onTitle})
  callbacks.current = {onNativeId, onTitle}; currentId.current = id

  useEffect(() => { onDirtyChange(notes.length > 0 || Boolean(note.trim())) }, [notes.length, note, onDirtyChange])
  useEffect(() => {
    ++generation.current
    return () => {
      const closing = ++generation.current
      queueMicrotask(() => {
        if (generation.current !== closing) return
        void (pending.current ?? Promise.resolve({id: currentId.current})).then(result => {
          if (result.id) return browserRequest('browser.close', {id: result.id})
        }).catch(() => {})
      })
    }
  }, [])
  useEffect(() => {
    if (!desktop || !visible || id || pending.current) return
    pending.current = browserRequest<{id: string}>('browser.create', {url: initialUrl, ownerThreadId: ownerThreadId ?? null, ...(nativeId ? {id: nativeId} : {})})
    void pending.current.then(result => { setId(result.id); currentId.current = result.id; callbacks.current.onNativeId(result.id) }).catch(reason => { setError(String(reason)); pending.current = null })
  }, [desktop, visible, id, initialUrl, nativeId, ownerThreadId])

  useEffect(() => {
    if (!desktop || !id) return
    const updateBounds = () => {
      const rect = surface.current?.getBoundingClientRect()
      const show = visible && currentUrl !== 'about:blank' && Boolean(rect?.width && rect?.height)
      void browserRequest('browser.bounds', {id, x: rect?.x ?? 0, y: rect?.y ?? 0, width: rect?.width ?? 1, height: rect?.height ?? 1, visible: show}).catch(reason => { if (visible) setError(String(reason)) })
    }
    updateBounds()
    const observer = new ResizeObserver(updateBounds)
    if (surface.current) observer.observe(surface.current)
    window.addEventListener('resize', updateBounds)
    return () => { observer.disconnect(); window.removeEventListener('resize', updateBounds); void browserRequest('browser.bounds', {id, visible: false}).catch(() => {}) }
  }, [desktop, id, visible, currentUrl, selection, showNotes])
  useEffect(() => {
    if (!desktop || !id) return
    let disposed = false, stop: (() => void) | undefined
    let locationEvents = 0
    const applyLocation = (location: {id: string; url?: string}) => {
      if (disposed || location.id !== id || !location.url) return
      setCurrentUrl(location.url)
      if (!addressEditing.current) setAddress(location.url === 'about:blank' ? '' : location.url)
    }
    void listen<{id: string; url: string}>('workspace-browser-location', event => {
      if (event.payload.id !== id) return
      ++locationEvents
      applyLocation(event.payload)
    }).then(async unlisten => {
      if (disposed) { unlisten(); return }
      stop = unlisten
      // Replay cached navigation after subscribing, without evaluating a blank page
      // or draining an annotation selection from the page snapshot.
      const beforeRead = locationEvents
      const location = await browserRequest<{id: string; url?: string}>('browser.location', {id})
      if (locationEvents === beforeRead) applyLocation(location)
    }).catch(() => {})
    return () => { disposed = true; stop?.() }
  }, [desktop, id])
  useEffect(() => {
    if (!desktop || !id || !visible || currentUrl === 'about:blank') return
    let active = true, busy = false
    const poll = async () => {
      if (busy) return
      busy = true
      try {
        const result = await browserRequest<Snapshot>('browser.snapshot', {id})
        if (!active) return
        setError('')
        if (result.url) { setCurrentUrl(result.url); if (!addressEditing.current) setAddress(result.url === 'about:blank' ? '' : result.url) }
        if (result.title) callbacks.current.onTitle(result.title)
        setAnnotating(Boolean(result.annotating))
        if (result.selection) { setSelection(result.selection); setNote(''); setShowNotes(true) }
      } catch (reason) { if (active) setError(String(reason)) }
      finally { busy = false }
    }
    void poll()
    const timer = setInterval(() => void poll(), 700)
    return () => { active = false; clearInterval(timer) }
  }, [desktop, id, visible, currentUrl])

  const act = async (action: string, extras: Record<string, unknown> = {}) => {
    if (!id) return
    try { setError(''); await browserRequest('browser.action', {id, action, ...(action === 'annotate' ? {color: getComputedStyle(document.documentElement).getPropertyValue('--accent').trim()} : {}), ...extras}); if (action === 'annotate') setAnnotating(Boolean(extras.enabled)) }
    catch (reason) { setError(String(reason)) }
  }
  const navigate = async () => {
    if (!id || !address.trim()) return
    let url = address.trim()
    if (!/^[a-z][a-z\d+.-]*:/i.test(url)) url = `https://${url}`
    try { setError(''); await browserRequest('browser.navigate', {id, url}); setCurrentUrl(url) }
    catch (reason) { setError(String(reason)) }
  }
  const sendNotes = () => {
    if (!notes.length || !canSendNotes) return
    const text = ['Web page annotations:', ...notes.map((entry, index) => `${index + 1}. ${entry.title || entry.url}\nPage: ${entry.url}\nElement: ${entry.selector}\nSelected text: ${entry.quote}\nNote: ${entry.note}`)].join('\n\n')
    onSendNotes(text); setNotes([]); setShowNotes(false)
  }
  return <section className="workspace-browser" aria-label="Browser" hidden={!visible}>
    <form className="workspace-browser-toolbar" onSubmit={event => { event.preventDefault(); void navigate() }}>
      <Button type="button" size="icon" variant="ghost" aria-label="Back in browser" disabled={!id} onClick={() => void act('back')}><ArrowLeft size={15}/></Button>
      <Button type="button" size="icon" variant="ghost" aria-label="Forward in browser" disabled={!id} onClick={() => void act('forward')}><ArrowRight size={15}/></Button>
      <Button type="button" size="icon" variant="ghost" aria-label="Reload page" disabled={!id} onClick={() => void act('reload')}><RotateCw size={14}/></Button>
      <input aria-label="Website address" placeholder="Enter a URL" value={address} onFocus={event => { addressEditing.current = true; event.currentTarget.select() }} onChange={event => setAddress(event.target.value)} onBlur={() => { addressEditing.current = false; if (!address) setAddress(currentUrl === 'about:blank' ? '' : currentUrl) }} />
      <Button type="submit" size="sm" variant="ghost" disabled={!id || !address.trim()}>Go</Button>
      <Button type="button" size="icon" variant="ghost" aria-label="Annotate page" title="Select a page element to annotate" aria-pressed={annotating} disabled={!id || currentUrl === 'about:blank'} onClick={() => void act('annotate', {enabled: !annotating})}><MousePointer2 size={15}/></Button>
      <Button type="button" size="icon" variant="ghost" aria-label={`Page notes${notes.length ? ` (${notes.length})` : ''}`} title="Page notes" aria-pressed={showNotes} onClick={() => setShowNotes(value => !value)}><MessageSquarePlus size={15}/>{notes.length > 0 && <span className="workspace-note-count">{notes.length}</span>}</Button>
    </form>
    {error && <p className="workspace-error" role="alert">{error}<Button size="icon" variant="ghost" aria-label="Dismiss browser error" onClick={() => setError('')}><X size={14}/></Button></p>}
    {annotating && <div className="workspace-browser-hint">Click an element on the page to add a note.</div>}
    <div ref={surface} className="workspace-browser-surface" aria-label="Web page">
      {!desktop ? <p className="workspace-empty">Open Brr on desktop to browse websites here.</p> : currentUrl === 'about:blank' && <p className="workspace-empty">Enter a website address to start browsing.</p>}
    </div>
    {(showNotes || selection) && <section className="workspace-browser-notes" aria-label="Page annotations">
      {selection && <form onSubmit={event => { event.preventDefault(); if (!note.trim()) return; setNotes(current => [...current, {...selection, id: crypto.randomUUID(), note: note.trim()}]); setSelection(null); setNote(''); void act('annotate', {enabled: false}) }}>
        <div className="workspace-annotation-target"><span title={selection.selector}>{selection.quote || selection.selector}</span><Button type="button" size="icon" variant="ghost" aria-label="Cancel annotation" onClick={() => { setSelection(null); setNote('') }}><X size={14}/></Button></div>
        <textarea autoFocus aria-label="Annotation note" placeholder="What should change here?" value={note} onChange={event => setNote(event.target.value)} />
        <Button size="sm" disabled={!note.trim()}>Add note</Button>
      </form>}
      {notes.map((entry, index) => <div className="workspace-annotation" key={entry.id}><span>{index + 1}. {entry.note}</span><Button size="icon" variant="ghost" aria-label={`Delete annotation ${index + 1}`} onClick={() => setNotes(current => current.filter(item => item.id !== entry.id))}><Trash2 size={13}/></Button></div>)}
      {!notes.length && !selection && <p className="workspace-empty">Use Annotate page to collect notes, then send them to the chat.</p>}
      {notes.length > 0 && !canSendNotes && <p className="workspace-empty">Return to the original chat to send these notes.</p>}
      {notes.length > 0 && <Button size="sm" disabled={!canSendNotes} title={canSendNotes ? undefined : 'Return to the chat where these notes were collected'} onClick={sendNotes}>Add notes to message</Button>}
    </section>}
  </section>
}
