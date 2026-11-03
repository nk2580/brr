import { useCallback, useEffect, useMemo, useRef, useState, type DragEvent } from 'react'
import { Archive, ArrowDown, ArrowUp, ChevronRight, Plus, RotateCcw, TicketCheck, X } from 'lucide-react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { client } from '../lib/client'
import { ticketConflict, ticketLabel, ticketOperationId, ticketProposalProjectSnapshot, tickets, type Ticket, type TicketActivity, type TicketAgent, type TicketChanges, type TicketComment, type TicketPriority, type TicketProject, type TicketRun, type TicketRunProposal, type TicketStatus } from '../lib/tickets'
import { Button } from './ui/button'
import './TicketsPanel.css'

type Props = { onClose: () => void; online?: boolean; initialProjectId?: string; onOpenAgents?: () => void; hidden?: boolean }
const columns: { id: TicketStatus; label: string }[] = [
  { id: 'todo', label: 'Todo' }, { id: 'in_progress', label: 'In progress' }, { id: 'review', label: 'Review' }, { id: 'complete', label: 'Complete' },
]
const priorities: TicketPriority[] = ['none', 'low', 'medium', 'high', 'urgent']
const message = (reason: unknown) => reason instanceof Error ? reason.message : String(reason)
const time = (stamp: number) => new Date(stamp < 10_000_000_000 ? stamp * 1000 : stamp).toLocaleString()
type TicketLocalDraft = { description?: string; title?: string; criteria?: Ticket['acceptanceCriteria'] }

