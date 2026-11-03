import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { Archive, Check, FileText, Image, LoaderCircle, Paperclip, Pause, Play, Plus, RefreshCw, Send, Trash2, X } from 'lucide-react'
import { chat, type ChatAttachment, type ChatSubmission, type QueueSnapshot } from '../lib/chat'
import { Button } from './ui/button'
import './ChatContext.css'

export type ChatContextBarProps = {
  ownerId: string
  attachments: ChatAttachment[]
  onAttachmentsChange: (attachments: ChatAttachment[]) => void
  onError: (error: string) => void
  disabled?: boolean
  onOpenReferences?: () => void
  onOpenCommands?: () => void
  renderToolbar?: (contextButton: ReactNode) => ReactNode
}

const messageOf = (reason: unknown) => reason instanceof Error ? reason.message : String(reason)

export function ChatContextBar({ ownerId, attachments, onAttachmentsChange, onError, disabled = false, onOpenReferences, onOpenCommands, renderToolbar }: ChatContextBarProps) {
  const trigger = useRef<HTMLButtonElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  const previewOpener = useRef<HTMLButtonElement | null>(null)
  const [menu, setMenu] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [retryPaths, setRetryPaths] = useState<string[] | null>(null)
  const [preview, setPreview] = useState<{ attachment: ChatAttachment; content: string; kind: 'image' | 'text' } | null>(null)

  const importPaths = async (paths: string[]) => {
    setBusy(true); setError('')
    const imported: ChatAttachment[] = []
    const failed: string[] = []
    let firstError = ''
    try {
      for (const path of paths) {
        try { imported.push(...await chat.importPaths(ownerId, [path])) }
        catch (reason) { failed.push(path); firstError ||= messageOf(reason) }
      }
      if (imported.length) onAttachmentsChange([...attachments, ...imported])
      setRetryPaths(failed.length ? failed : null)
      if (failed.length) {
        const text = `Could not attach ${failed.length === 1 ? 'a file' : `${failed.length} files`}: ${firstError}`
        setError(text); onError(text)
      } else setError('')
    } finally { setBusy(false) }
  }

  const chooseFiles = async () => {
    setMenu(false)
    try {
      const selected = await openDialog({ multiple: true, directory: false })
      if (!selected) return
      await importPaths(Array.isArray(selected) ? selected : [selected])
    } catch (reason) {
      const text = `Could not choose files: ${messageOf(reason)}`
      setError(text); onError(text)
    }
  }

  const remove = async (attachment: ChatAttachment) => {
    setError('')
    try {
      await chat.removeAttachment(ownerId, attachment.id)
      onAttachmentsChange(attachments.filter(item => item.id !== attachment.id))
    } catch (reason) {
      const text = `Could not remove ${attachment.name}: ${messageOf(reason)}`
      setError(text); onError(text)
    }
  }

  const showPreview = async (attachment: ChatAttachment, opener: HTMLButtonElement) => {
    setError('')
    try {
      const result = await chat.preview(ownerId, attachment.id)
      if (result.preview !== null) {
        previewOpener.current = opener
        setPreview({ attachment, content: result.preview, kind: attachment.kind === 'image' ? 'image' : 'text' })
      } else {
        const text = result.reason === 'preview-too-large'
          ? `This ${attachment.kind === 'image' ? 'image' : 'file'} is too large to preview. Its original remains available to tool readers.`
          : result.reason === 'preview-binary'
            ? 'This binary file cannot be previewed. Its original remains available to tool readers; Brr does not extract local document contents.'
            : 'No built-in preview is available. The original remains available to tool readers; Brr does not extract local document contents.'
        setError(text)
      }
    } catch (reason) {
      const text = `Could not preview ${attachment.name}: ${messageOf(reason)}`
      setError(text); onError(text)
    }
  }

  const closePreview = () => {
    setPreview(null)
    previewOpener.current?.focus()
    previewOpener.current = null
  }

  useEffect(() => {
    if (!menu) return
    menuRef.current?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus()
    const outside = (event: PointerEvent) => {
      const target = event.target
      if (!(target instanceof Node) || (!menuRef.current?.contains(target) && !trigger.current?.contains(target))) setMenu(false)
    }
    document.addEventListener('pointerdown', outside)
    return () => document.removeEventListener('pointerdown', outside)
  }, [menu])

  const onMenuKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const items = Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') ?? [])
    if (!items.length) return
    const active = items.indexOf(document.activeElement as HTMLButtonElement)
    if (event.key === 'Escape') { event.preventDefault(); setMenu(false); trigger.current?.focus(); return }
    let next = -1
    if (event.key === 'ArrowDown') next = (active + 1 + items.length) % items.length
    else if (event.key === 'ArrowUp') next = (active - 1 + items.length) % items.length
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = items.length - 1
    if (next >= 0) { event.preventDefault(); items[next]?.focus() }
  }

  useEffect(() => {
    if (!preview) return
    const close = () => { setPreview(null); previewOpener.current?.focus(); previewOpener.current = null }
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === 'Escape') { event.preventDefault(); close(); return }
      if (event.key !== 'Tab') return
      const dialog = document.querySelector<HTMLElement>('.chat-context-preview')
      const focusable = Array.from(dialog?.querySelectorAll<HTMLElement>('button:not([disabled]), [tabindex]:not([tabindex="-1"])') ?? [])
      if (!focusable.length) { event.preventDefault(); dialog?.focus(); return }
      const first = focusable[0], last = focusable[focusable.length - 1]
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus() }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus() }
    }
    document.addEventListener('keydown', onKeyDown)
    document.querySelector<HTMLButtonElement>('.chat-context-preview-close')?.focus()
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [preview])

  const contextButton = <div className="chat-context-add-wrap">
    <Button ref={trigger} type="button" variant="ghost" size="icon" className="chat-context-add" aria-label="Add context" title="Add context" aria-expanded={menu} aria-haspopup="menu" disabled={disabled || busy} onClick={() => setMenu(value => !value)}>
      {busy ? <LoaderCircle size={16} className="chat-context-spin" aria-hidden="true" /> : <Plus size={17} aria-hidden="true" />}
    </Button>
    {menu && <div ref={menuRef} className="chat-context-menu" role="menu" aria-label="Add context" onKeyDown={onMenuKeyDown}>
      <button type="button" role="menuitem" onClick={chooseFiles}><Paperclip size={15} aria-hidden="true" />Attach files</button>
      {onOpenReferences && <button type="button" role="menuitem" onClick={() => { setMenu(false); onOpenReferences() }}><FileText size={15} aria-hidden="true" />Add reference</button>}
      {onOpenCommands && <button type="button" role="menuitem" onClick={() => { setMenu(false); onOpenCommands() }}><Archive size={15} aria-hidden="true" />Browse commands</button>}
    </div>}
  </div>

  return <>
    <section className="chat-context" aria-label="Chat context">
    {!!attachments.length && <span className="chat-context-count">{attachments.length} attached</span>}
    {attachments.length > 0 && <ul className="chat-context-list" aria-label="Attached context">
      {attachments.map(item => <li className="chat-context-card" key={item.id}>
        <span className="chat-context-card-icon" aria-hidden="true">{item.kind === 'image' ? <Image size={17} /> : <FileText size={17} />}</span>
        <span className="chat-context-card-info">
          <button type="button" className="chat-context-preview-link" onClick={event => void showPreview(item, event.currentTarget)} aria-label={`Preview ${item.name}`}>{item.name}</button>
          <small>{formatSize(item.size)} · {item.kind === 'image' ? item.mime : 'Original available to tool readers · no extraction'}</small>
        </span>
        <Button type="button" variant="ghost" size="icon" className="chat-context-remove" aria-label={`Remove ${item.name}`} disabled={disabled || busy} onClick={() => void remove(item)}><X size={15} aria-hidden="true" /></Button>
      </li>)}
    </ul>}
    {error && <div className="chat-context-error" role="alert"><span>{error}</span>{retryPaths && <Button type="button" variant="ghost" size="sm" disabled={busy || disabled} onClick={() => void importPaths(retryPaths)}><RefreshCw size={14} aria-hidden="true" />Retry import</Button>}<Button type="button" variant="ghost" size="icon" aria-label="Clear attachment error" onClick={() => setError('')}><X size={14} aria-hidden="true" /></Button></div>}
    {preview && <div className="chat-context-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) closePreview() }}>
      <section className="chat-context-preview" role="dialog" aria-modal="true" aria-label={`Preview ${preview.attachment.name}`} tabIndex={-1}>
        <header><strong>{preview.attachment.name}</strong><Button type="button" variant="ghost" size="icon" className="chat-context-preview-close" aria-label="Close preview" onClick={closePreview}><X size={17} aria-hidden="true" /></Button></header>
        {preview.kind === 'image' ? <img src={preview.content} alt={preview.attachment.name} /> : <pre className="chat-context-text-preview">{preview.content}</pre>}
        <p>{preview.attachment.mime} · {formatSize(preview.attachment.size)}</p>
      </section>
    </div>}
    </section>
    {renderToolbar ? renderToolbar(contextButton) : <div className="chat-context-toolbar">{contextButton}</div>}
  </>
}

