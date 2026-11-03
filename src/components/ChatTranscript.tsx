import { useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import type { AgentItem, SessionState } from '../lib/session'
import { activeTranscriptTurnIndex, groupTranscriptItems, itemText, openingUserMessageIds, transcriptTurns, workSummary } from '../lib/transcript'
import './ChatTranscript.css'

type ScrollState = { top: number; atBottom: boolean; anchor?: { id: string; offset: number } }
const scrollByThread = new Map<string, ScrollState>()
const bottomGap = 72

function restoreSemanticAnchor(root: HTMLElement, anchor: ScrollState['anchor']) {
  if (!anchor) return
  const row = [...root.querySelectorAll<HTMLElement>('[data-transcript-item]')]
    .find(candidate => candidate.dataset.transcriptItem === anchor.id)
  if (row) root.scrollTop += row.getBoundingClientRect().top - root.getBoundingClientRect().top - anchor.offset
}

export type ChatTranscriptProps = {
  session: SessionState
  threadId: string
  renderItem: (item: AgentItem) => ReactNode
  onQuote: (text: string) => void
  onReuse: (text: string) => void
  onEdit: (item: AgentItem, turnId: string) => void
  onBranch: (turnId: string) => void
  onOpenFile?: (path: string, line?: number) => void
  afterItems?: ReactNode
  actionsDisabled?: boolean
}

export function ChatTranscript({ session, threadId, renderItem, onQuote, onReuse, onEdit, onBranch, onOpenFile, afterItems, actionsDisabled = false }: ChatTranscriptProps) {
  const root = useRef<HTMLDivElement>(null)
  const followBottom = useRef(true)
  const anchor = useRef<ScrollState['anchor']>(undefined)
  const [showLatest, setShowLatest] = useState(false)
  const [copyStatus, setCopyStatus] = useState('')
  const [openWork, setOpenWork] = useState<Record<string, boolean>>({})
  const editableTurnId = session.status === 'running' ? session.turnId : null
  const waitingForApproval = Boolean(session.turnId && session.approvals.some(approval => approval.params.turnId === session.turnId))
  const entries = useMemo(() => groupTranscriptItems(session.items, threadId, {
    turnId: session.turnId,
    status: waitingForApproval ? 'waiting' : session.status,
  }), [session.items, session.turnId, session.status, threadId, waitingForApproval])
  const turns = useMemo(() => transcriptTurns(session.items), [session.items])
  const currentTurnIndex = activeTranscriptTurnIndex(turns, session.turnId)
  const editableOpeningIds = useMemo(() => openingUserMessageIds(session.items, editableTurnId), [session.items, editableTurnId])

  useLayoutEffect(() => {
    const element = root.current
    if (!element) return
    const saved = scrollByThread.get(threadId)
    if (saved) {
      element.scrollTop = saved.top
      followBottom.current = saved.atBottom
      anchor.current = saved.anchor
    } else {
      element.scrollTop = element.scrollHeight
      followBottom.current = true
    }
    setShowLatest(!followBottom.current)
  }, [threadId])

  useLayoutEffect(() => {
    const element = root.current
    if (!element) return
    if (followBottom.current) {
      element.scrollTop = element.scrollHeight
      return
    }
    restoreSemanticAnchor(element, anchor.current)
  }, [session.items, openWork])

  useLayoutEffect(() => {
    const element = root.current
    const messages = element?.querySelector('.messages')
    if (!element || !messages || typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(() => {
      if (followBottom.current) element.scrollTop = element.scrollHeight
      else restoreSemanticAnchor(element, anchor.current)
      scrollByThread.set(threadId, { top: element.scrollTop, atBottom: followBottom.current, anchor: anchor.current })
    })
    observer.observe(messages)
    return () => observer.disconnect()
  }, [threadId, entries, openWork])

  const onScroll = () => {
    const element = root.current
    if (!element) return
    const atBottom = element.scrollHeight - element.scrollTop - element.clientHeight <= bottomGap
    followBottom.current = atBottom
    setShowLatest(!atBottom)
    const first = [...element.querySelectorAll<HTMLElement>('[data-transcript-item]')]
      .find(row => row.getBoundingClientRect().bottom > element.getBoundingClientRect().top)
    anchor.current = first ? {
      id: first.dataset.transcriptItem ?? '',
      offset: first.getBoundingClientRect().top - element.getBoundingClientRect().top,
    } : undefined
    scrollByThread.set(threadId, { top: element.scrollTop, atBottom, anchor: anchor.current })
  }

  const jumpLatest = () => {
    const element = root.current
    if (!element) return
    element.scrollTo({ top: element.scrollHeight, behavior: 'smooth' })
    followBottom.current = true
    setShowLatest(false)
  }

  const navigateTurn = (direction: -1 | 1) => {
    const next = turns[currentTurnIndex + direction]
    if (!next) return
    ;[...(root.current?.querySelectorAll<HTMLElement>('[data-turn-id]') ?? [])]
      .find(element => element.dataset.turnId === next)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
  }

  const messageActions = (item: AgentItem) => {
    const text = itemText(item)
    if (!text.trim()) return null
    const turnId = typeof item.turnId === 'string' ? item.turnId : session.turnId
    return <div className="chat-transcript-actions" aria-label="Message actions">
      <button type="button" onClick={() => void copy(text)}>Copy</button>
      <button type="button" onClick={() => onQuote(text)}>Quote</button>
      <button type="button" onClick={() => onReuse(text)}>Reuse</button>
      {item.type === 'userMessage' && editableOpeningIds.has(item.id) && turnId && <button type="button" disabled={actionsDisabled} title={actionsDisabled ? 'Wait for the current turn to finish before editing a branch.' : 'Branch before this turn using current files.'} onClick={() => onEdit(item, turnId)}>Edit</button>}
      {copyStatus && <span role="status">{copyStatus}</span>}
    </div>
  }

  const fileActions = (item: AgentItem) => item.type === 'fileChange' && onOpenFile && Array.isArray(item.changes)
    ? <div className="chat-file-actions">{item.changes.map((change, index) => {
        const firstHunk = typeof change.diff === 'string' ? change.diff.match(/^@@ -\d+(?:,\d+)? \+(\d+)/m) : null
        return <button type="button" key={`${change.path}:${index}`} onClick={() => onOpenFile(change.path, firstHunk ? Number(firstHunk[1]) : undefined)}>Open {change.path}</button>
      })}</div>
    : null

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text)
      setCopyStatus('Copied')
      window.setTimeout(() => setCopyStatus(''), 1400)
    } catch {
      setCopyStatus('Copy unavailable')
      window.setTimeout(() => setCopyStatus(''), 1800)
    }
  }

  return <div className="chat-transcript" ref={root} onScroll={onScroll} aria-label="Conversation transcript" tabIndex={0}>
    {turns.length > 1 && currentTurnIndex >= 0 && <nav className="chat-turn-navigation" aria-label="Conversation turns">
      <button type="button" aria-label="Previous turn" disabled={currentTurnIndex <= 0} onClick={() => navigateTurn(-1)}>↑</button>
      <span>Turn {currentTurnIndex + 1} of {turns.length}</span>
      <button type="button" aria-label="Next turn" disabled={currentTurnIndex >= turns.length - 1} onClick={() => navigateTurn(1)}>↓</button>
    </nav>}
    <div className="messages">
      {entries.map(entry => entry.kind === 'work'
        ? <details key={`work:${entry.id}`} className="chat-work-summary" data-transcript-item={entry.itemIds[0]}
            open={openWork[entry.id] ?? (['inProgress', 'waiting', 'unknown'].includes(entry.status) || entry.items.some(item => {
              const active = document.activeElement
              return active instanceof HTMLElement && active.closest<HTMLElement>('[data-transcript-item]')?.dataset.transcriptItem === item.id
            }))}
            onToggle={event => {
              const isOpen = event.currentTarget.open
              setOpenWork(previous => ({ ...previous, [entry.id]: isOpen }))
            }}>
            <summary>{entry.status === 'completed' ? 'Completed work'
              : entry.status === 'failed' ? 'Failed work'
                : entry.status === 'stopped' ? 'Stopped work'
                  : entry.status === 'waiting' ? 'Waiting for input'
                    : entry.status === 'inProgress' ? 'Work in progress'
                      : 'Work status unavailable'} · {workSummary(entry.items)}</summary>
            {entry.items.map(item => <div className="chat-transcript-item" key={item.id} data-transcript-item={item.id}>{renderItem(item)}{fileActions(item)}</div>)}
          </details>
        : <article key={entry.item.id} className="chat-transcript-item" data-transcript-item={entry.item.id} data-turn-id={typeof entry.item.turnId === 'string' ? entry.item.turnId : undefined}>
            {renderItem(entry.item)}
            {fileActions(entry.item)}
            {(entry.item.type === 'userMessage' || entry.item.type === 'agentMessage') && messageActions(entry.item)}
            {entry.item.type === 'agentMessage' && entry.item.turnStatus === 'completed' && typeof entry.item.turnId === 'string' && <div className="chat-branch-action"><button type="button" className="chat-branch-turn" disabled={actionsDisabled} title={actionsDisabled ? 'Wait for the current turn to finish before branching.' : undefined} onClick={() => onBranch(entry.item.turnId as string)}>Branch from this turn</button><span className="chat-branch-scope">Uses current files</span></div>}
          </article>)}
      {afterItems}
    </div>
    {showLatest && <button type="button" className="chat-jump-latest" onClick={jumpLatest}>Jump to latest</button>}
  </div>
}