export function TicketsPanel({ onClose, online = true, initialProjectId, onOpenAgents, hidden = false }: Props) {
  const [projects, setProjects] = useState<TicketProject[]>([])
  const [agents, setAgents] = useState<TicketAgent[]>([])
  const [projectId, setProjectId] = useState(initialProjectId ?? '')
  const [items, setItems] = useState<Ticket[]>([])
  const [selectedId, setSelectedId] = useState('')
  const [comments, setComments] = useState<TicketComment[]>([])
  const [activity, setActivity] = useState<TicketActivity[]>([])
  const [commentsCursor, setCommentsCursor] = useState<number | null>(null)
  const [activityCursor, setActivityCursor] = useState<number | null>(null)
  const seenCommentCursors = useRef(new Set<number>())
  const seenActivityCursors = useRef(new Set<number>())
  const [runs, setRuns] = useState<TicketRun[]>([])
  const [proposal, setProposal] = useState<TicketRunProposal | null>(null)
  const [draftTitle, setDraftTitle] = useState('')
  const [description, setDescription] = useState('')
  const [titleDraft, setTitleDraft] = useState('')
  const [criterionDraft, setCriterionDraft] = useState('')
  const [criteriaDraft, setCriteriaDraft] = useState<Ticket['acceptanceCriteria'] | null>(null)
  const [runFolders, setRunFolders] = useState<Record<string, string>>({})
  const [ticketDrafts, setTicketDrafts] = useState<Record<string, TicketLocalDraft>>({})
  const [comment, setComment] = useState('')
  const [search, setSearch] = useState('')
  const [agentFilter, setAgentFilter] = useState('')
  const [priorityFilter, setPriorityFilter] = useState<TicketPriority | ''>('')
  const [showArchived, setShowArchived] = useState(false)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState('')
  const [currentRemote, setCurrentRemote] = useState<Ticket | null>(null)
  const [draggedId, setDraggedId] = useState('')
  const listGeneration = useRef(0)
  const detailGeneration = useRef(0)
  const projectIdRef = useRef(projectId)
  const selectedIdRef = useRef(selectedId)
  const draftsRef = useRef(ticketDrafts)
  const itemsRef = useRef(items)
  projectIdRef.current = projectId
  selectedIdRef.current = selectedId
  draftsRef.current = ticketDrafts
  itemsRef.current = items
  const detailCloseButton = useRef<HTMLButtonElement>(null)
  const detailPreviousFocus = useRef<HTMLElement | null>(null)
  const operationIds = useRef(new Map<string, string>())
  const localTicketOverlay = useRef(new Map<string, Ticket>())
  const rememberTicket = (item: Ticket) => localTicketOverlay.current.set(`${item.projectId}:${item.id}`, item)
  const operation = (key: string) => {
    const existing = operationIds.current.get(key)
    if (existing) return existing
    const id = ticketOperationId()
    operationIds.current.set(key, id)
    return id
  }
  const finishOperation = (key: string) => operationIds.current.delete(key)
  const updateDraft = (id: string, field: keyof TicketLocalDraft, value: TicketLocalDraft[keyof TicketLocalDraft]) => setTicketDrafts(current => ({ ...current, [id]: { ...current[id], [field]: value } }))
  const clearDraft = (id: string, field: keyof TicketLocalDraft) => setTicketDrafts(current => {
    const next = { ...current, [id]: { ...current[id] } }
    delete next[id][field]
    if (!Object.keys(next[id]).length) delete next[id]
    return next
  })
  const selected = items.find(item => item.id === selectedId) ?? null
  const selectedProject = selected ? projects.find(project => project.id === selected.projectId) : undefined
  const reviewedProjectSnapshot = proposal ? ticketProposalProjectSnapshot(proposal.snapshot.context) : null

  const refresh = useCallback(async (capturedProject = projectId, keepSelection = true) => {
    const generation = ++listGeneration.current
    if (!capturedProject) { setItems([]); setSelectedId(''); return }
    const rows: Ticket[] = []
    const cursors = new Set<string>()
    let cursor: string | undefined
    for (let count = 0; count < 100; count += 1) {
      const page = await tickets.list({ projectId: capturedProject, limit: 100, ...(cursor ? { cursor } : {}), archived: showArchived || undefined, ...(agentFilter ? { agentId: agentFilter } : {}), ...(priorityFilter ? { priority: priorityFilter } : {}) })
      rows.push(...page.data)
      const next = page.nextCursor ?? undefined
      if (!next || cursors.has(next)) break
      cursors.add(next)
      cursor = next
    }
    if (generation !== listGeneration.current) return
    if (projectIdRef.current !== capturedProject) return
    // A cursor may repeat rows when the host fills a short final page. Keep
    // one card per ticket so duplicate responses never create duplicate React
    // keys or inflate the column counts.
    const uniqueRows = [...new Map(rows.map(row => [row.id, row])).values()]
    const mergedRows = new Map(uniqueRows.map(row => [row.id, row]))
    for (const [key, local] of localTicketOverlay.current) {
      if (local.projectId !== capturedProject) continue
      const remote = mergedRows.get(local.id)
      if (!remote || remote.revision < local.revision) mergedRows.set(local.id, local)
      else localTicketOverlay.current.delete(key)
    }
    const finalRows = [...mergedRows.values()].filter(item => item.archived === showArchived && (!agentFilter || item.agentId === agentFilter) && (!priorityFilter || item.priority === priorityFilter))
    setItems(finalRows)
    setSelectedId(current => keepSelection && finalRows.some(item => item.id === current) ? current : '')
  }, [projectId, showArchived, agentFilter, priorityFilter])

  const refreshDetail = useCallback(async (ticketId: string, merge = false) => {
    const generation = ++detailGeneration.current
    const [detail, proposals] = await Promise.all([tickets.read(ticketId), tickets.proposals(ticketId)])
    if (generation !== detailGeneration.current || selectedIdRef.current !== ticketId) return
    const current = itemsRef.current.find(item => item.id === ticketId)
    const local = localTicketOverlay.current.get(`${detail.ticket.projectId}:${ticketId}`)
    if (Math.max(current?.revision ?? 0, local?.revision ?? 0) > detail.ticket.revision) return
    if (merge && current && detail.ticket.revision > current.revision && draftsRef.current[ticketId]) {
      setCurrentRemote(detail.ticket)
      setError('This ticket changed elsewhere. Your edits are still here; review the current version before saving again.')
    }
    setItems(previous => previous.map(item => item.id === ticketId ? detail.ticket : item))
    const mergeRecords = <T extends { id: string; createdAt: number }>(older: T[], newer: T[]) => [...new Map([...older, ...newer].map(item => [item.id, item])).values()].sort((a, b) => b.createdAt - a.createdAt)
    setComments(previous => merge ? mergeRecords(previous, detail.comments.data) : detail.comments.data)
    setActivity(previous => merge ? mergeRecords(previous, detail.activity.data) : detail.activity.data)
    setCommentsCursor(detail.comments.nextCursor ?? null)
    setActivityCursor(detail.activity.nextCursor ?? null)
    setRuns(detail.runs)
    if (!merge || !Object.prototype.hasOwnProperty.call(draftsRef.current[ticketId] ?? {}, 'description')) setDescription(draftsRef.current[ticketId]?.description ?? detail.ticket.description)
    if (!merge || !Object.prototype.hasOwnProperty.call(draftsRef.current[ticketId] ?? {}, 'title')) setTitleDraft(draftsRef.current[ticketId]?.title ?? detail.ticket.title)
    if (!merge || !Object.prototype.hasOwnProperty.call(draftsRef.current[ticketId] ?? {}, 'criteria')) setCriteriaDraft(draftsRef.current[ticketId]?.criteria ?? detail.ticket.acceptanceCriteria)
    setProposal(proposals.data.filter(item => ['proposed', 'pending'].includes(item.status) && item.ticketRevision === detail.ticket.revision).at(-1) ?? null)
  }, [])

  useEffect(() => {
    let active = true
    void Promise.all([tickets.projects(), tickets.agents()]).then(([projectResult, agentResult]) => {
      if (!active) return
      const nextProjects = projectResult.data.filter(project => !project.hidden)
      setProjects(nextProjects)
      setAgents(agentResult.data)
      setProjectId(current => (initialProjectId && nextProjects.some(project => project.id === initialProjectId) ? initialProjectId : current && nextProjects.some(project => project.id === current) ? current : nextProjects[0]?.id ?? ''))
    }).catch(reason => { if (active) setError(message(reason)) })
    return () => { active = false }
  }, [initialProjectId])

  useEffect(() => {
    if (initialProjectId && projects.some(project => project.id === initialProjectId)) setProjectId(initialProjectId)
  }, [initialProjectId, projects])

  useEffect(() => { void refresh(projectId, false).catch(reason => setError(message(reason))) }, [projectId, showArchived, agentFilter, priorityFilter, refresh])
  useEffect(() => {
    if (hidden) return
    let disposed = false
    let unlisten: (() => void) | undefined
    void client.subscribe(event => { if (event.method === 'brr/tickets/changed') { void refresh(projectId).catch(reason => setError(message(reason))); if (selectedIdRef.current) void refreshDetail(selectedIdRef.current, true).catch(reason => setError(message(reason))) } })
      .then(dispose => { if (disposed) dispose(); else unlisten = dispose }).catch(reason => setError(message(reason)))
    const focus = () => { void refresh(projectId).catch(reason => setError(message(reason))); if (selectedIdRef.current) void refreshDetail(selectedIdRef.current, true).catch(reason => setError(message(reason))) }
    window.addEventListener('focus', focus)
    return () => { disposed = true; unlisten?.(); window.removeEventListener('focus', focus) }
  }, [hidden, projectId, refresh, refreshDetail])

  useEffect(() => {
    if (!selectedId) { detailGeneration.current += 1; seenCommentCursors.current.clear(); seenActivityCursors.current.clear(); setComments([]); setActivity([]); setCommentsCursor(null); setActivityCursor(null); setRuns([]); setProposal(null); setCurrentRemote(null); return }
    void refreshDetail(selectedId).catch(reason => setError(message(reason)))
  }, [selectedId, refreshDetail])

  useEffect(() => {
    if (!selectedId || hidden) return
    const handleKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { event.preventDefault(); closeDetail(); return }
      if (event.key !== 'Tab') return
      const dialog = document.querySelector<HTMLElement>('.ticket-detail[role="dialog"]')
      if (!dialog) return
      const controls = [...dialog.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex="0"]')]
      if (!controls.length) { event.preventDefault(); dialog.focus(); return }
      const first = controls[0], last = controls.at(-1)!
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus() }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus() }
    }
    window.addEventListener('keydown', handleKey)
    requestAnimationFrame(() => detailCloseButton.current?.focus())
    return () => window.removeEventListener('keydown', handleKey)
  }, [selectedId, hidden])

  const visibleItems = useMemo(() => items.filter(item => `${item.title} ${item.description} ${ticketLabel(item)}`.toLocaleLowerCase().includes(search.toLocaleLowerCase())), [items, search])
  const run = async (label: string, action: () => Promise<void>) => {
    setBusy(label); setError('')
    try { await action() } catch (reason) { setError(message(reason)) } finally { setBusy('') }
  }
  const create = (event: React.FormEvent) => {
    event.preventDefault()
    const title = draftTitle.trim()
    const capturedProject = projectId
    if (!title || !capturedProject) return
    void run('Creating ticket…', async () => {
      const key = `create:${capturedProject}:${title}`
      const created = await tickets.create({ projectId: capturedProject, title, operationId: operation(key) })
      finishOperation(key)
      if (capturedProject !== projectIdRef.current) return
      // A board page already in flight can describe the state before this
      // create. Fence it so its late response cannot erase the new card.
      listGeneration.current += 1
      rememberTicket(created)
      setItems(current => [created, ...current.filter(item => item.id !== created.id)])
      setSelectedId(created.id); setDraftTitle('')
    })
  }
  const selectTicket = (item: Ticket, target?: HTMLElement) => { if (target) detailPreviousFocus.current = target; setSelectedId(item.id); selectedIdRef.current = item.id; setCurrentRemote(null); setError('') }
  const closeDetail = () => { setSelectedId(''); selectedIdRef.current = ''; requestAnimationFrame(() => detailPreviousFocus.current?.focus()) }
  const syncConflict = (reason: unknown, keepEdit = false, ticketId?: string, capturedProject?: string) => {
    const latest = ticketConflict(reason)
    if (latest) {
      if (!capturedProject || projectIdRef.current === capturedProject) setItems(current => current.map(item => item.id === latest.id ? latest : item))
      const activeTicket = !ticketId || (selectedIdRef.current === ticketId && (!capturedProject || projectIdRef.current === capturedProject))
      if (activeTicket) {
        setCurrentRemote(latest)
        if (!keepEdit) setDescription(latest.description)
        setError('This ticket changed elsewhere. Your edit is still here; review the current version and save again.')
      }
      return
    }
    if (!ticketId || (selectedIdRef.current === ticketId && (!capturedProject || projectIdRef.current === capturedProject))) setError(message(reason))
  }
  const move = (item: Ticket, status: TicketStatus, beforeId?: string) => void run('Moving ticket…', async () => {
    const key = `move:${item.id}:${item.revision}:${status}:${beforeId ?? ''}`
    try {
      const updated = await tickets.move(item.id, item.revision, status, beforeId, operation(key))
      finishOperation(key)
      if (projectIdRef.current === item.projectId) { rememberTicket(updated); setItems(current => current.map(candidate => candidate.id === updated.id ? updated : candidate)) }
    } catch (reason) { syncConflict(reason, false, item.id, item.projectId) }
  })
  const saveDescription = () => {
    if (!selected) return
    const snapshot = selected
    const key = `update:${snapshot.id}:${snapshot.revision}:${JSON.stringify({ description })}`
    void run('Saving description…', async () => {
      try {
        const updated = await tickets.update(snapshot.id, snapshot.revision, { description }, operation(key))
        finishOperation(key)
        if (projectIdRef.current === snapshot.projectId) { rememberTicket(updated); setItems(current => current.map(item => item.id === updated.id ? updated : item)) }
        if (selectedIdRef.current === snapshot.id && projectIdRef.current === snapshot.projectId) {
          setCurrentRemote(null)
          if (draftsRef.current[snapshot.id]?.description === description) { setDescription(updated.description); clearDraft(snapshot.id, 'description') }
        }
      } catch (reason) { syncConflict(reason, true, snapshot.id, snapshot.projectId) }
    })
  }
  const saveField = (changes: TicketChanges) => {
    if (!selected) return
    const snapshot = selected
    const key = `update:${snapshot.id}:${snapshot.revision}:${JSON.stringify(changes)}`
    void run('Saving ticket…', async () => {
      try {
        const updated = await tickets.update(snapshot.id, snapshot.revision, changes, operation(key))
        finishOperation(key)
        if (projectIdRef.current === snapshot.projectId) { rememberTicket(updated); setItems(current => current.map(item => item.id === updated.id ? updated : item)) }
        if (selectedIdRef.current === snapshot.id && projectIdRef.current === snapshot.projectId) {
          if (changes.acceptanceCriteria && JSON.stringify(draftsRef.current[snapshot.id]?.criteria) === JSON.stringify(changes.acceptanceCriteria)) { setCriteriaDraft(updated.acceptanceCriteria); clearDraft(snapshot.id, 'criteria') }
          if (changes.title && draftsRef.current[snapshot.id]?.title === changes.title) { setTitleDraft(updated.title); clearDraft(snapshot.id, 'title') }
          if (changes.archived) { setSelectedId(''); selectedIdRef.current = '' }
        }
      } catch (reason) { syncConflict(reason, false, snapshot.id, snapshot.projectId) }
    })
  }
  const addComment = (event: React.FormEvent) => {
    event.preventDefault()
    const snapshot = selected
    const body = comment.trim()
    if (!snapshot || !body) return
    const key = `comment:${snapshot.id}:${body}`
    void run('Adding comment…', async () => { const created = await tickets.comment(snapshot.id, body, operation(key)); finishOperation(key); if (selectedIdRef.current === snapshot.id) { setComments(current => [...current, created]); setComment(current => current === body ? '' : current) } })
  }
  const loadOlderComments = () => {
    const snapshot = selected
    const cursor = commentsCursor
    if (!snapshot || cursor === null || seenCommentCursors.current.has(cursor)) return
    seenCommentCursors.current.add(cursor)
    void run('Loading comments…', async () => {
      const page = await tickets.read(snapshot.id, { commentsCursor: cursor })
      if (selectedIdRef.current !== snapshot.id) return
      setComments(current => [...new Map([...current, ...page.comments.data].map(item => [item.id, item])).values()].sort((a, b) => b.createdAt - a.createdAt))
      const next = page.comments.nextCursor ?? null
      setCommentsCursor(next !== null && seenCommentCursors.current.has(next) ? null : next)
    })
  }
  const loadOlderActivity = () => {
    const snapshot = selected
    const cursor = activityCursor
    if (!snapshot || cursor === null || seenActivityCursors.current.has(cursor)) return
    seenActivityCursors.current.add(cursor)
    void run('Loading activity…', async () => {
      const page = await tickets.read(snapshot.id, { activityCursor: cursor })
      if (selectedIdRef.current !== snapshot.id) return
      setActivity(current => [...new Map([...current, ...page.activity.data].map(item => [item.id, item])).values()].sort((a, b) => b.createdAt - a.createdAt))
      const next = page.activity.nextCursor ?? null
      setActivityCursor(next !== null && seenActivityCursors.current.has(next) ? null : next)
    })
  }
  const propose = () => {
    if (!selected) return
    const snapshot = selected
    const projectSnapshot = projects.find(project => project.id === snapshot.projectId)
    if (!projectSnapshot) { setError('The ticket project is unavailable.'); return }
    if (projectSnapshot.folders.length > 1 && !runFolders[snapshot.id]) { setError('Choose a project folder before proposing an agent run.'); return }
    if (!snapshot.agentId || !snapshot.description.trim() || !snapshot.acceptanceCriteria.length) { setError('Assign an agent and add a description and acceptance criterion before proposing a run.'); return }
    if (description !== snapshot.description || titleDraft.trim() !== snapshot.title || JSON.stringify(criteriaDraft) !== JSON.stringify(snapshot.acceptanceCriteria)) { setError('Save the title, description, and checklist before proposing a run.'); return }
    const folder = projectSnapshot.folders.length === 1 ? projectSnapshot.folders[0] : runFolders[snapshot.id]
    const key = `propose:${snapshot.id}:${snapshot.revision}:${snapshot.agentId}:${folder ?? 'private'}`
    void run('Preparing run for review…', async () => {
      const result = await tickets.proposeRun(snapshot.id, snapshot.revision, snapshot.agentId!, folder, operation(key))
      finishOperation(key)
      if (selectedIdRef.current === snapshot.id) setProposal(result)
    })
  }
  const dropOn = (event: DragEvent<HTMLElement>, status: TicketStatus) => {
    event.preventDefault()
    const id = event.dataTransfer.getData('text/ticket-id') || draggedId
    const item = items.find(candidate => candidate.id === id)
    if (item && item.status !== status) move(item, status)
    setDraggedId('')
  }
  const reorder = (item: Ticket, direction: -1 | 1) => {
    const siblings = items.filter(candidate => candidate.status === item.status).sort((a, b) => a.position - b.position)
    const index = siblings.findIndex(candidate => candidate.id === item.id)
    if (direction < 0 && index > 0) move(item, item.status, siblings[index - 1].id)
    if (direction > 0 && index >= 0 && index < siblings.length - 1) move(item, item.status, siblings[index + 2]?.id)
  }
  const useCurrent = () => {
    if (!currentRemote || currentRemote.id !== selectedIdRef.current || currentRemote.projectId !== projectIdRef.current) return
    const latest = currentRemote
    setDescription(latest.description); setTitleDraft(latest.title); setCriteriaDraft(latest.acceptanceCriteria)
    clearDraft(latest.id, 'description'); clearDraft(latest.id, 'title'); clearDraft(latest.id, 'criteria')
    setCurrentRemote(null); setError('Current ticket loaded.')
  }
  const toggleArchive = () => {
    if (!selected) return
    const snapshot = selected
    const archived = !snapshot.archived
    const key = `update:${snapshot.id}:${snapshot.revision}:${JSON.stringify({ archived })}`
    void run(archived ? 'Archiving ticket…' : 'Restoring ticket…', async () => {
      const updated = await tickets.update(snapshot.id, snapshot.revision, { archived }, operation(key))
      finishOperation(key)
      if (projectIdRef.current === snapshot.projectId) setItems(current => archived ? current.filter(item => item.id !== updated.id) : current.map(item => item.id === updated.id ? updated : item))
      localTicketOverlay.current.delete(`${snapshot.projectId}:${updated.id}`)
      if (selectedIdRef.current === snapshot.id && projectIdRef.current === snapshot.projectId) { setSelectedId(''); selectedIdRef.current = '' }
      if (!archived) setShowArchived(false)
    })
  }
  const approveProposal = () => {
    if (!selected || !proposal) return
    const snapshot = selected
    const review = proposal
    const key = `approve:${snapshot.id}:${review.hash}`
    void run('Starting reviewed run…', async () => {
      await tickets.approveRun(snapshot.id, review.hash, operation(key))
      finishOperation(key)
      if (selectedIdRef.current === snapshot.id && projectIdRef.current === snapshot.projectId) { setProposal(null); onOpenAgents?.() }
    })
  }
  const rejectProposal = () => {
    if (!selected || !proposal) return
    const snapshot = selected
    const review = proposal
    const key = `reject:${snapshot.id}:${review.hash}`
    void run('Discarding proposal…', async () => {
      await tickets.rejectRun(snapshot.id, review.hash, operation(key))
      finishOperation(key)
      if (selectedIdRef.current === snapshot.id && projectIdRef.current === snapshot.projectId) setProposal(null)
    })
  }
  const saveTitle = () => { if (selected && titleDraft.trim() && titleDraft.trim() !== selected.title) saveField({ title: titleDraft.trim() }) }
  const changeCriteria = (next: Ticket['acceptanceCriteria']) => {
    if (!selected) return
    setCriteriaDraft(next); updateDraft(selected.id, 'criteria', next)
  }
  const addCriterion = () => {
    const text = criterionDraft.trim()
    if (!selected || !text) return
    const next = [...(criteriaDraft ?? selected.acceptanceCriteria), { id: crypto.randomUUID(), text, checked: false }]
    changeCriteria(next); setCriterionDraft('')
  }
  const saveCriteria = () => { if (selected && criteriaDraft) saveField({ acceptanceCriteria: criteriaDraft }) }

  return <section className="tickets-panel" aria-label="Tickets" hidden={hidden}>
    <aside id="tickets-sidebar" className="tickets-project-sidebar" aria-label="Ticket projects">
      <div className="tickets-sidebar-heading"><span>Projects</span><Button variant="ghost" size="icon" aria-label="Close tickets" onClick={onClose}><X size={15}/></Button></div>
      {projects.map(project => <button className="tickets-project-row" key={project.id} aria-current={projectId === project.id ? 'page' : undefined} onClick={() => { setProjectId(project.id); setSelectedId(''); selectedIdRef.current = ''; setCurrentRemote(null) }}><TicketCheck size={15}/><span>{project.name}</span><ChevronRight size={13}/></button>)}
      {!projects.length && <p className="tickets-project-empty">Open a project folder in Home to start tracking tickets.</p>}
    </aside>
    <div className="tickets-main">
      <header className="tickets-header"><div><p className="tickets-kicker">Project work</p><h1>Tickets</h1></div><span className="tickets-project-name">{projects.find(project => project.id === projectId)?.name ?? 'No project selected'}</span></header>
      {error && <div className="tickets-alert" role="alert">{error}{currentRemote && <Button size="sm" variant="outline" onClick={useCurrent}>Load current version</Button>}</div>}
      {busy && <p className="tickets-busy" role="status">{busy}</p>}
      <div className="tickets-toolbar">
        <form className="tickets-quick-create" onSubmit={create}><input aria-label="New ticket title" placeholder="Add a ticket…" value={draftTitle} onChange={event => setDraftTitle(event.target.value)} disabled={!projectId || showArchived}/><Button size="sm" type="submit" disabled={!draftTitle.trim() || !projectId || showArchived}><Plus size={14}/>Add ticket</Button></form>
        <input className="tickets-search" aria-label="Search tickets" placeholder="Search tickets" value={search} onChange={event => setSearch(event.target.value)}/>
        <select aria-label="Filter by agent" value={agentFilter} onChange={event => setAgentFilter(event.target.value)}><option value="">All agents</option>{agents.map(agent => <option key={agent.id} value={agent.id}>{agent.name}</option>)}</select>
        <select aria-label="Filter by priority" value={priorityFilter} onChange={event => setPriorityFilter(event.target.value as TicketPriority | '')}><option value="">All priorities</option>{priorities.map(value => <option key={value} value={value}>{value === 'none' ? 'No priority' : value}</option>)}</select>
        <Button size="sm" variant={showArchived ? 'secondary' : 'outline'} aria-pressed={showArchived} onClick={() => setShowArchived(value => !value)}>{showArchived ? <RotateCcw size={14}/> : <Archive size={14}/>} {showArchived ? 'Show active' : 'Archived'}</Button>
      </div>
      {!projectId ? <div className="tickets-empty"><h2>Select a project</h2><p>Tickets are organized by project.</p></div> : <div className="tickets-board" aria-label="Ticket board">
        {columns.map(column => {
          const inColumn = visibleItems.filter(item => item.status === column.id).sort((a, b) => a.position - b.position)
          return <section key={column.id} className="tickets-column" aria-label={column.label} onDragOver={event => event.preventDefault()} onDrop={event => dropOn(event, column.id)}>
            <header><h2>{column.label}</h2><span>{inColumn.length}</span></header>
            <div className="tickets-column-cards">{inColumn.map((item, index) => <article className={`ticket-card${draggedId === item.id ? ' is-dragging' : ''}`} key={item.id} draggable={!showArchived} onDragStart={event => { setDraggedId(item.id); event.dataTransfer.setData('text/ticket-id', item.id); event.dataTransfer.effectAllowed = 'move' }} onDragEnd={() => setDraggedId('')} onDragOver={event => event.preventDefault()} onDrop={event => { event.preventDefault(); event.stopPropagation(); const movingId = event.dataTransfer.getData('text/ticket-id') || draggedId; const moving = items.find(candidate => candidate.id === movingId); if (moving && moving.id !== item.id) move(moving, column.id, item.id); setDraggedId('') }}>
              <button className="ticket-card-open" aria-label={`${ticketLabel(item)} ${item.title}`} aria-current={selectedId === item.id ? 'true' : undefined} onClick={event => selectTicket(item, event.currentTarget)}><span className="ticket-number">{ticketLabel(item)}</span><strong>{item.title}</strong><span className={`ticket-priority priority-${item.priority}`}>{item.priority === 'none' ? 'No priority' : item.priority}</span>{item.agentId && <small>{agents.find(agent => agent.id === item.agentId)?.name ?? item.agentId}</small>}{item.execution && <span className={`ticket-execution execution-${item.execution.status}`} aria-label={`Agent execution: ${item.execution.status}`}>Run · {item.execution.status}</span>}</button>
              <div className="ticket-order-controls"><Button variant="ghost" size="icon" aria-label={`Move ${ticketLabel(item)} up`} disabled={showArchived || index === 0} onClick={() => reorder(item, -1)}><ArrowUp size={12}/></Button><Button variant="ghost" size="icon" aria-label={`Move ${ticketLabel(item)} down`} disabled={showArchived || index === inColumn.length - 1} onClick={() => reorder(item, 1)}><ArrowDown size={12}/></Button></div>
              <label className="ticket-move-control"><span className="sr-only">Move {ticketLabel(item)}</span><select aria-label={`Move ${ticketLabel(item)}`} value={item.status} onChange={event => move(item, event.target.value as TicketStatus)} disabled={showArchived}>{columns.map(option => <option key={option.id} value={option.id}>{option.label}</option>)}</select></label>
            </article>)}</div>
            {!inColumn.length && <p className="tickets-column-empty">Drop tickets here</p>}
          </section>
        })}
      </div>}
      {selected && <div className="ticket-detail-backdrop" onMouseDown={event => { if (event.target === event.currentTarget) closeDetail() }}><aside className="ticket-detail" role="dialog" aria-label="Ticket details" aria-modal="true" tabIndex={-1}>
        <header className="ticket-detail-header"><div><span className="ticket-number">{ticketLabel(selected)}</span><label className="ticket-title-field"><span className="sr-only">Ticket title</span><input aria-label="Ticket title" value={titleDraft} onChange={event => { setTitleDraft(event.target.value); updateDraft(selected.id, 'title', event.target.value) }} onBlur={saveTitle} onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); saveTitle() } }} disabled={selected.archived}/></label></div><Button size="sm" variant="outline" disabled={!!busy} onClick={toggleArchive}>{selected.archived ? 'Restore' : 'Archive'}</Button><Button ref={detailCloseButton} variant="ghost" size="icon" aria-label="Close ticket details" onClick={closeDetail}><X size={15}/></Button></header>
        {currentRemote && <div className="ticket-remote-version"><strong>Current version · revision {currentRemote.revision}</strong><p>{currentRemote.title}</p></div>}
        <div className="ticket-detail-fields"><label>Status<select aria-label="Status" value={selected.status} onChange={event => move(selected, event.target.value as TicketStatus)} disabled={selected.archived}>{columns.map(option => <option key={option.id} value={option.id}>{option.label}</option>)}</select></label>
          <label>Priority<select aria-label="Priority" value={selected.priority} onChange={event => saveField({ priority: event.target.value as TicketPriority })} disabled={selected.archived}>{priorities.map(value => <option key={value} value={value}>{value === 'none' ? 'No priority' : value}</option>)}</select></label>
          <label>Assigned agent<select aria-label="Assigned agent" value={selected.agentId ?? ''} onChange={event => saveField({ agentId: event.target.value || null })} disabled={selected.archived}><option value="">Unassigned</option>{agents.map(agent => <option key={agent.id} value={agent.id}>{agent.name}</option>)}</select></label>
        </div>
        <label className="ticket-description-field">Description<textarea aria-label="Description" value={description} onChange={event => { setDescription(event.target.value); updateDraft(selected.id, 'description', event.target.value) }} rows={5} disabled={selected.archived}/></label>
        <Button size="sm" onClick={saveDescription} disabled={selected.archived || description === selected.description || !!busy}>Save description</Button>
        <section className="ticket-criteria" aria-label="Acceptance criteria"><div className="ticket-section-heading"><h3>Acceptance criteria</h3><Button size="sm" variant="outline" disabled={selected.archived || !!busy || !criteriaDraft} onClick={saveCriteria}>Save checklist</Button></div>{(criteriaDraft ?? selected.acceptanceCriteria).map((criterion, index) => <div className="ticket-criterion-row" key={criterion.id}><input type="checkbox" aria-label={`Complete criterion ${index + 1}`} checked={criterion.checked} disabled={selected.archived || !!busy} onChange={event => changeCriteria((criteriaDraft ?? selected.acceptanceCriteria).map((item, i) => i === index ? { ...item, checked: event.target.checked } : item))}/><input aria-label={`Criterion ${index + 1}`} value={criterion.text} disabled={selected.archived || !!busy} onChange={event => changeCriteria((criteriaDraft ?? selected.acceptanceCriteria).map((item, i) => i === index ? { ...item, text: event.target.value } : item))}/><Button variant="ghost" size="icon" aria-label={`Remove criterion ${index + 1}`} disabled={selected.archived || !!busy} onClick={() => changeCriteria((criteriaDraft ?? selected.acceptanceCriteria).filter((_, i) => i !== index))}><X size={12}/></Button></div>)}<form className="ticket-add-criterion" onSubmit={event => { event.preventDefault(); addCriterion() }}><input aria-label="New acceptance criterion" placeholder="Add acceptance criterion" value={criterionDraft} onChange={event => setCriterionDraft(event.target.value)} disabled={selected.archived}/><Button size="sm" type="submit" disabled={!criterionDraft.trim() || selected.archived}><Plus size={13}/>Add</Button></form>{!(criteriaDraft ?? selected.acceptanceCriteria).length && <p className="tickets-muted">Add a criterion before proposing an agent run.</p>}</section>
        <div className="ticket-run-actions">
          {selectedProject && selectedProject.folders.length > 1 && <label>Run folder<select aria-label="Run folder" value={runFolders[selected.id] ?? ''} onChange={event => setRunFolders(current => ({ ...current, [selected.id]: event.target.value }))}><option value="">Choose a registered folder</option>{selectedProject.folders.map(folder => <option key={folder} value={folder}>{folder}</option>)}</select></label>}
          {selectedProject?.folders.length === 1 && <span>Run folder · {selectedProject.folders[0]}</span>}
          {selectedProject?.folders.length === 0 && <span>No folders attached · private workspace</span>}
          <Button size="sm" onClick={propose} disabled={selected.archived || !!busy || !selectedProject || (selectedProject.folders.length > 1 && !runFolders[selected.id])}>Propose agent run</Button>
          <span>{!selectedProject ? 'Project details are unavailable.' : selectedProject.folders.length > 1 && !runFolders[selected.id] ? 'Choose a project folder before proposing a run.' : 'Requires an assigned agent, description, and criterion.'}</span>
        </div>
        {proposal && <section className="ticket-run-review" aria-label="Review proposed agent run"><h3>Review exact run request</h3><dl><dt>Agent</dt><dd>{agents.find(agent => agent.id === proposal.snapshot.agentId)?.name ?? proposal.snapshot.agentId}</dd><dt>Task</dt><dd>{proposal.snapshot.title}</dd><dt>Project</dt><dd>{projects.find(project => project.id === proposal.snapshot.projectId)?.name ?? 'Project'} · {proposal.snapshot.projectId ?? selected.projectId}</dd><dt>Run folder</dt><dd>{proposal.snapshot.cwd ?? (reviewedProjectSnapshot?.folders.length === 0 ? 'No folders attached · private workspace' : 'Not specified')}</dd><dt>Reviewed project folders</dt><dd>{reviewedProjectSnapshot?.folders.length ? <ul className="ticket-reviewed-roots" aria-label="Reviewed project folders">{reviewedProjectSnapshot.folders.map(folder => <li key={folder}>{folder}</li>)}</ul> : reviewedProjectSnapshot ? <span>No folders attached · private workspace</span> : <span>Not recorded in this proposal</span>}</dd><dt>Workspace</dt><dd>{proposal.snapshot.workspacePermission ?? 'Not specified'}</dd><dt>Model</dt><dd>{proposal.snapshot.model ?? 'Default'}</dd><dt>Reasoning</dt><dd>{proposal.snapshot.effort ?? 'Default'}</dd><dt>Instructions</dt><dd><ReactMarkdown remarkPlugins={[remarkGfm]}>{proposal.snapshot.instructions}</ReactMarkdown></dd><dt>Accept when</dt><dd><ul>{proposal.snapshot.acceptanceCriteria.map((criterion, index) => <li key={index}>{criterion}</li>)}</ul></dd></dl><Button size="sm" disabled={!online || !!busy} onClick={approveProposal}>Approve and run</Button><Button size="sm" variant="outline" disabled={!!busy} onClick={rejectProposal}>Reject</Button></section>}
        <section className="ticket-comments" aria-label="Comments"><h3>Comments</h3><form onSubmit={addComment}><textarea aria-label="Comment" placeholder="Add a comment" rows={2} value={comment} onChange={event => setComment(event.target.value)}/><Button size="sm" type="submit" disabled={!comment.trim() || !!busy}>Comment</Button></form>{comments.map(item => <article key={item.id}><p>{item.body}</p><small>{item.actor.kind} · {time(item.createdAt)}</small></article>)}{commentsCursor !== null && <Button size="sm" variant="outline" disabled={!!busy} onClick={loadOlderComments}>Load older comments</Button>}</section>
        <section className="ticket-activity" aria-label="Ticket activity"><h3>Activity</h3>{activity.map(item => <p key={item.id}><span>{item.kind.replaceAll('_', ' ')}</span><small>{time(item.createdAt)}</small></p>)}{activityCursor !== null && <Button size="sm" variant="outline" disabled={!!busy} onClick={loadOlderActivity}>Load older activity</Button>}{!activity.length && <p className="tickets-muted">Changes will appear here.</p>}</section>
        <section className="ticket-runs" aria-label="Agent runs"><h3>Agent runs</h3>{runs.map(run => <div className="ticket-run" key={run.taskId}><button onClick={() => onOpenAgents?.()}><strong>{run.status}</strong><span>{run.result || run.taskId}</span></button>{run.evidence?.length ? <ul aria-label={`Evidence for ${run.taskId}`}>{run.evidence.map((item, index) => <li key={`${run.taskId}-${index}`}>{item}</li>)}</ul> : null}</div>)}{!runs.length && !selected.execution && <p className="tickets-muted">No agent runs yet.</p>}{selected.execution && !runs.some(run => run.taskId === selected.execution?.taskId) && <div className="ticket-run"><button onClick={() => onOpenAgents?.()}><strong>{selected.execution.status}</strong><span>{selected.execution.result || selected.execution.taskId}</span></button>{selected.execution.evidence?.length ? <ul aria-label={`Evidence for ${selected.execution.taskId}`}>{selected.execution.evidence.map((item, index) => <li key={`${selected.execution?.taskId}-${index}`}>{item}</li>)}</ul> : null}</div>}</section>
      </aside></div>}
    </div>
  </section>
}
