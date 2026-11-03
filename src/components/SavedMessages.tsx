import { useCallback, useEffect, useRef, useState } from 'react'
import { Copy, RotateCcw, X } from 'lucide-react'
import { Button } from './ui/button'
import { chat, type RecoveryItem, type SavedDraft } from '../lib/chat'
import './SavedMessages.css'

type Props = {
  onRestore: (draft: SavedDraft) => Promise<void> | void
  interruptedSetupCount?: number
  onReviewInterruptedSetups?: () => void
}

const operationKey = (item: RecoveryItem) => `brr:recovery-restore:${item.sourceKind}:${item.sourceId}`
function operationIdFor(item: RecoveryItem, cachedIds: Map<string, string>) {
  const key = operationKey(item)
  const cached = cachedIds.get(key)
  if (cached) return cached
  try {
    const existing = localStorage.getItem(key)
    if (existing) { cachedIds.set(key, existing); return existing }
    const operationId = crypto.randomUUID()
    localStorage.setItem(key, operationId)
    cachedIds.set(key, operationId)
    return operationId
  } catch {
    const operationId = crypto.randomUUID()
    cachedIds.set(key, operationId)
    return operationId
  }
}

export function SavedMessages({ onRestore, interruptedSetupCount = 0, onReviewInterruptedSetups }: Props) {
  const [items, setItems] = useState<RecoveryItem[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const [busyId, setBusyId] = useState('')
  const [notice, setNotice] = useState('')
  const fetchVersion = useRef(0)
  const mounted = useRef(false)
  const operationIds = useRef(new Map<string, string>())

  const refresh = useCallback(async () => {
    const version = ++fetchVersion.current
    try {
      const response = await chat.recovery.list()
      if (!mounted.current || version !== fetchVersion.current) return
      setItems(response.items)
      setError('')
    } catch (reason) {
      if (mounted.current && version === fetchVersion.current) setError(`Saved messages could not be loaded: ${String(reason)}`)
    } finally {
      if (mounted.current && version === fetchVersion.current) setLoading(false)
    }
  }, [])

  useEffect(() => {
    mounted.current = true
    void refresh()
    return () => { mounted.current = false; fetchVersion.current += 1 }
  }, [refresh])

  const restore = async (item: RecoveryItem) => {
    if (!item.restorable || !['queued', 'failed', 'draft'].includes(item.status) || busyId) return
    setBusyId(`${item.sourceKind}:${item.sourceId}`)
    setError('')
    try {
      const result = await chat.recovery.restore({ sourceId: item.sourceId, sourceKind: item.sourceKind, operationId: operationIdFor(item, operationIds.current) })
      if (!mounted.current) return
      if (result.target === 'chat') {
        setNotice('This message was already restored to chat.')
        await refresh()
        return
      }
      await onRestore(result.draft)
      if (!mounted.current) return
      setNotice('Restored as a new saved draft. It has not been sent.')
      await refresh()
    } catch (reason) {
      if (mounted.current) setError(`Could not restore saved message: ${String(reason)}`)
    } finally {
      if (mounted.current) setBusyId('')
    }
  }

  const cancel = async (item: RecoveryItem) => {
    if (item.sourceKind !== 'submission' || item.status !== 'queued' || busyId) return
    setBusyId(`${item.sourceKind}:${item.sourceId}`)
    setError('')
    try {
      await chat.recovery.cancel({ submissionId: item.sourceId, expectedRevision: item.revision })
      if (!mounted.current) return
      setNotice('Saved queued message cancelled.')
      await refresh()
    } catch (reason) {
      if (mounted.current) setError(`Could not cancel saved message: ${String(reason)}`)
    } finally {
      if (mounted.current) setBusyId('')
    }
  }

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text)
      setNotice('Message copied.')
    } catch (reason) {
      setError(`Could not copy message: ${String(reason)}`)
    }
  }

  return <section className="saved-messages-page" aria-labelledby="saved-messages-title">
    <header className="saved-messages-header">
      <div><p className="saved-messages-eyebrow">Recovery</p><h1 id="saved-messages-title">Saved messages — runtime chat unavailable</h1></div>
      <Button variant="ghost" size="sm" onClick={() => void refresh()} disabled={loading}>Refresh</Button>
    </header>
    <p className="saved-messages-intro">These messages were preserved when their runtime chat could not be recovered. Restoring creates an unsent draft.</p>
    {interruptedSetupCount > 0 && onReviewInterruptedSetups && <Button className="saved-messages-review-setups" type="button" variant="ghost" size="sm" onClick={onReviewInterruptedSetups}>Review interrupted setups ({interruptedSetupCount})</Button>}
    {error && <p role="status" className="saved-messages-error">{error}</p>}
    {notice && <p role="status" className="saved-messages-notice">{notice}</p>}
    {loading ? <p className="saved-messages-empty">Loading saved messages…</p> : items.length === 0 ? <p className="saved-messages-empty">No saved messages need attention.</p> : <div className="saved-messages-list" aria-label="Saved messages">
      {items.map(item => {
        const rowId = `${item.sourceKind}:${item.sourceId}`
        const canRestore = item.restorable && ['queued', 'failed', 'draft'].includes(item.status)
        const canCancel = item.sourceKind === 'submission' && item.status === 'queued'
        const busy = busyId === rowId
        return <article className="saved-message-card" key={rowId}>
          <div className="saved-message-meta"><span className={`saved-message-status status-${item.status}`}>{item.status === 'uncertain' ? 'Unconfirmed' : item.status === 'draft' ? 'Saved draft' : item.status}</span><span>{item.kind === 'agent' ? 'Agent chat' : 'Chat'}</span></div>
          <p className="saved-message-text">{item.text || '(No text)'}</p>
          {item.reason && <p className="saved-message-reason">{item.reason}</p>}
          {item.status === 'uncertain' && <p className="saved-message-reason">Delivery is unknown. This message can only be copied.</p>}
          {item.status === 'dispatching' && <p className="saved-message-reason">Delivery is still being checked. This message can only be copied.</p>}
          {item.status === 'draft' && !item.restorable && <p className="saved-message-reason">This message is already saved as a draft.</p>}
          {!canRestore && item.status !== 'uncertain' && item.reason && <p className="saved-message-reason">This message cannot be restored automatically.</p>}
          <div className="saved-message-actions">
            <Button variant="ghost" size="sm" onClick={() => void copy(item.text)}><Copy size={14} aria-hidden="true"/>Copy</Button>
            {canCancel && <Button variant="ghost" size="sm" disabled={busy} onClick={() => void cancel(item)}><X size={14} aria-hidden="true"/>{busy ? 'Cancelling…' : 'Cancel'}</Button>}
            {canRestore && <Button variant="secondary" size="sm" disabled={busy || Boolean(busyId)} onClick={() => void restore(item)}><RotateCcw size={14} aria-hidden="true"/>{busy ? 'Restoring…' : 'Restore'}</Button>}
          </div>
        </article>
      })}
    </div>}
  </section>
}