export type ChatQueueProps = {
  snapshot: QueueSnapshot
  running: boolean
  threadId: string
  onChanged: () => void
  onError: (error: string) => void
}

export function ChatQueue({ snapshot, running, threadId, onChanged, onError }: ChatQueueProps) {
  const [editing, setEditing] = useState<string | null>(null)
  const [editText, setEditText] = useState('')
  const [expanded, setExpanded] = useState<Record<string, boolean>>({})
  const [busy, setBusy] = useState('')
  const [error, setError] = useState('')
  const entries = snapshot.entries.filter(entry => ['queued', 'dispatching', 'uncertain', 'failed'].includes(entry.status))

  const action = async (key: string, work: () => Promise<unknown>) => {
    setBusy(key); setError('')
    try { await work(); onChanged() }
    catch (reason) { const text = messageOf(reason); setError(text); onError(text) }
    finally { setBusy('') }
  }
  const beginEdit = (entry: ChatSubmission) => {
    setEditing(entry.id)
    setEditText(entry.document.text)
    setExpanded(previous => ({ ...previous, [entry.id]: true }))
  }

  if (!entries.length && !snapshot.state.paused) return null

  if (!entries.length) return <section className="chat-queue chat-queue-compact" aria-label="Message queue">
    <span className="chat-queue-paused">Queue paused</span>
    <Button type="button" variant="ghost" size="sm" aria-label="Resume queue" disabled={!!busy} onClick={() => void action('queue', () => chat.resume(threadId))}>
      <Play size={14} aria-hidden="true" />Resume queue
    </Button>
    {error && <p className="chat-queue-error" role="alert">{error}</p>}
  </section>

  return <section className="chat-queue" aria-label="Message queue">
    <div className="chat-queue-heading"><div><strong>Queued messages</strong><span>{entries.length}</span></div>
      <Button type="button" variant="ghost" size="sm" aria-label={snapshot.state.paused ? 'Resume queue' : 'Pause queue'} disabled={!!busy} onClick={() => void action('queue', () => snapshot.state.paused ? chat.resume(threadId) : chat.pause(threadId))}>
        {snapshot.state.paused ? <Play size={14} aria-hidden="true" /> : <Pause size={14} aria-hidden="true" />}{snapshot.state.paused ? 'Resume queue' : 'Pause queue'}
      </Button>
    </div>
    {snapshot.state.paused && <p className="chat-queue-paused">Queue paused{snapshot.state.reason ? ` · ${snapshot.state.reason}` : ''}</p>}
    <ul className="chat-queue-list" aria-label="Pending messages">
      {entries.map(entry => {
        const text = entry.document.text || 'Message with attachments'
        const label = entry.status === 'queued' ? 'Queued' : entry.status === 'dispatching' ? 'Sending' : entry.status === 'uncertain' ? 'Needs review' : 'Failed'
        return <li className={`chat-queue-item is-${entry.status}`} key={entry.id}>
          <details open={editing === entry.id || expanded[entry.id] === true} onToggle={event => {
            const isOpen = event.currentTarget.open
            setExpanded(previous => ({ ...previous, [entry.id]: isOpen }))
          }}>
            <summary aria-label={`${label}: ${text}`}><span className="chat-queue-row-status">{label}</span><span className="chat-queue-row-text">{text}</span></summary>
            <div className="chat-queue-item-details">
              {entry.status === 'uncertain' ? <div className="chat-queue-uncertain" role="status"><strong>Delivery could not be confirmed</strong><span>Check the conversation before sending again.</span>{entry.error && <small>{entry.error}</small>}</div>
                : entry.status === 'failed' ? <div className="chat-queue-failed" role="status"><strong>Message was not sent</strong>{entry.error && <span>{entry.error}</span>}</div>
                  : entry.status === 'dispatching' ? <span className="chat-queue-status"><LoaderCircle size={14} className="chat-context-spin" aria-hidden="true" />Sending…</span>
                    : editing === entry.id ? <div className="chat-queue-edit"><label htmlFor={`queue-edit-${entry.id}`}>Edit queued message</label><textarea id={`queue-edit-${entry.id}`} value={editText} onChange={event => setEditText(event.target.value)} disabled={!!busy} /><div><Button type="button" variant="ghost" size="sm" disabled={!!busy || !editText.trim()} onClick={() => void action(entry.id, async () => { await chat.editQueued(entry.id, editText); setEditing(null) })}><Check size={14} aria-hidden="true" />Save edit</Button><Button type="button" variant="ghost" size="sm" disabled={!!busy} onClick={() => setEditing(null)}>Cancel</Button></div></div>
                      : <><p>{text}</p><div className="chat-queue-item-actions"><Button type="button" variant="ghost" size="sm" disabled={!!busy} onClick={() => beginEdit(entry)}>Edit</Button><Button type="button" variant="ghost" size="sm" disabled={!!busy} onClick={() => void action(entry.id, () => chat.removeQueued(entry.id))}><Trash2 size={14} aria-hidden="true" />Remove</Button><Button type="button" variant="secondary" size="sm" disabled={!!busy || !running} onClick={() => void action(entry.id, () => chat.steerQueued(entry.id))}><Send size={14} aria-hidden="true" />Steer now</Button></div></>}
            </div>
          </details>
        </li>
      })}
    </ul>
    {error && <p className="chat-queue-error" role="alert">{error}</p>}
  </section>
}

function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}
