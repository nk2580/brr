import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from 'react'
import { Activity, Bot, Check, ChevronLeft, CircleAlert, CircleHelp, Clock3, CloudOff, Code2, FileClock, FolderSync, GitMerge, MessageSquare, MoreHorizontal, Plus, RefreshCw, ScanLine, Send, ShieldCheck, Sparkles, Trash2, Users, Wifi, X } from 'lucide-react'
import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'
import { cancel as cancelBarcodeScan, Format, scan as scanBarcode, type ScanOptions } from '@tauri-apps/plugin-barcode-scanner'
import { Logo } from '../../../src/components/Logo'
import { QuestionForm } from '../../../src/components/QuestionForm'
import { ThreadAttachments } from './ThreadAttachments'
import type { QuestionAnswers, QuestionRequest } from '../../../src/lib/questions'
import { hydrateThread, reduceSession, type RpcEvent, type SessionState } from '../../../src/lib/session'
import { createOperationId, parseInvitation, PendingMutationError, RemoteClient, type ApiError, type Channel, type EventReplay, type PairInvitation, type PendingMutation, type Profile, type RemoteEvent, type Reply, type Snapshot } from '@brr/remote-client'

type Row = Record<string, unknown>
type Bridge = ConstructorParameters<typeof RemoteClient>[0]
type Scanner = { scan: (options?: ScanOptions) => Promise<{ content: string }>; cancel: () => Promise<void> }
type Props = { client?: RemoteClient; native?: boolean; scanner?: Scanner }
type Tab = 'chats' | 'agents' | 'attention' | 'fleet'
type AgentPane = 'chat' | 'tasks' | 'memory' | 'skills' | 'profile'
type Notice = { kind: 'error' | 'success' | 'info'; message: string; connection?: true }

const tauriBridge: Bridge = {
  invoke: <T,>(command: string, args?: Record<string, unknown>) => invoke<T>(command, args),
  listen: <T,>(event: string, handler: (payload: T) => void) => listen<T>(event, (message) => handler(message.payload)),
}
const nativeClient = new RemoteClient(tauriBridge)

function nativeAvailable() { return '__TAURI_INTERNALS__' in window }
function asRows(value: unknown): Row[] { return Array.isArray(value) ? value.filter(isRow) : [] }
function isRow(value: unknown): value is Row { return Boolean(value && typeof value === 'object' && !Array.isArray(value)) }
function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(',')}]`
  if (isRow(value)) return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${stableJson(value[key])}`).join(',')}}`
  return JSON.stringify(value) ?? 'null'
}
function isDelegationProposal(row: Row): boolean {
  return text(row, 'status') === 'proposed' && Boolean(text(row, 'parentTaskId')) && Boolean(text(row, 'hash')) && Array.isArray(row.tasks)
}
function isReviewableProfileDraft(row: Row): boolean {
  return ['proposed', 'approved'].includes(text(row, 'status')) && Boolean(text(row, 'hash'))
}
function profileCandidateFromRecord(value: unknown): Row {
  if (!isRow(value)) return {}
  const content = isRow(value.content) ? value.content : {}
  const data = isRow(content.data) ? content.data : {}
  const candidate = isRow(data.candidate) ? data.candidate : isRow(content.candidate) ? content.candidate : {}
  return Object.fromEntries(['name', 'description', 'instructions', 'soul', 'model', 'effort'].map(key => [key, text(candidate, key)]))
}
function conflictTableLabel(value: string) {
  return value.replace(/([a-z0-9])([A-Z])/g, '$1 $2').replace(/[_-]/g, ' ').replace(/^./, first => first.toUpperCase())
}
function text(row: Row, key: string, fallback = '') { return typeof row[key] === 'string' ? row[key] as string : fallback }
function number(row: Row, key: string, fallback = 0) { return typeof row[key] === 'number' ? row[key] as number : fallback }
function revisionOptions(row: Row, key = 'revision'): { expectedRevision?: string } {
  const value = row[key]
  if (typeof value === 'string' && value.trim()) return { expectedRevision: value }
  if (typeof value === 'number' && Number.isFinite(value)) return { expectedRevision: String(value) }
  return {}
}
function owner(row: Row, primary: string) { return text(row, 'ownerNodeId', primary) }
function chatDraftKey(profileId: string, ownerId: string, threadId: string) { return `draft:chat:${profileId}:${ownerId}:${threadId || 'new'}` }
function definitionDraftsKey(profileId: string) { return `pending:definitions:${profileId}` }
function nodeId(row: Row) { return text(row, 'nodeId', text(row, 'id')) }
function nodeIsAvailable(id: string, nodes: Row[]) {
  const node = nodes.find(item => nodeId(item) === id)
  return Boolean(id && node && node.online !== false && node.removed !== true)
}
function compactNodeId(id: string) { return id.length > 24 ? `${id.slice(0, 10)}…${id.slice(-7)}` : id || 'Unknown node' }
function nodeLabel(id: string, nodes: Row[]) {
  const node = nodes.find(item => nodeId(item) === id)
  return (node && text(node, 'name').trim()) || compactNodeId(id)
}
function availableAgentNodeIds(agent: Row, nodes: Row[], primary: string) {
  const declared = agent.availableNodeIds
  const eligible = Array.isArray(declared)
    ? declared.filter((id): id is string => typeof id === 'string' && id.length > 0)
    : [text(agent, 'originNodeId', owner(agent, primary))]
  return [...new Set(eligible)].filter(id => nodeIsAvailable(id, nodes))
}
function threadIsHistorical(thread: Row) { return thread.historical === true }
function nodeEpoch(row: Row, snapshot: Snapshot | null) {
  const nodeId = owner(row, snapshot?.nodeId ?? '')
  const node = asRows(snapshot?.nodes).find(item => text(item, 'nodeId', text(item, 'id')) === nodeId)
  return text(node ?? {}, 'epoch')
}
function hydrateRow(thread: Row) { return hydrateThread({ ...thread, id: text(thread, 'id') }) }
function liveTurn(row: Row | null) {
  if (!row) return null
  const explicit = isRow(row.currentTurn) ? row.currentTurn : isRow(row.turn) ? row.turn : null
  const turns = asRows(row.turns ?? row.items).filter(item => item.type === 'turn')
  const candidate = explicit ?? turns.at(-1) ?? null
  const id = text(candidate ?? {}, 'id', text(row, 'activeTurnId'))
  const status = text(candidate ?? {}, 'status', text(row, 'turnStatus', text(row, 'status'))).replace(/[_\s-]/g, '').toLowerCase()
  return id && ['inprogress', 'running', 'started'].includes(status) ? { ...candidate, id } as Row : null
}
function labelFor(row: Row, ...keys: string[]) { for (const key of keys) if (text(row, key)) return text(row, key); return 'Untitled' }
function formatDate(value: unknown) {
  const date = typeof value === 'number' ? new Date(Math.abs(value) < 100_000_000_000 ? value * 1000 : value) : typeof value === 'string' ? new Date(value) : null
  return date && !Number.isNaN(date.valueOf()) ? new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' }).format(date) : ''
}
function responseResult<T>(reply: { result?: T; error?: ApiError }): T {
  if (reply.error) throw new Error(reply.error.message)
  if (reply.result === undefined) throw new Error('The node returned an empty response.')
  return reply.result
}

const snapshotRefreshMethods = new Set([
  'thread/started', 'thread/updated', 'thread/archived', 'thread/unarchived', 'thread/name/updated',
  'turn/completed', 'brr/questions/changed', 'brr/approvals/changed', 'brr/sync/changed',
  'brr/nodes/changed', 'brr/devices/changed', 'brr/fleet/changed', 'brr/agents/changed',
  'brr/tasks/changed', 'brr/tickets/changed', 'brr/chat/changed', 'item/commandExecution/requestApproval',
  'item/fileChange/requestApproval', 'item/permissions/requestApproval', 'serverRequest/resolved',
])
function needsSnapshotRefresh(event: RemoteEvent) {
  return snapshotRefreshMethods.has(text(event.message as Row, 'method'))
}

export function MobileApp({ client = nativeClient, native = nativeAvailable(), scanner = { scan: scanBarcode, cancel: cancelBarcodeScan } }: Props) {
  const [tab, setTab] = useState<Tab>('chats')
  const [profiles, setProfiles] = useState<Profile[]>([])
  const [profile, setProfile] = useState<Profile | null>(null)
  const profileRef = useRef<Profile | null>(null)
  profileRef.current = profile
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null)
  const [online, setOnline] = useState(false)
  const onlineRef = useRef(false)
  const setConnectionOnline = (value: boolean) => { onlineRef.current = value; setOnline(value) }
  const [activeThread, setActiveThread] = useState<Row | null>(null)
  const [sessionActionsOpen, setSessionActionsOpen] = useState(false)
  const [renamingThread, setRenamingThread] = useState(false)
  const [renameValue, setRenameValue] = useState('')
  const [confirmDeleteThread, setConfirmDeleteThread] = useState(false)
  const [olderTurnCursor, setOlderTurnCursor] = useState<string | null>(null)
  const [loadingOlderTurns, setLoadingOlderTurns] = useState(false)
  const [newSession, setNewSession] = useState(false)
  const [session, setSession] = useState<Row | null>(null)
  const [liveSession, setLiveSession] = useState<SessionState | null>(null)
  const [selectedAgent, setSelectedAgent] = useState<Row | null>(null)
  const [agentExecutionNodeId, setAgentExecutionNodeId] = useState('')
  const [agentPane, setAgentPane] = useState<AgentPane>('chat')
  const [rows, setRows] = useState<Row[]>([])
  const [taskNextCursor, setTaskNextCursor] = useState<string | null>(null)
  const [loadingMoreTasks, setLoadingMoreTasks] = useState(false)
  const [drafts, setDrafts] = useState<Row[]>([])
  const draftsRef = useRef<Row[]>([])
  draftsRef.current = drafts
  const definitionDraftWrites = useRef(new Map<string, Promise<void>>())
  const blockedDefinitionDraftProfiles = useRef(new Set<string>())
  const inFlightDefinitionMutations = useRef(new Map<string, number>())
  const definitionDraftOperationIds = useRef(new Map<string, { profileId: string; operationId: string }>())
  const [newProfileDrafts, setNewProfileDrafts] = useState<Row[]>([])
  const [busy, setBusy] = useState('')
  const [notice, setNotice] = useState<Notice | null>(null)
  const [search, setSearch] = useState('')
  const [invitation, setInvitation] = useState('')
  const [deviceName, setDeviceName] = useState('')
  const [pairing, setPairing] = useState(false)
  const [compose, setCompose] = useState('')
  const composeRef = useRef('')
  composeRef.current = compose
  const [chatDraftOperationId, setChatDraftOperationId] = useState('')
  const chatDraftOperationIdRef = useRef('')
  chatDraftOperationIdRef.current = chatDraftOperationId
  const [steering, setSteering] = useState(false)
  const [chatModel, setChatModel] = useState<string | null>(null)
  const [chatEffort, setChatEffort] = useState('medium')
  const [selectedProjectId, setSelectedProjectId] = useState('')
  const [ownerModels, setOwnerModels] = useState<Row[]>([])
  const [ownerModelsNodeId, setOwnerModelsNodeId] = useState('')
  const [ownerModelsLoading, setOwnerModelsLoading] = useState(false)
  const [ownerModelsUnavailable, setOwnerModelsUnavailable] = useState(false)
  const ownerModelsGeneration = useRef(0)
  const [chatMode, setChatMode] = useState<'default' | 'plan'>('default')
  const [chatAccess, setChatAccess] = useState<'workspace-write' | 'read-only'>('workspace-write')
  const [draftTask, setDraftTask] = useState({ title: '', instructions: '', acceptance: '' })
  const [taskSchedule, setTaskSchedule] = useState({ name: '', title: '', instructions: '', acceptance: '', cron: '0 9 * * 1', timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC' })
  const [memoryEdit, setMemoryEdit] = useState<Row | null>(null)
  const [forgottenMemoryEvent, setForgottenMemoryEvent] = useState('')
  const [pendingMutations, setPendingMutations] = useState<PendingMutation[]>([])
  const [selectedNodeId, setSelectedNodeId] = useState('')
  const scrollRef = useRef<HTMLDivElement>(null)
  const taskRowsRef = useRef<Row[]>([])
  const workflowRowsRef = useRef<Row[]>([])
  const proposalRowsRef = useRef<Row[]>([])
  const taskCursorHistory = useRef(new Set<string>())
  const taskListGeneration = useRef(0)
  const turnCursorHistory = useRef(new Set<string>())
  const snapshotRef = useRef<Snapshot | null>(null)
  const snapshotRequestGeneration = useRef(new Map<string, number>())
  const snapshotCacheWrites = useRef(new Map<string, Promise<void>>())
  const activeThreadRef = useRef<Row | null>(null)
  activeThreadRef.current = activeThread
  const chatSelectionGeneration = useRef(0)
  const invalidateChatSelection = () => {
    chatSelectionGeneration.current += 1
    setBusy('')
  }
  const primaryNodeId = profile?.nodeId ?? ''
  const agents = useMemo(() => asRows(snapshot?.agents), [snapshot])
  const threads = useMemo(() => asRows(snapshot?.threads), [snapshot])
  const questions = useMemo(() => asRows(snapshot?.questions), [snapshot])
  const approvals = useMemo(() => asRows(snapshot?.approvals), [snapshot])
  const nodes = useMemo(() => asRows(snapshot?.nodes), [snapshot])
  const effectiveAgentNodeId = agentExecutionNodeId || selectedNodeId || primaryNodeId
  const agentNodeOnline = online && nodeIsAvailable(effectiveAgentNodeId, nodes) && (!selectedAgent || availableAgentNodeIds(selectedAgent, nodes, primaryNodeId).includes(effectiveAgentNodeId))
  const newAgentNodeOnline = online && nodeIsAvailable(selectedNodeId, nodes)
  const activeThreadNodeId = activeThread ? owner(activeThread, primaryNodeId) : ''
  const activeThreadCanOperate = Boolean(activeThread && online && !threadIsHistorical(activeThread) && nodeIsAvailable(activeThreadNodeId, nodes))
  const currentChatDraftKey = profile ? chatDraftKey(profile.id, activeThread ? activeThreadNodeId : selectedNodeId, text(activeThread ?? {}, 'id')) : ''
  const currentChatDraftKeyRef = useRef(currentChatDraftKey)
  currentChatDraftKeyRef.current = currentChatDraftKey
  const lastProfileIdentity = useRef<string | null>(null)
  useEffect(() => {
    const nextProfileId = profile?.id ?? null
    if (lastProfileIdentity.current === nextProfileId) return
    const previousProfileId = lastProfileIdentity.current
    lastProfileIdentity.current = nextProfileId
    if (previousProfileId === null) return
    invalidateChatSelection()
    activeThreadRef.current = null
    setActiveThread(null)
    setSession(null)
    setLiveSession(null)
    setSessionActionsOpen(false)
    setRenamingThread(false)
    setRenameValue('')
    setConfirmDeleteThread(false)
    setNewSession(false)
    composeRef.current = ''
    chatDraftOperationIdRef.current = ''
    setCompose('')
    setChatDraftOperationId('')
    setSteering(false)
    setOlderTurnCursor(null)
    setLoadingOlderTurns(false)
    turnCursorHistory.current.clear()
    setSelectedNodeId('')
    setSelectedProjectId('')
    setChatModel(null)
    setChatMode('default')
    setChatAccess('workspace-write')
    setSelectedAgent(null)
    setAgentExecutionNodeId('')
    setAgentPane('chat')
    setRows([])
    setTaskNextCursor(null)
    setLoadingMoreTasks(false)
    taskRowsRef.current = []
    workflowRowsRef.current = []
    proposalRowsRef.current = []
    taskCursorHistory.current.clear()
    setNewProfileDrafts([])
    setDraftTask({ title: '', instructions: '', acceptance: '' })
    setTaskSchedule({ name: '', title: '', instructions: '', acceptance: '', cron: '0 9 * * 1', timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC' })
    setMemoryEdit(null)
    setForgottenMemoryEvent('')
    setDrafts([])
    setPendingMutations([])
    setOwnerModels([])
    setOwnerModelsNodeId('')
    setOwnerModelsUnavailable(false)
    setSnapshot(null)
    snapshotRef.current = null
    setConnectionOnline(false)
  }, [profile?.id])
  const filteredThreads = useMemo(() => threads.filter(row => labelFor(row, 'title', 'preview').toLocaleLowerCase().includes(search.toLocaleLowerCase())), [threads, search])
  const activeTurn = liveSession
    ? liveSession.status === 'running' && liveSession.turnId ? { id: liveSession.turnId, status: 'inProgress' } : null
    : liveTurn(session ?? activeThread)
  const modelOwnerNodeId = activeThread ? activeThreadNodeId : selectedNodeId
  const ownerModelsRequested = Boolean(activeThread || newSession)
  const availableModels = ownerModelsRequested
    ? ownerModelsNodeId === modelOwnerNodeId ? ownerModels : []
    : asRows(snapshot?.models)
  const savedThreadModel = activeThread ? text(activeThread, 'model') : ''
  const modelValue = activeThread
    ? chatModel !== null ? chatModel : savedThreadModel || text(availableModels[0] ?? {}, 'model', text(availableModels[0] ?? {}, 'id'))
    : newSession
      ? chatModel !== null ? chatModel : text(availableModels[0] ?? {}, 'model', text(availableModels[0] ?? {}, 'id'))
      : chatModel || text(availableModels[0] ?? {}, 'model', text(availableModels[0] ?? {}, 'id'))
  const reasoningModel = availableModels.find(model => text(model, 'model', text(model, 'id')) === modelValue)
  const reasoningOptions = asRows(reasoningModel?.supportedReasoningEfforts).map(option => text(option, 'reasoningEffort', text(option, 'effort'))).filter(Boolean)
  const effectiveReasoningOptions = reasoningOptions.length ? reasoningOptions : ['minimal', 'low', 'medium', 'high', 'xhigh']
  const availableProjects = asRows(snapshot?.projects).filter(project => {
    const id = text(project, 'id')
    if (!modelOwnerNodeId || !id.startsWith('fleet_')) return false
    const owners = Array.isArray(project.availableNodeIds)
      ? project.availableNodeIds.filter((node): node is string => typeof node === 'string')
      : [text(project, 'originNodeId', owner(project, primaryNodeId))]
    return owners.includes(modelOwnerNodeId)
  })
  const sessionProjectId = availableProjects.some(project => text(project, 'id') === selectedProjectId) ? selectedProjectId : ''

  useEffect(() => {
    const generation = ++ownerModelsGeneration.current
    const targetNodeId = modelOwnerNodeId
    if (!profile || !ownerModelsRequested || !targetNodeId) {
      setOwnerModels([])
      setOwnerModelsNodeId('')
      setOwnerModelsLoading(false)
      setOwnerModelsUnavailable(false)
      return () => { if (ownerModelsGeneration.current === generation) ownerModelsGeneration.current++ }
    }
    let active = true
    setOwnerModels([])
    setOwnerModelsNodeId(targetNodeId)
    setOwnerModelsLoading(true)
    setOwnerModelsUnavailable(false)
    void client.request<Row>(profile.id, {
      channel: 'system',
      method: 'models',
      params: {},
      targetNodeId,
    }, 'online').then(reply => {
      if (!active || ownerModelsGeneration.current !== generation) return
      if (reply.error) {
        setOwnerModelsUnavailable(true)
        return
      }
      const result = reply.result
      setOwnerModels(asRows(isRow(result) ? result.data ?? result.models ?? result : result))
    }).catch(() => {
      if (active && ownerModelsGeneration.current === generation) setOwnerModelsUnavailable(true)
    }).finally(() => {
      if (active && ownerModelsGeneration.current === generation) setOwnerModelsLoading(false)
    })
    return () => { active = false }
  }, [client, modelOwnerNodeId, ownerModelsRequested, profile?.id])

  const beginSnapshotRequest = (targetProfileId: string) => {
    const next = (snapshotRequestGeneration.current.get(targetProfileId) ?? 0) + 1
    snapshotRequestGeneration.current.set(targetProfileId, next)
    return next
  }
  const saveSnapshot = useCallback(async (value: Snapshot, targetProfileId: string, generation?: number) => {
    const isCurrent = () => generation === undefined || snapshotRequestGeneration.current.get(targetProfileId) === generation
    if (!targetProfileId || profileRef.current?.id !== targetProfileId || !isCurrent()) return
    snapshotRef.current = value
    setSnapshot(value)
    setConnectionOnline(true)
    setNotice(current => current?.connection ? null : current)
    setSelectedNodeId(current => current || value.nodeId)
    const previous = snapshotCacheWrites.current.get(targetProfileId) ?? Promise.resolve()
    const write = previous.catch(() => undefined).then(async () => {
      if (profileRef.current?.id !== targetProfileId || !isCurrent()) return
      await client.cachePut(`snapshot:${targetProfileId}`, value).catch(() => undefined)
    })
    snapshotCacheWrites.current.set(targetProfileId, write)
    await write
  }, [client])

  const refreshPendingMutations = useCallback(async () => {
    const targetProfileId = profileRef.current?.id
    if (!targetProfileId) { setPendingMutations([]); return }
    const pending = await client.pendingMutations(targetProfileId).catch(() => [])
    if (profileRef.current?.id === targetProfileId) setPendingMutations(pending)
  }, [client, profile?.id])

  const clearConfirmedDraft = async (targetProfileId: string, operationId: string, request: PendingMutation['request'], targetNodeId?: string) => {
    const target = targetNodeId ?? request.targetNodeId ?? ''
    if (request.channel === 'sessions' && ['start', 'send', 'steer'].includes(request.method)) {
      const requestParams = isRow(request.params) ? request.params : {}
      const draftKey = chatDraftKey(targetProfileId, target, text(requestParams, 'threadId', 'new'))
      const cached = await client.cacheGet<Row>(draftKey).catch(() => null)
      const body = text(requestParams, 'text')
      if (isRow(cached) && text(cached, 'operationId') === operationId && text(cached, 'text') === body) {
        await client.cachePut(draftKey, null).catch(() => undefined)
      }
      if (profileRef.current?.id === targetProfileId && currentChatDraftKeyRef.current === draftKey && composeRef.current === body && chatDraftOperationIdRef.current === operationId) {
        composeRef.current = ''
        chatDraftOperationIdRef.current = ''
        setCompose('')
        setChatDraftOperationId('')
      }
      return
    }
    if (request.channel !== 'agents' || !['tasks.create', 'workflows.create', 'memory.revise'].includes(request.method)) return
    if (blockedDefinitionDraftProfiles.current.has(targetProfileId)) return
    await removeDefinitionDrafts(targetProfileId, draft => text(draft, 'profileId') === targetProfileId
      && text(draft, 'operationId', text(draft, 'id')) === operationId
      && text(draft, 'targetNodeId') === target
      && stableJson(draft.params) === stableJson(request.params))
  }

  const definitionDraftOperation = (targetProfileId: string, kind: string, targetNodeId: string, payload: unknown) => {
    const key = stableJson({ profileId: targetProfileId, kind, targetNodeId, payload })
    const existing = definitionDraftOperationIds.current.get(key)
    if (existing) return { key, operationId: existing.operationId }
    const operationId = createOperationId()
    definitionDraftOperationIds.current.set(key, { profileId: targetProfileId, operationId })
    return { key, operationId }
  }

  const completeDefinitionDraftOperation = (key: string, operationId: string) => {
    if (definitionDraftOperationIds.current.get(key)?.operationId === operationId) {
      definitionDraftOperationIds.current.delete(key)
    }
  }

  const beginDefinitionMutation = (targetProfileId: string) => {
    inFlightDefinitionMutations.current.set(
      targetProfileId,
      (inFlightDefinitionMutations.current.get(targetProfileId) ?? 0) + 1,
    )
    let finished = false
    return () => {
      if (finished) return
      finished = true
      const remaining = (inFlightDefinitionMutations.current.get(targetProfileId) ?? 1) - 1
      if (remaining > 0) inFlightDefinitionMutations.current.set(targetProfileId, remaining)
      else inFlightDefinitionMutations.current.delete(targetProfileId)
    }
  }

  const updateDefinitionDraftList = async (targetProfileId: string, update: (current: Row[]) => Row[]) => {
    if (blockedDefinitionDraftProfiles.current.has(targetProfileId)) {
      throw new Error('This paired profile is being forgotten.')
    }
    const key = definitionDraftsKey(targetProfileId)
    const previous = definitionDraftWrites.current.get(targetProfileId) ?? Promise.resolve()
    const write = previous.catch(() => undefined).then(async () => {
      const cached = await client.cacheGet<Row[]>(key)
      const current = Array.isArray(cached)
        ? cached
        : profileRef.current?.id === targetProfileId
          ? draftsRef.current
          : []
      const next = update(current)
      if (next !== current) await client.cachePut(key, next)
      if (profileRef.current?.id === targetProfileId) {
        draftsRef.current = next
        setDrafts(next)
      }
    })
    definitionDraftWrites.current.set(targetProfileId, write)
    try {
      await write
    } finally {
      if (definitionDraftWrites.current.get(targetProfileId) === write) {
        definitionDraftWrites.current.delete(targetProfileId)
      }
    }
  }

  const persistDefinitionDraft = (targetProfileId: string, draft: Row) => updateDefinitionDraftList(
    targetProfileId,
    current => current.some(existing => text(existing, 'id') === text(draft, 'id'))
      ? current
      : [...current, draft],
  )

  const removeDefinitionDrafts = (targetProfileId: string, shouldRemove: (draft: Row) => boolean) => updateDefinitionDraftList(
    targetProfileId,
    current => {
      const next = current.filter(draft => !shouldRemove(draft))
      return next.length === current.length ? current : next
    },
  )

  const beginDefinitionDraftForget = async (targetProfileId: string) => {
    blockedDefinitionDraftProfiles.current.add(targetProfileId)
    await definitionDraftWrites.current.get(targetProfileId)?.catch(() => undefined)
  }

  useEffect(() => { void refreshPendingMutations() }, [refreshPendingMutations])

  useEffect(() => {
    let active = true
    setChatDraftOperationId('')
    if (currentChatDraftKey) void client.cacheGet<Row>(currentChatDraftKey).then(draft => {
      if (!active || !isRow(draft)) return
      setCompose(text(draft, 'text'))
      setChatDraftOperationId(text(draft, 'operationId'))
    }).catch(() => undefined)
    return () => { active = false }
  }, [client, currentChatDraftKey])

  const refresh = useCallback(async (targetProfileId = profile?.id) => {
    if (!targetProfileId || profileRef.current?.id !== targetProfileId) return
    const generation = beginSnapshotRequest(targetProfileId)
    try {
      const value = await client.snapshot(targetProfileId)
      if (profileRef.current?.id !== targetProfileId || snapshotRequestGeneration.current.get(targetProfileId) !== generation) return
      await saveSnapshot(value, targetProfileId, generation)
    } catch (error) {
      if (profileRef.current?.id === targetProfileId && snapshotRequestGeneration.current.get(targetProfileId) === generation) {
        setConnectionOnline(false)
        setNotice({ kind: 'info', connection: true, message: error instanceof Error ? error.message : 'The node is offline. Showing the last saved snapshot.' })
      }
    }
  }, [client, profile?.id, saveSnapshot])

  const refreshAndSubscribe = useCallback(async (targetProfileId = profile?.id) => {
    if (!targetProfileId || profileRef.current?.id !== targetProfileId) return
    await refresh(targetProfileId)
    if (profileRef.current?.id !== targetProfileId) return
    try {
      await client.subscribe(targetProfileId, snapshotRef.current?.cursor)
    } catch (error) {
      setConnectionOnline(false)
      setNotice({ kind: 'info', connection: true, message: error instanceof Error ? error.message : 'Could not reconnect the event stream. Refresh to try again.' })
    }
  }, [client, profile?.id, refresh])

  useEffect(() => {
    if (!profile?.id) return
    const invalidate = () => { beginSnapshotRequest(profile.id) }
    invalidate()
    return invalidate
  }, [profile?.id])

  useEffect(() => {
    let alive = true
    void client.profiles().then(async saved => {
      if (!alive) return
      setProfiles(saved)
      const current = saved[0]
      if (!current || (profileRef.current && profileRef.current.id !== current.id)) return
      profileRef.current = current
      setProfile(current)
      const cached = await client.cacheGet<Snapshot>(`snapshot:${current.id}`).catch(() => null)
      if (alive && profileRef.current?.id === current.id && cached) {
        snapshotRef.current = cached
        setSnapshot(cached)
        setSelectedNodeId(cached.nodeId)
      }
      const pending = await client.cacheGet<Row[]>(definitionDraftsKey(current.id)).catch(() => null)
      if (alive && profileRef.current?.id === current.id && Array.isArray(pending)) setDrafts(pending)
    }).catch(error => {
      if (alive && nativeAvailable()) setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not read paired nodes.' })
    })
    return () => { alive = false }
  }, [client])

  useEffect(() => {
    if (!profile) return
    let active = true
    let cleanup: (() => void)[] = []
    let snapshotTimer: number | undefined
    const scheduleSnapshotRefresh = () => {
      if (snapshotTimer !== undefined) return
      snapshotTimer = window.setTimeout(() => {
        snapshotTimer = undefined
        void refresh(profile.id)
      }, 120)
    }
    const updateCursor = (epoch: string, sequence: number) => {
      const cursor = { epoch, sequence }
      if (snapshotRef.current) snapshotRef.current = { ...snapshotRef.current, cursor }
      setSnapshot(current => current ? { ...current, cursor } : current)
    }
    const applyToVisibleThread = (event: RemoteEvent): 'applied' | 'unrelated' | 'ownerChanged' => {
      const thread = activeThreadRef.current
      const params = isRow(event.message.params) ? event.message.params : {}
      if (!thread || text(params, 'threadId') !== text(thread, 'id') || event.nodeId !== owner(thread, profile.nodeId)) return 'unrelated'
      const ownerEpoch = text(thread, 'ownerEpoch', nodeEpoch(thread, snapshotRef.current))
      if (event.ownerEpoch && ownerEpoch && event.ownerEpoch !== ownerEpoch) return 'ownerChanged'
      const rpc = {
        ...(event.message as RpcEvent),
        ...(event.ownerEpoch ? { brrEpoch: event.ownerEpoch } : {}),
        ownerNodeId: event.nodeId,
      }
      setLiveSession(current => reduceSession(current ?? hydrateRow(thread), rpc))
      return 'applied'
    }
    const resnapshotVisibleThread = async () => {
      const targetProfileId = profile.id
      await refresh(targetProfileId)
      if (!active || profileRef.current?.id !== targetProfileId) return
      const thread = activeThreadRef.current
      if (!thread) return
      const currentSnapshot = snapshotRef.current
      const nodeId = owner(thread, profile.nodeId)
      const listed = asRows(currentSnapshot?.threads).find(item => text(item, 'id') === text(thread, 'id'))
      const latest: Row = { ...thread, ...(listed ?? {}), ownerNodeId: nodeId }
      const latestEpoch = nodeEpoch(latest, currentSnapshot)
      if (latestEpoch) latest.ownerEpoch = latestEpoch
      try {
        const result = responseResult(await client.request<Row>(profile.id, {
          channel: 'sessions', method: 'read', params: { threadId: text(thread, 'id') },
          targetNodeId: nodeId,
        }, 'online'))
        if (!active || profileRef.current?.id !== targetProfileId) return
        const hydrated = { ...latest, ...result, ownerNodeId: nodeId, ...(latestEpoch ? { ownerEpoch: latestEpoch } : {}) }
        setActiveThread(hydrated)
        setSession(hydrated)
        setLiveSession(hydrateRow(hydrated))
      } catch (error) {
        setNotice({ kind: 'info', message: error instanceof Error ? error.message : 'The conversation could not be refreshed after reconnecting.' })
      }
    }
    const syncVisible = async () => {
      if (document.visibilityState !== 'visible') return
      const generation = beginSnapshotRequest(profile.id)
      try {
        const current = await client.snapshot(profile.id)
        if (!active || profileRef.current?.id !== profile.id) return
        if (snapshotRequestGeneration.current.get(profile.id) === generation) await saveSnapshot(current, profile.id, generation)
        if (!active || profileRef.current?.id !== profile.id) return
        await client.subscribe(profile.id, snapshotRef.current?.cursor)
      } catch (error) {
        if (!active || profileRef.current?.id !== profile.id || snapshotRequestGeneration.current.get(profile.id) !== generation) return
        setConnectionOnline(false)
        setNotice({ kind: 'info', connection: true, message: error instanceof Error ? error.message : 'The node is offline. Showing the last saved snapshot.' })
      }
    }
    const onVisibilityChange = () => {
      if (document.visibilityState === 'hidden') void client.unsubscribe(profile.id)
      else void syncVisible()
    }
    const attach = async () => {
      try {
        cleanup = await Promise.all([
          client.listen<RemoteEvent>('remote-event', event => {
            if (!active) return
            updateCursor(event.epoch, event.sequence)
            if (applyToVisibleThread(event) === 'ownerChanged') {
              void resnapshotVisibleThread()
              return
            }
            if (needsSnapshotRefresh(event)) scheduleSnapshotRefresh()
          }),
          client.listen<EventReplay>('remote-replay', replay => {
            if (!active) return
            const current = snapshotRef.current
            if (replay.requiresSnapshot || (current && replay.epoch !== current.epoch)) {
              void resnapshotVisibleThread()
              return
            }
            if (replay.events.some(event => applyToVisibleThread(event) === 'ownerChanged')) {
              void resnapshotVisibleThread()
              return
            }
            updateCursor(replay.epoch, replay.latest)
            if (replay.events.some(needsSnapshotRefresh)) scheduleSnapshotRefresh()
          }),
          client.listen<{ profileId: string; message: string }>('remote-disconnect', event => {
            if (event.profileId !== profile.id) return
            setConnectionOnline(false)
            setNotice({ kind: 'info', connection: true, message: event.message || 'Connection lost. Cached information is still available.' })
          }),
        ])
        await syncVisible()
      } catch { /* Browser tests use an injected bridge without native events. */ }
    }
    document.addEventListener('visibilitychange', onVisibilityChange)
    void attach()
    return () => { active = false; if (snapshotTimer !== undefined) window.clearTimeout(snapshotTimer); cleanup.forEach(unlisten => unlisten()); document.removeEventListener('visibilitychange', onVisibilityChange); void client.unsubscribe(profile.id) }
  }, [client, profile?.id, profile?.nodeId, saveSnapshot])

  const runRequest = useCallback(async <T,>(channel: Channel, method: string, params: Record<string, unknown>, options: { operationId?: string; expectedEpoch?: string; expectedRevision?: string; targetNodeId?: string } = {}) => {
    if (!profile) throw new Error('Pair a primary node first.')
    let reply: Reply<T>
    try {
      reply = await client.request<T>(profile.id, { channel, method, params, ...options, targetNodeId: options.targetNodeId ?? profile.nodeId, operationId: options.operationId ?? createOperationId() }, online ? 'online' : 'offline')
    } catch (error) {
      if (profileRef.current?.id !== profile.id) throw error
      void refreshPendingMutations()
      if (error instanceof PendingMutationError) {
        setNotice({ kind: 'info', message: 'An earlier action for this node is still unresolved. Check its current state, then retry the saved action.' })
        throw error
      }
      setConnectionOnline(false)
      setNotice({ kind: 'info', message: 'Connection lost. The result of the last action may be unknown; check the node before trying again.' })
      throw error
    }
    void refreshPendingMutations()
    return responseResult<T>(reply)
  }, [client, online, profile, refreshPendingMutations])

  const pairNode = async (event: FormEvent) => {
    event.preventDefault()
    setPairing(true)
    try {
      const parsed: PairInvitation = parseInvitation(invitation)
      const paired = await client.pair(parsed, deviceName.trim() || parsed.name)
      setProfiles(current => [...current.filter(item => item.id !== paired.id), paired])
      profileRef.current = paired
      setProfile(paired)
      setInvitation('')
      setSnapshot(null)
      snapshotRef.current = null
      setDrafts([])
      setPendingMutations([])
      setNotice({ kind: 'success', message: `Paired with ${paired.name || 'your Brr node'}.` })
      const definitions = await client.cacheGet<Row[]>(definitionDraftsKey(paired.id)).catch(() => null)
      if (profileRef.current?.id === paired.id && Array.isArray(definitions)) setDrafts(definitions)
      await refresh(paired.id)
    } catch (error) {
      const message = error instanceof Error ? error.message : typeof error === 'string' ? error : 'Pairing failed. Check the invitation and try again.'
      setNotice({ kind: 'error', message })
    } finally { setPairing(false) }
  }

  const forgetPairedNode = async () => {
    if (!profile) return
    const forgottenProfileId = profile.id
    if ((inFlightDefinitionMutations.current.get(forgottenProfileId) ?? 0) > 0) {
      setNotice({ kind: 'info', message: 'A definition operation is still completing. Wait for it to finish before forgetting this node.' })
      return
    }
    try {
      await beginDefinitionDraftForget(forgottenProfileId)
      await client.forget(forgottenProfileId)
    } catch (error) {
      blockedDefinitionDraftProfiles.current.delete(forgottenProfileId)
      if (profileRef.current?.id === forgottenProfileId) {
        setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not forget this node.' })
      }
      return
    }

    for (const [key, value] of definitionDraftOperationIds.current) {
      if (value.profileId === forgottenProfileId) definitionDraftOperationIds.current.delete(key)
    }
    if (profileRef.current?.id === forgottenProfileId) {
      profileRef.current = null
      draftsRef.current = []
      setProfiles([])
      setProfile(null)
      setSnapshot(null)
      snapshotRef.current = null
      setDrafts([])
      setPendingMutations([])
      setConnectionOnline(false)
    }
    try {
      await client.cachePut(definitionDraftsKey(forgottenProfileId), null)
      if (!profileRef.current) setNotice({ kind: 'success', message: 'This phone was unpaired from the primary node.' })
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error)
      if (!profileRef.current) setNotice({ kind: 'error', message: `The node was unpaired, but offline definition drafts could not be cleared: ${detail}` })
    }
  }

  const retryPendingMutation = async (pending: PendingMutation) => {
    if (!profile) return
    const targetProfileId = profile.id
    const isDefinitionMutation = pending.request.channel === 'agents'
      && ['tasks.create', 'workflows.create', 'memory.revise'].includes(pending.request.method)
    if (isDefinitionMutation && blockedDefinitionDraftProfiles.current.has(targetProfileId)) {
      setNotice({ kind: 'info', message: 'This paired profile is being forgotten. The definition retry was not sent.' })
      return
    }
    const finishDefinitionMutation = isDefinitionMutation ? beginDefinitionMutation(targetProfileId) : undefined
    try {
      const reply = await client.retryMutation(targetProfileId, pending.operationId, online ? 'online' : 'offline')
      if (reply.error) throw new Error(reply.error.message)
      await clearConfirmedDraft(targetProfileId, pending.operationId, pending.request, pending.targetNodeId)
      await refreshPendingMutations()
      await refresh()
      setNotice({ kind: 'success', message: 'Saved action received a response from its owning node.' })
    } catch (error) {
      await refreshPendingMutations()
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'The saved action is still unresolved.' })
    } finally {
      finishDefinitionMutation?.()
    }
  }

  const applyTurnPage = (thread: Row, page: Row) => {
    const older = asRows(page.data).slice().reverse()
    const current = asRows(thread.turns)
    const seen = new Set<string>()
    const turns = [...older, ...current].filter(turn => {
      const id = text(turn, 'id')
      if (!id || seen.has(id)) return false
      seen.add(id)
      return true
    })
    const updated = { ...thread, turns }
    setActiveThread(updated)
    setSession(updated)
    setLiveSession(hydrateRow(updated))
    return updated
  }

  const openThread = async (thread: Row) => {
    const targetProfileId = profile?.id
    if (!targetProfileId || profileRef.current?.id !== targetProfileId) return false
    invalidateChatSelection()
    const selectionGeneration = chatSelectionGeneration.current
    const threadOwner = owner(thread, primaryNodeId)
    setChatModel(null)
    setSelectedProjectId('')
    const scopedThread = { ...thread, ownerNodeId: threadOwner }
    turnCursorHistory.current = new Set()
    setOlderTurnCursor(null)
    setActiveThread(scopedThread)
    setSession(scopedThread)
    setLiveSession(hydrateRow(scopedThread))
    if (threadIsHistorical(scopedThread) || !nodeIsAvailable(threadOwner, nodes)) {
      setNotice({ kind: 'info', message: threadIsHistorical(scopedThread) ? 'Historical conversation. Live actions are unavailable.' : 'The conversation owner is offline or no longer in this fleet. Live actions are unavailable.' })
      return true
    }
    try {
      const data = await runRequest<Row>('sessions', 'read', { threadId: text(thread, 'id') }, { targetNodeId: threadOwner })
      if (profileRef.current?.id !== targetProfileId || chatSelectionGeneration.current !== selectionGeneration) return false
      let opened: Row = { ...scopedThread, ...data, ownerNodeId: threadOwner }
      setActiveThread(opened)
      setSession(opened)
      setLiveSession(hydrateRow(opened))
      if (text(opened, 'historyMode') === 'paginated') {
        const page = await runRequest<Row>('sessions', 'turns', { threadId: text(thread, 'id') }, { targetNodeId: threadOwner })
        if (profileRef.current?.id !== targetProfileId || chatSelectionGeneration.current !== selectionGeneration) return false
        const nextCursor = typeof page.nextCursor === 'string' ? page.nextCursor : null
        setOlderTurnCursor(nextCursor)
        opened = applyTurnPage(opened, page)
      }
      return true
    } catch (error) {
      if (profileRef.current?.id === targetProfileId && chatSelectionGeneration.current === selectionGeneration) {
        setNotice({ kind: 'info', message: error instanceof Error ? error.message : 'Showing cached conversation details.' })
      }
      return false
    }
  }

  const loadOlderTurns = async () => {
    if (!activeThread || !olderTurnCursor || loadingOlderTurns) return
    const targetProfileId = profile?.id
    const selectionGeneration = chatSelectionGeneration.current
    const isCurrentSelection = () => Boolean(targetProfileId)
      && profileRef.current?.id === targetProfileId
      && chatSelectionGeneration.current === selectionGeneration
    const cursor = olderTurnCursor
    if (turnCursorHistory.current.has(cursor)) {
      setNotice({ kind: 'error', message: 'The runtime repeated a thread history cursor. Refresh the conversation before loading more.' })
      setOlderTurnCursor(null)
      return
    }
    turnCursorHistory.current.add(cursor)
    setLoadingOlderTurns(true)
    try {
      const threadOwner = owner(activeThread, primaryNodeId)
      const page = await runRequest<Row>('sessions', 'turns', { threadId: text(activeThread, 'id'), cursor }, { targetNodeId: threadOwner })
      if (!isCurrentSelection()) return
      const nextCursor = typeof page.nextCursor === 'string' ? page.nextCursor : null
      if (nextCursor && (nextCursor === cursor || turnCursorHistory.current.has(nextCursor))) {
        setNotice({ kind: 'error', message: 'The runtime repeated a thread history cursor. Refresh the conversation before loading more.' })
        setOlderTurnCursor(null)
        return
      }
      setOlderTurnCursor(nextCursor)
      applyTurnPage(activeThread, page)
    } catch (error) {
      if (!isCurrentSelection()) return
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not load older conversation history.' })
    } finally { if (isCurrentSelection()) setLoadingOlderTurns(false) }
  }

  const sendMessage = async (event: FormEvent) => {
    event.preventDefault()
    const body = compose.trim()
    if (!body || !profile) return
    const targetProfileId = profile.id
    const selectionGeneration = chatSelectionGeneration.current
    const isCurrentSelection = () => profileRef.current?.id === targetProfileId
      && chatSelectionGeneration.current === selectionGeneration
    const canSend = activeThread ? activeThreadCanOperate : online && nodeIsAvailable(selectedNodeId, nodes)
    if (!canSend || (activeThread && threadIsHistorical(activeThread))) {
      const operationId = chatDraftOperationId || createOperationId()
      await client.cachePut(currentChatDraftKey, { text: body, threadId: text(activeThread ?? {}, 'id'), ownerNodeId: activeThread ? activeThreadNodeId : selectedNodeId, baseRevision: snapshot?.epoch, operationId }).catch(() => undefined)
      if (!isCurrentSelection()) return
      chatDraftOperationIdRef.current = operationId
      setChatDraftOperationId(operationId)
      setNotice({ kind: 'info', message: 'Message saved on this device. Connect to send it.' })
      return
    }
    const targetNodeId = activeThread ? owner(activeThread, primaryNodeId) : selectedNodeId
    const operationId = chatDraftOperationId || createOperationId()
    setBusy('Sending message…')
    try {
      const params = activeThread
        ? { threadId: text(activeThread, 'id'), text: body, model: modelValue, effort: chatEffort, mode: chatMode, ...(steering && activeTurn ? { expectedTurnId: text(activeTurn, 'id') } : {}) }
        : { model: modelValue, projectId: sessionProjectId || null, nodeId: selectedNodeId, readOnly: chatAccess === 'read-only' }
      const method = activeThread ? steering && activeTurn ? 'steer' : 'send' : 'start'
      const channel: Channel = 'sessions'
      const payload = activeThread ? params : { ...params, text: body, effort: chatEffort, mode: chatMode }
      const request = { id: '', channel, method, params: payload, targetNodeId }
      await client.cachePut(currentChatDraftKey, { text: body, threadId: text(activeThread ?? {}, 'id'), ownerNodeId: targetNodeId, baseRevision: snapshot?.epoch, operationId }).catch(() => undefined)
      if (!isCurrentSelection()) return
      chatDraftOperationIdRef.current = operationId
      setChatDraftOperationId(operationId)
      const result = await runRequest<Row>(channel, method, payload, { operationId, targetNodeId, ...(steering && activeThread && activeTurn ? { expectedEpoch: text(activeThread, 'ownerEpoch', nodeEpoch(activeThread, snapshot)) } : {}) })
      await clearConfirmedDraft(targetProfileId, operationId, request, targetNodeId)
      if (!isCurrentSelection()) return
      if (currentChatDraftKeyRef.current === currentChatDraftKey && composeRef.current === body && chatDraftOperationIdRef.current === operationId) {
        composeRef.current = ''
        chatDraftOperationIdRef.current = ''
        setChatDraftOperationId('')
        setCompose('')
        setSteering(false)
      }
      if (!activeThread) {
        const thread = isRow(result.thread) ? result.thread : result
        setActiveThread(thread)
        setSession(thread)
        setLiveSession(hydrateRow(thread))
        setNewSession(false)
      }
      await refresh(targetProfileId)
    } catch (error) {
      if (!isCurrentSelection()) return
      if (error instanceof PendingMutationError) {
        setNotice({ kind: 'info', message: 'This message was not sent. An earlier action for this node is still unresolved; check its state, then retry the saved action.' })
        return
      }
      setNotice({ kind: 'error', message: `The message may have reached the node. Check the conversation before sending it again. ${error instanceof Error ? error.message : ''}`.trim() })
    } finally { if (isCurrentSelection()) setBusy('') }
  }

  const stopActiveTurn = async () => {
    if (!activeThread || !activeTurn || !activeThreadCanOperate) return
    try {
      await runRequest('sessions', 'stop', { threadId: text(activeThread, 'id'), turnId: text(activeTurn, 'id') }, { operationId: createOperationId(), expectedEpoch: text(activeThread, 'ownerEpoch', nodeEpoch(activeThread, snapshot)), targetNodeId: owner(activeThread, primaryNodeId) })
      await openThread(activeThread)
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Stop outcome is unknown. Refresh the conversation before retrying.' })
    }
  }

  const sessionAction = async (action: 'rename' | 'archive' | 'delete', name?: string) => {
    if (!activeThread || !activeThreadCanOperate) return
    const threadId = text(activeThread, 'id')
    const threadOwner = owner(activeThread, primaryNodeId)
    try {
      await runRequest('sessions', action, { threadId, ...(action === 'rename' ? { name: name?.trim() } : {}) }, {
        operationId: createOperationId(),
        targetNodeId: threadOwner,
      })
      if (action === 'rename') {
        const updated = { ...activeThread, name: name?.trim(), title: name?.trim() }
        setActiveThread(updated)
        setSession(updated)
        setSnapshot(current => current ? { ...current, threads: asRows(current.threads).map(thread => text(thread, 'id') === threadId ? { ...thread, name: name?.trim(), title: name?.trim() } : thread) } : current)
        setRenamingThread(false)
        setSessionActionsOpen(false)
        return
      }
      setActiveThread(null)
      setSession(null)
      setLiveSession(null)
      setSessionActionsOpen(false)
      setConfirmDeleteThread(false)
      await refresh()
      setNotice({ kind: 'success', message: action === 'archive' ? 'Conversation archived.' : 'Conversation deleted.' })
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : `Could not ${action} this conversation.` })
    }
  }

  const openAgent = async (agent: Row) => {
    setSelectedAgent(agent)
    setForgottenMemoryEvent('')
    const managementNodeId = selectedNodeId || primaryNodeId
    setAgentExecutionNodeId(managementNodeId)
    setAgentPane('profile')
    try {
      const result = await runRequest<Row>('agents', 'agents.read', { agentId: text(agent, 'id') }, { targetNodeId: managementNodeId })
      setSelectedAgent(result)
    } catch (error) { setNotice({ kind: 'info', message: error instanceof Error ? error.message : 'Showing cached agent details.' }) }
  }

  const startAgentInterview = async () => {
    if (selectedAgent ? !agentNodeOnline : !newAgentNodeOnline) {
      setNotice({ kind: 'info', message: 'Choose an online node that is available for this agent before starting work.' })
      return
    }
    const targetProfileId = profile?.id
    const selectionGeneration = chatSelectionGeneration.current
    setBusy('Starting interview…')
    try {
      const result = await runRequest<Row>('agents', 'agents.interview', { model: text(selectedAgent ?? {}, 'model', text(asRows(snapshot?.models)[0] ?? {}, 'model', text(asRows(snapshot?.models)[0] ?? {}, 'id'))), effort: text(selectedAgent ?? {}, 'effort', 'medium') }, { targetNodeId: selectedAgent ? effectiveAgentNodeId : selectedNodeId })
      if (!targetProfileId || profileRef.current?.id !== targetProfileId || chatSelectionGeneration.current !== selectionGeneration) return
      const thread = isRow(result.thread) ? result.thread : result
      setTab('chats')
      await openThread(thread)
    } catch (error) {
      if (targetProfileId && profileRef.current?.id === targetProfileId && chatSelectionGeneration.current === selectionGeneration) {
        setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not start the interview.' })
      }
    } finally { if (targetProfileId && profileRef.current?.id === targetProfileId) setBusy('') }
  }

  const openAgentConversation = async (savedThread?: Row) => {
    if (!selectedAgent) return
    if (savedThread) {
      const binding = isRow(savedThread.brr) ? savedThread.brr : {}
      const thread = { ...savedThread, ownerNodeId: text(savedThread, 'ownerNodeId', text(binding, 'ownerNodeId', effectiveAgentNodeId)) }
      setTab('chats')
      await openThread(thread)
      return
    }
    const targetProfileId = profile?.id
    const selectionGeneration = chatSelectionGeneration.current
    setBusy('Opening agent conversation…')
    try {
      const result = await runRequest<Row>('agents', 'agents.open', { agentId: text(selectedAgent, 'id'), model: text(selectedAgent, 'model'), effort: text(selectedAgent, 'effort') }, { targetNodeId: effectiveAgentNodeId })
      if (!targetProfileId || profileRef.current?.id !== targetProfileId || chatSelectionGeneration.current !== selectionGeneration) return
      const thread = isRow(result.thread) ? result.thread : result
      setTab('chats')
      await openThread(thread)
    } catch (error) {
      if (targetProfileId && profileRef.current?.id === targetProfileId && chatSelectionGeneration.current === selectionGeneration) {
        setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not open the agent conversation.' })
      }
    } finally { if (targetProfileId && profileRef.current?.id === targetProfileId) setBusy('') }
  }

  const loadAgentPane = async (pane: AgentPane) => {
    setAgentPane(pane)
    if (!selectedAgent) return
    const id = text(selectedAgent, 'id')
    const taskGeneration = ++taskListGeneration.current
    const endpoint: Record<AgentPane, [string, string, Record<string, unknown>]> = {
      chat: ['agents', 'agents.sessions', { agentId: id }],
      tasks: ['agents', 'tasks.list', { agentId: id }],
      memory: ['agents', 'memory.list', { agentId: id }],
      skills: ['agents', 'skills.list', { agentId: id }],
      profile: ['agents', 'agents.drafts', {}],
    }
    if (!agentNodeOnline) {
      setRows([])
      setNotice({ kind: 'info', message: 'This node is offline. Connect it to read or manage this agent here.' })
      return
    }
    try {
      const [channel, method, params] = endpoint[pane]
      const result = await runRequest<unknown>(channel as Channel, method, params, { targetNodeId: effectiveAgentNodeId })
      if (pane === 'tasks' && taskGeneration !== taskListGeneration.current) return
      let loaded = asRows(isRow(result) && 'data' in result ? result.data : result)
      if (pane === 'tasks') {
        taskRowsRef.current = loaded
        taskCursorHistory.current = new Set()
        setTaskNextCursor(isRow(result) && typeof result.nextCursor === 'string' ? result.nextCursor : null)
        const [workflows, proposals] = await Promise.all([
          runRequest<unknown>('agents', 'workflows.list', { agentId: id }, { targetNodeId: effectiveAgentNodeId }).catch(() => []),
          runRequest<unknown>('agents', 'tasks.proposals', { agentId: id }, { targetNodeId: effectiveAgentNodeId }).catch(() => []),
        ])
        if (taskGeneration !== taskListGeneration.current) return
        workflowRowsRef.current = asRows(isRow(workflows) && 'data' in workflows ? workflows.data : workflows)
        proposalRowsRef.current = asRows(isRow(proposals) && 'data' in proposals ? proposals.data : proposals)
        loaded = [...taskRowsRef.current, ...workflowRowsRef.current, ...proposalRowsRef.current]
      } else if (pane === 'memory') {
        const sources = asRows(isRow(result) ? result.sources : undefined)
        const sourceById = new Map(sources
          .filter(source => {
            const sourceAgent = text(source, 'agentId')
            const sourceOwner = text(source, 'ownerNodeId')
            return (!sourceAgent || sourceAgent === id) && (!sourceOwner || sourceOwner === effectiveAgentNodeId)
          })
          .map(source => [text(source, 'sourceId'), source] as const)
          .filter(([sourceId]) => Boolean(sourceId)))
        loaded = loaded.map(memory => ({
          ...memory,
          sources: (Array.isArray(memory.sourceIds) ? memory.sourceIds : [])
            .filter((sourceId): sourceId is string => typeof sourceId === 'string')
            .map(sourceId => sourceById.get(sourceId))
            .filter((source): source is Row => Boolean(source)),
        }))
      } else if (pane === 'profile') {
        loaded = loaded.filter(draft => text(draft, 'agentId') === id)
      }
      setRows(loaded)
    } catch (error) { setRows([]); setNotice({ kind: 'error', message: error instanceof Error ? error.message : `Could not load ${pane}.` }) }
  }

  useEffect(() => {
    if (tab !== 'agents' || selectedAgent || !online || !selectedNodeId) return
    let active = true
    void runRequest<unknown>('agents', 'agents.drafts', {}, { targetNodeId: selectedNodeId })
      .then(result => {
        if (active) setNewProfileDrafts(asRows(isRow(result) && 'data' in result ? result.data : result).filter(draft => !text(draft, 'agentId') && isReviewableProfileDraft(draft)))
      })
      .catch(error => { if (active) setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not load new agent profile reviews.' }) })
    return () => { active = false }
  }, [tab, selectedAgent?.id, online, selectedNodeId, runRequest])

  const reviewNewProfile = async (draft: Row, approve: boolean) => {
    try {
      await runRequest('agents', approve ? 'agents.approve' : 'agents.reject', { draftId: text(draft, 'id'), hash: text(draft, 'hash') }, { ...revisionOptions(draft, 'hash'), targetNodeId: selectedNodeId })
      setNewProfileDrafts(current => current.filter(item => text(item, 'id') !== text(draft, 'id')))
      if (approve) await refresh()
      setNotice({ kind: 'success', message: approve ? 'New agent profile approved and created.' : 'New agent profile proposal rejected.' })
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not review the new agent profile.' })
    }
  }

  useEffect(() => { if (tab === 'agents' && selectedAgent) void loadAgentPane(agentPane) }, [tab, selectedAgent?.id, agentPane, effectiveAgentNodeId, agentNodeOnline]) // intentional data refresh on pane and execution node changes

  const loadMoreTasks = async () => {
    const agentId = text(selectedAgent ?? {}, 'id')
    const cursor = taskNextCursor
    if (!agentId || !cursor || loadingMoreTasks) return
    const taskGeneration = taskListGeneration.current
    if (taskCursorHistory.current.has(cursor)) {
      setNotice({ kind: 'error', message: 'The node repeated a task cursor. Refresh the task list before loading more.' })
      setTaskNextCursor(null)
      return
    }
    taskCursorHistory.current.add(cursor)
    setLoadingMoreTasks(true)
    try {
      const page = await runRequest<Row>('agents', 'tasks.list', { agentId, after: cursor, limit: 100 }, { targetNodeId: effectiveAgentNodeId })
      if (taskGeneration !== taskListGeneration.current || agentId !== text(selectedAgent ?? {}, 'id')) return
      const nextCursor = typeof page.nextCursor === 'string' ? page.nextCursor : null
      if (nextCursor && (nextCursor === cursor || taskCursorHistory.current.has(nextCursor))) {
        setNotice({ kind: 'error', message: 'The node repeated a task cursor. Refresh the task list before loading more.' })
        setTaskNextCursor(null)
        return
      }
      const incoming = asRows(page.data)
      const unique = new Map<string, Row>()
      for (const task of [...taskRowsRef.current, ...incoming]) {
        const id = text(task, 'id')
        if (id) unique.set(id, task)
      }
      taskRowsRef.current = [...unique.values()]
      setTaskNextCursor(nextCursor)
      setRows([...taskRowsRef.current, ...workflowRowsRef.current, ...proposalRowsRef.current])
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not load more tasks.' })
    } finally { if (taskGeneration === taskListGeneration.current) setLoadingMoreTasks(false) }
  }

  const createTask = async (event: FormEvent) => {
    event.preventDefault()
    if (!profile) return
    const submittedDraftTask = { ...draftTask }
    const title = submittedDraftTask.title.trim()
    const instructions = submittedDraftTask.instructions.trim()
    const criteria = submittedDraftTask.acceptance.split('\n').map(value => value.trim()).filter(Boolean)
    if (!title || !instructions || criteria.length === 0) { setNotice({ kind: 'error', message: 'Add instructions and at least one acceptance criterion.' }); return }
    const value = { profileId: profile.id, agentId: text(selectedAgent ?? {}, 'id'), title, instructions, acceptanceCriteria: criteria, ...(selectedAgent ? { targetNodeId: effectiveAgentNodeId } : { targetNodeId: selectedNodeId }), ...revisionOptions(selectedAgent ?? {}), operationId: createOperationId() }
    if (!agentNodeOnline) {
      const params = { agentId: value.agentId, title, instructions, acceptanceCriteria: criteria }
      const identity = definitionDraftOperation(profile.id, 'task', value.targetNodeId, { params, revision: revisionOptions(selectedAgent ?? {}) })
      const draft = { kind: 'task', params, ...value, id: identity.operationId, operationId: identity.operationId, status: 'saved_offline', createdAt: Date.now() }
      try {
        await persistDefinitionDraft(profile.id, draft)
        completeDefinitionDraftOperation(identity.key, identity.operationId)
        if (profileRef.current?.id !== profile.id) return
        setDraftTask(current => stableJson(current) === stableJson(submittedDraftTask)
          ? { title: '', instructions: '', acceptance: '' }
          : current)
        setNotice({ kind: 'success', message: 'Task definition saved on this device. Review it before creating the task.' })
      } catch (error) {
        if (profileRef.current?.id === profile.id) setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not save task draft.' })
      }
      return
    }
    setBusy('Creating task…')
    try {
      await runRequest('agents', 'tasks.create', { agentId: value.agentId, title, instructions, acceptanceCriteria: criteria }, { operationId: value.operationId, ...revisionOptions(value, 'expectedRevision'), targetNodeId: value.targetNodeId })
      setDraftTask({ title: '', instructions: '', acceptance: '' })
      await loadAgentPane('tasks')
      setNotice({ kind: 'success', message: 'Task created.' })
    } catch (error) { setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not create task.' }) }
    finally { setBusy('') }
  }

  const saveSchedule = async (event: FormEvent) => {
    event.preventDefault()
    if (!selectedAgent || !profile) return
    const acceptanceCriteria = taskSchedule.acceptance.split('\n').map(value => value.trim()).filter(Boolean)
    if (!acceptanceCriteria.length) { setNotice({ kind: 'error', message: 'Add at least one schedule acceptance criterion.' }); return }
    const params = { name: taskSchedule.name.trim(), agentId: text(selectedAgent, 'id'), title: taskSchedule.title.trim(), instructions: taskSchedule.instructions.trim(), acceptanceCriteria, expression: taskSchedule.cron, timezone: taskSchedule.timezone }
    if (!agentNodeOnline) {
      const revision = revisionOptions(selectedAgent)
      const identity = definitionDraftOperation(profile.id, 'workflow', effectiveAgentNodeId, { params, revision })
      const operationId = identity.operationId
      const draft = { id: operationId, profileId: profile.id, kind: 'workflow', status: 'saved_offline', params, operationId, targetNodeId: effectiveAgentNodeId, ...revision, createdAt: Date.now() }
      try {
        await persistDefinitionDraft(profile.id, draft)
        completeDefinitionDraftOperation(identity.key, operationId)
        if (profileRef.current?.id === profile.id) setNotice({ kind: 'success', message: 'Schedule definition saved on this device for review.' })
      } catch (error) {
        if (profileRef.current?.id === profile.id) setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not save schedule draft.' })
      }
      return
    }
    try {
      await runRequest('agents', 'workflows.create', params, { operationId: createOperationId(), ...revisionOptions(selectedAgent), targetNodeId: effectiveAgentNodeId })
      setTaskSchedule({ ...taskSchedule, name: '', title: '', instructions: '', acceptance: '' })
      await loadAgentPane('tasks')
      setNotice({ kind: 'success', message: 'Schedule saved.' })
    } catch (error) { setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not save schedule.' }) }
  }

  const publishDefinitionDraft = async (draft: Row) => {
    if (!profile || text(draft, 'profileId') !== profile.id) return
    const targetProfileId = profile.id
    if (blockedDefinitionDraftProfiles.current.has(targetProfileId)) {
      setNotice({ kind: 'info', message: 'This paired profile is being forgotten. The definition was not sent.' })
      return
    }
    const finishDefinitionMutation = beginDefinitionMutation(targetProfileId)
    const kind = text(draft, 'kind')
    const params = isRow(draft.params) ? draft.params : {}
    const method = kind === 'workflow' ? 'workflows.create' : kind === 'memory' ? 'memory.revise' : 'tasks.create'
    try {
      await runRequest('agents', method, params, {
        operationId: text(draft, 'operationId', text(draft, 'id')),
        ...revisionOptions(draft, 'expectedRevision'),
        targetNodeId: text(draft, 'targetNodeId') || selectedNodeId,
      })
      await removeDefinitionDrafts(targetProfileId, candidate => text(candidate, 'profileId') === targetProfileId
        && text(candidate, 'id') === text(draft, 'id'))
      if (profileRef.current?.id === targetProfileId) setNotice({ kind: 'success', message: 'Saved definition created on its owning node.' })
      if (kind === 'task' || kind === 'workflow') await loadAgentPane('tasks')
      if (kind === 'memory') await loadAgentPane('memory')
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not create the saved definition. Check its current state before retrying.' })
    } finally {
      finishDefinitionMutation()
    }
  }

  const saveMemory = async (value: Row) => {
    if (!selectedAgent || !profile) return
    const params = { agentId: text(selectedAgent, 'id'), id: text(value, 'id'), revision: Number(value.revision), content: text(value, 'content') }
    if (!agentNodeOnline) {
      const revision = revisionOptions(value)
      const identity = definitionDraftOperation(profile.id, 'memory', effectiveAgentNodeId, { params, revision })
      const operationId = identity.operationId
      const draft = { id: operationId, profileId: profile.id, kind: 'memory', status: 'saved_offline', params, operationId, ...revision, targetNodeId: effectiveAgentNodeId, createdAt: Date.now() }
      try {
        await persistDefinitionDraft(profile.id, draft)
        completeDefinitionDraftOperation(identity.key, operationId)
        if (profileRef.current?.id !== profile.id) return
        setMemoryEdit(current => current && stableJson(current) === stableJson(value) ? null : current)
        setNotice({ kind: 'success', message: 'Memory revision saved on this device. It will only apply after you review and send it.' })
      } catch (error) {
        if (profileRef.current?.id === profile.id) setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not save memory draft.' })
      }
      return
    }
    try {
      await runRequest('agents', 'memory.revise', params, { ...revisionOptions(value), targetNodeId: effectiveAgentNodeId })
      setMemoryEdit(null)
      await loadAgentPane('memory')
    } catch (error) { setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not revise memory.' }) }
  }

  const forgetMemory = async (value: Row) => {
    if (!selectedAgent) return
    try {
      const result = await runRequest<Row>('agents', 'memory.forget', { agentId: text(selectedAgent, 'id'), id: text(value, 'id') }, { operationId: createOperationId(), ...revisionOptions(value), targetNodeId: effectiveAgentNodeId })
      setForgottenMemoryEvent(String(result.eventId ?? ''))
      await loadAgentPane('memory')
    } catch (error) { setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not forget this memory.' }) }
  }

  const undoForgetMemory = async () => {
    if (!forgottenMemoryEvent) return
    try {
      await runRequest('agents', 'memory.undo', { eventId: Number(forgottenMemoryEvent), agentId: text(selectedAgent ?? {}, 'id') }, { operationId: createOperationId(), targetNodeId: effectiveAgentNodeId })
      setForgottenMemoryEvent('')
      await loadAgentPane('memory')
    } catch (error) { setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not undo the memory change.' }) }
  }

  const editAgentProfile = async () => {
    if (!selectedAgent) return
    setBusy('Starting profile interview…')
    try {
      const result = await runRequest<Row>('agents', 'agents.edit', { agentId: text(selectedAgent, 'id') }, { targetNodeId: effectiveAgentNodeId })
      const thread = isRow(result.thread) ? result.thread : result
      await activateThread(thread)
    } catch (error) { setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not open the profile interview.' }) }
    finally { setBusy('') }
  }

  const refreshAttention = async () => {
    if (!online) return
    await refresh()
  }

  const answerQuestion = async (question: Row, answers: QuestionAnswers) => {
    const current = snapshotRef.current
    const targetProfileId = profile?.id
    if (!targetProfileId || profileRef.current?.id !== targetProfileId || !onlineRef.current || !nodeIsAvailable(owner(question, current?.nodeId ?? primaryNodeId), asRows(current?.nodes))) return
    const id = text(question, 'id')
    try {
      await runRequest('questions', 'answer', { requestId: id, answers }, { targetNodeId: owner(question, primaryNodeId), expectedEpoch: text(question, 'ownerEpoch', text(question, 'epoch', nodeEpoch(question, snapshot))) })
      await client.cachePut(`draft:question:${profile?.id}:${id}`, null).catch(() => undefined)
      await refreshAttention()
      setNotice({ kind: 'success', message: 'Answer sent.' })
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Answer delivery is uncertain. Check the question state before retrying.' })
      throw error
    }
  }

  const saveQuestionDraft = async (question: Row, answers: QuestionAnswers) => {
    const requestId = text(question, 'id')
    const targetProfileId = profile?.id
    const key = `draft:question:${targetProfileId}:${requestId}`
    await client.cachePut(key, answers)
    const current = snapshotRef.current
    const questionOwner = owner(question, current?.nodeId ?? primaryNodeId)
    if (!targetProfileId || profileRef.current?.id !== targetProfileId || !onlineRef.current || !nodeIsAvailable(questionOwner, asRows(current?.nodes))) return
    await runRequest('questions', 'draft', { requestId, answers }, { targetNodeId: owner(question, primaryNodeId), expectedEpoch: text(question, 'ownerEpoch', text(question, 'epoch', nodeEpoch(question, snapshot))) })
  }

  const dismissQuestion = async (question: Row) => {
    const current = snapshotRef.current
    const targetProfileId = profile?.id
    if (!targetProfileId || profileRef.current?.id !== targetProfileId || !onlineRef.current || !nodeIsAvailable(owner(question, current?.nodeId ?? primaryNodeId), asRows(current?.nodes))) return
    await runRequest('questions', 'dismiss', { requestId: text(question, 'id') }, { targetNodeId: owner(question, primaryNodeId), expectedEpoch: text(question, 'ownerEpoch', text(question, 'epoch', nodeEpoch(question, snapshot))) })
    await client.cachePut(`draft:question:${profile?.id}:${text(question, 'id')}`, null).catch(() => undefined)
    await refreshAttention()
  }

  const answerApproval = async (approval: Row, result: Record<string, unknown>) => {
    const current = snapshotRef.current
    const targetProfileId = profile?.id
    if (!targetProfileId || profileRef.current?.id !== targetProfileId || !onlineRef.current || !nodeIsAvailable(owner(approval, current?.nodeId ?? primaryNodeId), asRows(current?.nodes))) return
    try {
      const params: Row = { requestId: approval.requestId ?? approval.id, result }
      const context = isRow(approval.params) ? approval.params : {}
      for (const key of ['threadId', 'turnId']) if (context[key] !== undefined) params[key] = context[key]
      await runRequest('approvals', 'answer', params, { targetNodeId: owner(approval, primaryNodeId), expectedEpoch: text(approval, 'ownerEpoch', text(approval, 'epoch', nodeEpoch(approval, snapshot))) })
      await refreshAttention()
    } catch (error) {
      setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'The decision may have reached the node. Refresh before taking another action.' })
      throw error
    }
  }

  const activateThread = async (thread: Row) => {
    const targetProfileId = profileRef.current?.id
    await openThread(thread)
    if (targetProfileId && profileRef.current?.id === targetProfileId) setTab('chats')
  }
  const onlineState = online ? <span className="connection-pill is-online"><Wifi size={13} aria-hidden="true" /> Connected</span> : <span className="connection-pill"><CloudOff size={13} aria-hidden="true" /> Offline</span>

  return <div className="mobile-shell">
    <header className="topbar">
      <div className="brand"><Logo animated={false} /><span className="brand-divider" /><span className="brand-title">Remote</span></div>
      <div className="topbar-actions">{profile && <span className="node-chip"><span className="node-dot" />{profile.name || 'Primary node'}</span>}{onlineState}<button className="icon-button" aria-label="Refresh snapshot" disabled={!profile || busy !== ''} onClick={() => void refreshAndSubscribe()}><RefreshCw size={18} /></button></div>
    </header>

    {notice && <div className={`notice notice-${notice.kind}`} role={notice.kind === 'error' ? 'alert' : 'status'}><span className="notice-icon">{notice.kind === 'error' ? <CircleAlert size={17} /> : notice.kind === 'success' ? <Check size={17} /> : <CircleHelp size={17} />}</span><p>{notice.message}</p><button aria-label="Dismiss message" className="icon-button" onClick={() => setNotice(null)}><X size={16} /></button></div>}

    {profile && pendingMutations.length > 0 && <aside className="pending-mutations" aria-label="Saved actions to check"><div><strong>Actions with unknown outcomes</strong><p>Check current state first. Retry sends the same saved request and operation ID.</p></div>{pendingMutations.map(pending => <div className="pending-mutation-row" key={pending.operationId}><span>{pending.request.channel}.{pending.request.method} · {pending.targetNodeId || pending.request.targetNodeId || 'Primary node'}</span><button className="secondary-button" disabled={!online || Boolean(busy)} onClick={() => void retryPendingMutation(pending)}>Retry saved action</button></div>)}</aside>}

    <main className="main-content" ref={scrollRef}>
      {!profile ? <PairingScreen invitation={invitation} name={deviceName} busy={pairing} onInvitation={setInvitation} onName={setDeviceName} onSubmit={pairNode} native={native} scanner={scanner} /> : <>
        {tab === 'chats' && <section className="page chats-page" aria-label="Chats">
          {activeThread || newSession ? <div className="conversation-view">
            <div className="page-heading conversation-heading"><button className="back-button" aria-label="Back to chats" onClick={() => { invalidateChatSelection(); activeThreadRef.current = null; setActiveThread(null); setSession(null); setLiveSession(null); setNewSession(false); composeRef.current = ''; setCompose(''); setChatDraftOperationId('') }}><ChevronLeft /></button><div className="heading-copy"><p className="eyebrow">{activeThread ? `${owner(activeThread, primaryNodeId) === primaryNodeId ? 'Primary node' : nodeLabel(owner(activeThread, primaryNodeId), nodes)}${threadIsHistorical(activeThread) ? ' · Historical' : activeThreadCanOperate ? '' : ' · Offline'}` : `New session · ${selectedNodeId ? nodeLabel(selectedNodeId, nodes) : 'Choose a node'}`}</p><h1>{activeThread ? labelFor(activeThread, 'title', 'preview', 'id') : 'New session'}</h1></div>{activeThread && <button className="icon-button" aria-label="More conversation actions" aria-expanded={sessionActionsOpen} onClick={() => { setSessionActionsOpen(open => !open); setRenamingThread(false); setConfirmDeleteThread(false) }}><MoreHorizontal /></button>}</div>
            {sessionActionsOpen && activeThread && <div className="session-action-menu" role="group" aria-label="Conversation actions">{renamingThread ? <form onSubmit={event => { event.preventDefault(); void sessionAction('rename', renameValue) }}><label className="field-label">Conversation name<input aria-label="Conversation name" required value={renameValue} onChange={event => setRenameValue(event.target.value)} /></label><div className="inline-actions"><button type="button" className="text-button" onClick={() => setRenamingThread(false)}>Cancel</button><button className="primary-button" disabled={!activeThreadCanOperate}>Save name</button></div></form> : confirmDeleteThread ? <><p>Delete this conversation permanently?</p><div className="inline-actions"><button className="text-button" onClick={() => setConfirmDeleteThread(false)}>Keep conversation</button><button className="secondary-button danger-text" disabled={!activeThreadCanOperate} onClick={() => void sessionAction('delete')}>Delete permanently</button></div></> : <div className="inline-actions"><button className="text-button" disabled={!activeThreadCanOperate} onClick={() => { setRenameValue(labelFor(activeThread, 'title', 'name', 'preview')); setRenamingThread(true) }}>Rename conversation</button><button className="text-button" disabled={!activeThreadCanOperate} onClick={() => void sessionAction('archive')}>Archive conversation</button><button className="text-button danger-text" disabled={!activeThreadCanOperate} onClick={() => setConfirmDeleteThread(true)}>Delete conversation</button></div>}</div>}
            <div className="transcript" aria-label="Conversation transcript">
              {olderTurnCursor && <button type="button" className="secondary-button load-history-button" disabled={!online || loadingOlderTurns} onClick={() => void loadOlderTurns()}>{loadingOlderTurns ? 'Loading history…' : 'Load older history'}</button>}
              {liveSession?.items.length || asRows(session?.items ?? activeThread?.items ?? []).length ? (liveSession?.items ?? asRows(session?.items ?? activeThread?.items ?? [])).map((item, index) => <TranscriptItem item={item as Row} key={text(item as Row, 'id', String(index))} />) : <div className="empty-state"><MessageSquare size={22} /><h2>No messages yet</h2><p>Send a message to continue this conversation.</p></div>}
              {typeof session?.attachmentError === 'string' && <p className="helper-copy" role="status">{session.attachmentError}</p>}
              {activeThread && profile && <ThreadAttachments key={`${profile.id}:${activeThreadNodeId}:${text(activeThread, 'id')}`} client={client} profileId={profile.id} ownerNodeId={activeThreadNodeId} threadId={text(activeThread, 'id')} attachments={session?.attachments ?? activeThread.attachments} online={online} />}
            </div>
            {activeThread && activeTurn && <div className="turn-actions"><span><span className="status-dot" data-status="running" /> Active run</span><div><button type="button" className="secondary-button" disabled={!activeThreadCanOperate || Boolean(busy)} onClick={() => void stopActiveTurn()}>Stop run</button><button type="button" className="text-button" disabled={!activeThreadCanOperate || Boolean(busy)} onClick={() => setSteering(true)}>Steer this turn</button></div></div>}
            {steering && <div className="steer-notice" role="status"><span>Next message will steer active turn {text(activeTurn ?? {}, 'id')}</span><button className="icon-button" aria-label="Cancel steering" onClick={() => setSteering(false)}><X size={15} /></button></div>}
            <form className="composer" onSubmit={sendMessage}><textarea aria-label="Message" placeholder={online ? activeThread ? 'Message your agent…' : 'Start with a message…' : 'Write a draft to save on this device…'} value={compose} onChange={event => { composeRef.current = event.target.value; chatDraftOperationIdRef.current = ''; setCompose(event.target.value); setChatDraftOperationId('') }} rows={2} /><div className="composer-controls">
              <label>Model<select aria-label="Model" value={modelValue} onChange={event => { setChatModel(event.target.value); setChatEffort('medium') }}><option value="">Runtime default</option>{activeThread && savedThreadModel && !availableModels.some(model => text(model, 'model', text(model, 'id')) === savedThreadModel) && <option value={savedThreadModel}>Saved model · {savedThreadModel}</option>}{availableModels.map((model, index) => { const id = text(model, 'model', text(model, 'id')); return id ? <option key={id || index} value={id}>{labelFor(model, 'name', 'displayName', 'model', 'id')}</option> : null })}</select></label>
              {!activeThread && newSession && <>
                <label>Project<select aria-label="Project" value={sessionProjectId} onChange={event => setSelectedProjectId(event.target.value)}><option value="">No project</option>{availableProjects.map(project => <option key={text(project, 'id')} value={text(project, 'id')}>{labelFor(project, 'name', 'id')}</option>)}</select></label>
                {ownerModelsLoading && <span role="status">Loading models from this node…</span>}
                {ownerModelsUnavailable && <span role="status">Model list unavailable. The runtime default is available.</span>}
              </>}
              <label>Reasoning<select aria-label="Reasoning" value={chatEffort} onChange={event => setChatEffort(event.target.value)}>{effectiveReasoningOptions.map(effort => <option key={effort} value={effort}>{effort}</option>)}</select></label>
              <label>Mode<select aria-label="Mode" value={chatMode} onChange={event => setChatMode(event.target.value as 'default' | 'plan')}><option value="default">Default</option><option value="plan">Plan</option></select></label>
              <label>Access<select aria-label="Access" value={chatAccess} disabled={Boolean(activeThread)} onChange={event => setChatAccess(event.target.value as 'workspace-write' | 'read-only')}><option value="workspace-write">Workspace write</option><option value="read-only">Read only</option></select></label>
            </div><div className="composer-footer"><span>{activeThread ? threadIsHistorical(activeThread) ? 'Historical conversation · live actions unavailable' : activeThreadCanOperate ? 'Access is fixed for this session' : 'Owner unavailable · save a local draft' : nodeIsAvailable(selectedNodeId, nodes) ? 'Execution stays on the owning node' : 'Choose an online node to run'}</span><button className="primary-button" aria-label={activeThreadCanOperate || (!activeThread && online && nodeIsAvailable(selectedNodeId, nodes)) ? activeThread ? 'Send message' : 'Start session' : 'Save draft'} disabled={!compose.trim() || Boolean(busy)}>{activeThreadCanOperate || (!activeThread && online && nodeIsAvailable(selectedNodeId, nodes)) ? <><Send size={16} /> {activeThread ? 'Send' : 'Start'}</> : <><FileClock size={16} /> Save draft</>}</button></div></form>
          </div> : <>
            <div className="page-heading"><div><p className="eyebrow">Conversations</p><h1>Chats</h1></div><button className="primary-button" disabled={!online || Boolean(busy)} onClick={() => { invalidateChatSelection(); activeThreadRef.current = null; setActiveThread(null); setSession(null); setLiveSession(null); setNewSession(true); composeRef.current = ''; setCompose('') }}><Plus size={16} /> New chat</button></div>
            <label className="search-field"><SearchIcon /><input aria-label="Search chats" placeholder="Find a conversation" value={search} onChange={event => setSearch(event.target.value)} /></label>
            {filteredThreads.length ? <div className="list-card">{filteredThreads.map(thread => <button className="list-row" key={text(thread, 'id')} onClick={() => void activateThread(thread)}><span className="row-icon"><MessageSquare size={17} /></span><span className="row-main"><strong>{labelFor(thread, 'title', 'preview', 'id')}</strong><small>{labelFor(thread, 'preview', 'updatedAt')} · {nodeLabel(owner(thread, primaryNodeId), nodes)}</small></span><span className="row-trailing"><small>{formatDate(thread.updatedAt)}</small><ChevronLeft className="chevron-right" size={16} /></span></button>)}</div> : <EmptyState icon={<MessageSquare />} title="No conversations yet" detail="Start from a named agent or continue a conversation from your primary node." />}
            <div className="mobile-card new-chat-card"><div className="section-title"><div><p className="eyebrow">Start something new</p><h2>Choose where to run</h2></div><ShieldCheck size={20} /></div><p>New sessions require an explicit execution node. Your phone never runs tools or moves an active session.</p><label className="field-label">Execution node<select aria-label="Execution node" value={selectedNodeId} onChange={event => { setSelectedNodeId(event.target.value); setSelectedProjectId(''); setChatModel(null) }}>{nodes.map(node => <option key={nodeId(node)} value={nodeId(node)} disabled={!nodeIsAvailable(nodeId(node), nodes)}>{labelFor(node, 'name', 'nodeId')} {nodeIsAvailable(nodeId(node), nodes) ? '' : '· offline or unavailable'}</option>)}</select></label><button className="secondary-button" disabled={!online || !nodeIsAvailable(selectedNodeId, nodes) || Boolean(busy)} onClick={() => { invalidateChatSelection(); activeThreadRef.current = null; setActiveThread(null); setSession(null); setLiveSession(null); composeRef.current = ''; setCompose(''); setChatModel(null); setSelectedProjectId(''); setNewSession(true) }}><Plus size={16} /> New session</button></div>
          </>}
        </section>}

        {tab === 'agents' && <section className="page" aria-label="Agents">
          {!selectedAgent ? <><div className="page-heading"><div><p className="eyebrow">Persistent identities</p><h1>Agents</h1></div><button className="primary-button" disabled={!newAgentNodeOnline || Boolean(busy)} onClick={() => void startAgentInterview()}><Sparkles size={16} /> New agent</button></div><div className="mobile-card new-chat-card"><label className="field-label">New agent execution node<select aria-label="New agent execution node" value={selectedNodeId} onChange={event => setSelectedNodeId(event.target.value)}>{nodes.map(node => { const id = nodeId(node); const eligible = nodeIsAvailable(id, nodes); return <option key={id} value={id} disabled={!eligible}>{labelFor(node, 'name', 'nodeId')}{eligible ? '' : ' · offline or unavailable'}</option> })}</select></label><p className="helper-copy">The interview runs on this online node. Agent identity origin is recorded separately.</p></div>
            {newProfileDrafts.length > 0 && <div className="subsection new-profile-reviews"><div className="section-title"><h2>New profile reviews</h2><span className="count-chip">{newProfileDrafts.length}</span></div>{newProfileDrafts.map(draft => { const candidate = isRow(draft.candidate) ? draft.candidate : {}; return <article className="review-card" key={text(draft, 'id')}><p className="eyebrow">New agent proposal · {text(draft, 'status')}</p><h3>{labelFor(candidate, 'name')}</h3><p>{text(candidate, 'description')}</p><p>Review hash · <code>{text(draft, 'hash')}</code></p><details><summary>Review full profile</summary><h4>Instructions</h4><pre>{text(candidate, 'instructions')}</pre><h4>Soul</h4><pre>{text(candidate, 'soul')}</pre></details><div className="inline-actions"><button className="secondary-button" disabled={!newAgentNodeOnline} onClick={() => void reviewNewProfile(draft, false)}>Reject proposal</button><button className="primary-button" disabled={!newAgentNodeOnline} onClick={() => void reviewNewProfile(draft, true)}><Check size={15} /> Approve and create</button></div></article> })}</div>}
            {agents.length ? <div className="list-card">{agents.map(agent => <button className="list-row" key={text(agent, 'id')} onClick={() => void openAgent(agent)}><span className="row-icon agent-icon"><Bot size={18} /></span><span className="row-main"><strong>{labelFor(agent, 'name')}</strong><small>{labelFor(agent, 'description', 'Agent profile')}</small></span><span className="row-trailing"><small>{text(agent, 'model')}</small><ChevronLeft className="chevron-right" size={16} /></span></button>)}</div> : <EmptyState icon={<Bot />} title="No agents yet" detail="Create a persistent agent through a guided interview. Review the proposed profile before saving it." action={<button className="primary-button" disabled={!online || Boolean(busy)} onClick={() => void startAgentInterview()}><Sparkles size={16} /> Start an interview</button>} />}
          </> : <AgentDetail agent={selectedAgent} nodes={nodes} executionNodeId={effectiveAgentNodeId} originNodeId={text(selectedAgent, 'originNodeId', text(selectedAgent, 'ownerNodeId', primaryNodeId))} onExecutionNodeChange={nodeId => { setAgentExecutionNodeId(nodeId); setForgottenMemoryEvent('') }} pane={agentPane} rows={rows} busy={busy} drafts={drafts} taskNextCursor={taskNextCursor} loadingMoreTasks={loadingMoreTasks} onLoadMoreTasks={loadMoreTasks} draftTask={draftTask} schedule={taskSchedule} memoryEdit={memoryEdit} forgottenMemoryEvent={forgottenMemoryEvent} onBack={() => setSelectedAgent(null)} onPane={setAgentPane} onOpen={openAgentConversation} onInterview={editAgentProfile} onTaskChange={setDraftTask} onTaskSubmit={createTask} onScheduleChange={setTaskSchedule} onScheduleSubmit={saveSchedule} onMemoryEdit={setMemoryEdit} onMemorySave={saveMemory} onMemoryForget={forgetMemory} onMemoryUndo={undoForgetMemory} onPublishDraft={publishDefinitionDraft} onProfileAction={async (draft, approve) => { try { await runRequest('agents', approve ? 'agents.approve' : 'agents.reject', { draftId: text(draft, 'id'), hash: text(draft, 'hash') }, { ...revisionOptions(draft, 'hash'), targetNodeId: effectiveAgentNodeId }); await refresh(); await loadAgentPane('profile') } catch (error) { setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not update profile review.' }) } }} onRevokeSkill={async skill => { try { await runRequest('agents', 'skills.revoke', { id: text(skill, 'id'), agentId: text(selectedAgent ?? {}, 'id') }, { ...revisionOptions(skill), targetNodeId: effectiveAgentNodeId }); await loadAgentPane('skills') } catch (error) { setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not revoke the skill.' }) } }} onTaskAction={async (task, action) => { try { const proposal = isDelegationProposal(task); const workflow = 'expression' in task; const method = proposal ? 'tasks.approve' : workflow ? 'workflows.control' : 'tasks.control'; const params = proposal ? { proposalId: text(task, 'id'), hash: text(task, 'hash') } : workflow ? { workflowId: text(task, 'id'), paused: action === 'pause' } : { taskId: text(task, 'id'), action }; await runRequest('agents', method, params, { operationId: createOperationId(), ...revisionOptions(task), expectedEpoch: text(task, 'ownerEpoch', text(task, 'epoch', nodeEpoch(task, snapshot))), targetNodeId: owner(task, effectiveAgentNodeId) }); await loadAgentPane('tasks') } catch (error) { setNotice({ kind: 'error', message: error instanceof Error ? error.message : 'Could not update the task.' }) } }} online={agentNodeOnline} />}
        </section>}

        {tab === 'attention' && <AttentionPage questions={questions} approvals={approvals} nodes={asRows(snapshot?.nodes)} online={online} profile={profile} client={client} onRefresh={() => void refreshAttention()} onSaveDraft={saveQuestionDraft} onAnswer={answerQuestion} onDismiss={dismissQuestion} onApprove={answerApproval} onOpenThread={thread => void activateThread(thread)} />}

        {tab === 'fleet' && <FleetPage profile={profile} snapshot={snapshot} online={online} profiles={profiles} onRefresh={() => void refreshAndSubscribe()} onForget={forgetPairedNode} runRequest={runRequest} />}
      </>}
    </main>

    <nav className="tabbar" role="tablist" aria-label="Main navigation">
      <TabButton id="chats" active={tab === 'chats'} disabled={!profile} onClick={() => setTab('chats')} icon={<MessageSquare />} label="Chats" badge={threads.length} />
      <TabButton id="agents" active={tab === 'agents'} disabled={!profile} onClick={() => setTab('agents')} icon={<Bot />} label="Agents" badge={agents.length} />
      <TabButton id="attention" active={tab === 'attention'} disabled={!profile} onClick={() => { setTab('attention'); void refreshAttention() }} icon={<Activity />} label="Attention" badge={questions.filter(q => text(q, 'resolution', 'pending') === 'pending').length + approvals.length} />
      <TabButton id="fleet" active={tab === 'fleet'} disabled={false} onClick={() => setTab('fleet')} icon={<Users />} label="Fleet" badge={profiles.length} />
    </nav>
  </div>
}

function PairingScreen({ invitation, name, busy, onInvitation, onName, onSubmit, native, scanner }: { invitation: string; name: string; busy: boolean; onInvitation: (value: string) => void; onName: (value: string) => void; onSubmit: (event: FormEvent) => void; native: boolean; scanner: Scanner }) {
  const [scanning, setScanning] = useState(false)
  const scanGeneration = useRef(0)
  const [scanNotice, setScanNotice] = useState('')
  useEffect(() => {
    document.body.classList.toggle('scanner-mode', scanning)
    return () => document.body.classList.remove('scanner-mode')
  }, [scanning])
  const startScan = async () => {
    if (!native || scanning) return
    const generation = ++scanGeneration.current
    setScanning(true)
    setScanNotice('')
    try {
      const result = await scanner.scan({ formats: [Format.QRCode], windowed: true })
      if (scanGeneration.current !== generation) return
      parseInvitation(result.content)
      onInvitation(result.content)
      setScanNotice('QR invitation scanned. Review the details, then pair this phone.')
    } catch (error) {
      if (scanGeneration.current !== generation) return
      const reason = error instanceof Error ? error.message.toLowerCase() : String(error).toLowerCase()
      if (!reason.includes('cancel')) setScanNotice('Could not scan that QR code. Check camera access or paste the HTTPS pairing invitation instead.')
    } finally {
      if (scanGeneration.current === generation) setScanning(false)
    }
  }
  const stopScan = async () => {
    if (!scanning) return
    ++scanGeneration.current
    setScanning(false)
    try { await scanner.cancel() } catch { /* The scan may have completed between the tap and cancellation. */ }
  }
  return <section className={`pairing-screen${scanning ? ' is-scanning' : ''}`} aria-label="Pair a primary node">
    <div className="pairing-mark"><Logo animated={false} /></div>
    <p className="eyebrow">Your agents, wherever they run</p>
    <h1>Pair your Brr node</h1>
    <p className="pairing-intro">Connect this phone to your primary over your local network or private VPN. Your conversations and agents stay on their owning nodes.</p>
    <form onSubmit={onSubmit} className="pairing-form">
      <label className="field-label" htmlFor="pairing-invitation">Pairing link or invitation</label>
      <textarea id="pairing-invitation" aria-label="Pairing link or invitation" placeholder={'Paste a brr://pair link or invitation JSON'} rows={4} value={invitation} onChange={event => onInvitation(event.target.value)} autoCapitalize="none" autoCorrect="off" spellCheck={false} />
      {native && <button className="secondary-button scan-button" type="button" disabled={busy || scanning} onClick={() => void startScan()}><ScanLine size={17} /> Scan QR code</button>}
      {scanNotice && <p className="scan-notice" role="status">{scanNotice}</p>}
      <label className="field-label" htmlFor="device-name">Phone name <span className="optional-label">Optional</span></label>
      <input id="device-name" value={name} onChange={event => onName(event.target.value)} placeholder="e.g. Nik's iPhone" maxLength={48} />
      <button className="primary-button pair-button" disabled={!invitation.trim() || busy || !native}>{busy ? <><RefreshCw className="spin" size={17} /> Pairing securely…</> : <><ShieldCheck size={17} /> Pair this phone</>}</button>
      {!native && <p className="native-note">Open this screen in the Brr mobile app to pair securely.</p>}
    </form>
    {scanning && <div className="scanner-overlay" role="dialog" aria-modal="true" aria-label="Scan pairing QR code"><div className="scanner-frame" aria-hidden="true" /><p>Point your camera at the Brr pairing QR code.</p><button className="secondary-button" type="button" onClick={() => void stopScan()}><X size={16} /> Cancel scan</button></div>}
    <p className="trust-note"><ShieldCheck size={15} /> Invitations expire after one use. The server certificate and device secret stay in iOS Keychain.</p>
  </section>
}

function TabButton({ id, active, disabled, onClick, icon, label, badge }: { id: Tab; active: boolean; disabled: boolean; onClick: () => void; icon: React.ReactNode; label: string; badge?: number }) {
  return <button className={`tab-button${active ? ' is-active' : ''}`} role="tab" aria-selected={active} aria-controls={`tab-panel-${id}`} disabled={disabled} onClick={onClick}><span className="tab-icon">{icon}{Boolean(badge) && <i>{badge! > 9 ? '9+' : badge}</i>}</span><span>{label}</span></button>
}

function SearchIcon() { return <svg aria-hidden="true" viewBox="0 0 20 20"><circle cx="8.8" cy="8.8" r="5.7" /><path d="m13 13 4 4" /></svg> }

function EmptyState({ icon, title, detail, action }: { icon: React.ReactNode; title: string; detail: string; action?: React.ReactNode }) {
  return <div className="empty-state"><span className="empty-icon">{icon}</span><h2>{title}</h2><p>{detail}</p>{action}</div>
}

function TranscriptItem({ item }: { item: Row }) {
  const type = text(item, 'type')
  const user = type.toLowerCase().includes('user')
  const userText = user && Array.isArray(item.content)
    ? item.content.filter((block): block is Row => isRow(block) && block.type === 'text' && typeof block.text === 'string').map(block => text(block, 'text')).join('\n')
    : ''
  const content = userText || text(item, 'text', text(item, 'message', text(item, 'aggregatedOutput')))
  if (!content) return null
  return <article className={`transcript-item${user ? ' is-user' : ''}`}><span className="transcript-avatar">{user ? <Users size={14} /> : <Bot size={15} />}</span><div className="transcript-copy"><p className="transcript-speaker">{user ? 'You' : 'Agent'}</p><p>{content}</p></div></article>
}

function AgentDetail({ agent, nodes, executionNodeId, originNodeId, onExecutionNodeChange, pane, rows, busy, drafts, taskNextCursor, loadingMoreTasks, onLoadMoreTasks, draftTask, schedule, memoryEdit, forgottenMemoryEvent, online, onBack, onPane, onOpen, onInterview, onTaskChange, onTaskSubmit, onScheduleChange, onScheduleSubmit, onMemoryEdit, onMemorySave, onMemoryForget, onMemoryUndo, onPublishDraft, onProfileAction, onRevokeSkill, onTaskAction }: {
  agent: Row; nodes: Row[]; executionNodeId: string; originNodeId: string; onExecutionNodeChange: (nodeId: string) => void; pane: AgentPane; rows: Row[]; busy: string; drafts: Row[]; taskNextCursor: string | null; loadingMoreTasks: boolean; onLoadMoreTasks: () => void; draftTask: { title: string; instructions: string; acceptance: string }; schedule: { name: string; title: string; instructions: string; acceptance: string; cron: string; timezone: string }; memoryEdit: Row | null; forgottenMemoryEvent: string; online: boolean;
  onBack: () => void; onPane: (pane: AgentPane) => void; onOpen: (thread?: Row) => void; onInterview: () => void; onTaskChange: (value: { title: string; instructions: string; acceptance: string }) => void; onTaskSubmit: (event: FormEvent) => void;
  onScheduleChange: (value: { name: string; title: string; instructions: string; acceptance: string; cron: string; timezone: string }) => void; onScheduleSubmit: (event: FormEvent) => void;
  onMemoryEdit: (value: Row | null) => void; onMemorySave: (value: Row) => void; onMemoryForget: (memory: Row) => void; onMemoryUndo: () => void; onPublishDraft: (draft: Row) => void; onProfileAction: (draft: Row, approve: boolean) => void; onRevokeSkill: (skill: Row) => void; onTaskAction: (task: Row, action: string) => void;
}) {
  const [confirmForgetId, setConfirmForgetId] = useState('')
  const agentName = labelFor(agent, 'name')
  const eligibleNodeIds = new Set(availableAgentNodeIds(agent, nodes, originNodeId))
  const profileTabs: [AgentPane, string][] = [['chat', 'Chats'], ['tasks', 'Tasks & schedules'], ['memory', 'Memory'], ['skills', 'Skills'], ['profile', 'Profile']]
  return <div className="agent-detail">
    <div className="page-heading detail-heading"><button className="back-button" aria-label="Back to agents" onClick={onBack}><ChevronLeft /></button><div className="heading-copy"><p className="eyebrow">Persistent agent</p><h1>{agentName}</h1></div><button className="icon-button" aria-label="Agent actions" title="Agent actions"><MoreHorizontal /></button></div>
    <div className="agent-summary"><span className="avatar-large"><Bot size={22} /></span><div><strong>{agentName}</strong><p>{text(agent, 'description', 'No description yet.')}</p><small>{text(agent, 'model', 'Default model')} · {text(agent, 'effort', 'medium')} reasoning</small><small>Identity origin · {originNodeId}</small></div></div>
    <div className="mobile-card agent-node-card"><label className="field-label">Agent execution node<select aria-label="Agent execution node" value={executionNodeId} onChange={event => onExecutionNodeChange(event.target.value)}>{nodes.map(node => { const id = nodeId(node); const eligible = eligibleNodeIds.has(id); return <option key={id} value={id} disabled={!eligible}>{labelFor(node, 'name', 'nodeId')}{eligible ? '' : node.online === false ? ' · offline' : ' · not available for this agent'}</option> })}</select></label><p className="helper-copy">Management and new work run only on an available node. Existing work stays with its owner.</p>{!online && <p className="offline-help">This node is offline or unavailable. Reads are unavailable; edits stay on this device until you review them.</p>}</div>
    <div className="segmented-tabs" role="tablist" aria-label="Agent sections">{profileTabs.map(([id, label]) => <button key={id} role="tab" aria-selected={pane === id} onClick={() => onPane(id)}>{label}</button>)}</div>
    {pane === 'chat' && <div className="pane-content"><div className="section-title"><h2>Conversations</h2><button className="secondary-button" disabled={!online || Boolean(busy)} onClick={() => onOpen()}><MessageSquare size={15} /> Open chat</button></div>{rows.length ? <div className="list-card">{rows.map(row => <button className="list-row" key={text(row, 'id')} onClick={() => onOpen(row)}><span className="row-icon"><MessageSquare size={16} /></span><span className="row-main"><strong>{labelFor(row, 'title', 'preview')}</strong><small>{formatDate(row.updatedAt)}</small></span><ChevronLeft className="chevron-right" size={16} /></button>)}</div> : <EmptyState icon={<MessageSquare />} title="No conversations" detail="Open a conversation with this agent to ask a question or start a task." action={<button className="primary-button" disabled={!online || Boolean(busy)} onClick={() => onOpen()}><MessageSquare size={16} /> Start conversation</button>} />}</div>}
    {pane === 'tasks' && <div className="pane-content"><h2>Tasks & schedules</h2>{taskNextCursor && <button type="button" className="secondary-button" disabled={!online || loadingMoreTasks} onClick={onLoadMoreTasks}>{loadingMoreTasks ? 'Loading tasks…' : 'Load more tasks'}</button>}<form className="edit-form" onSubmit={onTaskSubmit}><h3>New task</h3><label className="field-label">Title<input required value={draftTask.title} onChange={event => onTaskChange({ ...draftTask, title: event.target.value })} /></label><label className="field-label">Instructions<textarea required rows={3} value={draftTask.instructions} onChange={event => onTaskChange({ ...draftTask, instructions: event.target.value })} /></label><label className="field-label">Acceptance criteria <small>One per line</small><textarea required rows={3} value={draftTask.acceptance} onChange={event => onTaskChange({ ...draftTask, acceptance: event.target.value })} /></label><button className="primary-button" disabled={Boolean(busy)}><Plus size={16} /> {online ? 'Create task' : 'Save task draft'}</button>{!online && <p className="offline-help">Saved locally until you review it and create it on the selected node.</p>}</form>
      {drafts.length > 0 && <div className="subsection"><h3>Saved offline definitions</h3>{drafts.map(draft => <div className="compact-row" key={text(draft, 'id')}><FileClock size={17} /><div><strong>{labelFor(isRow(draft.params) ? draft.params : draft, 'title', 'name')}</strong><small>{text(draft, 'kind')} · Review before creating · {formatDate(draft.createdAt)}</small></div><button className="secondary-button" disabled={!online} onClick={() => onPublishDraft(draft)}>Review & create</button></div>)}</div>}
      <div className="subsection"><h3>Tasks</h3>{rows.filter(row => !isDelegationProposal(row) && ('status' in row || 'acceptanceCriteria' in row)).length ? <div className="list-card">{rows.filter(row => !isDelegationProposal(row) && ('status' in row || 'acceptanceCriteria' in row)).map(task => <div className="task-row" key={text(task, 'id')}><span className="status-dot" data-status={text(task, 'status')} /><div className="row-main"><strong>{labelFor(task, 'title')}</strong><small>{text(task, 'status')} · {formatDate(task.updatedAt)}</small>{text(task, 'reason') && <p>{text(task, 'reason')}</p>}</div><select aria-label={`Actions for ${labelFor(task, 'title')}`} value="" disabled={!online} onChange={event => { if (event.target.value) onTaskAction(task, event.target.value); event.currentTarget.value = '' }}><option value="">Actions</option>{['pause', 'resume', 'cancel', 'retry'].map(action => <option key={action}>{action}</option>)}</select></div>)}</div> : <p className="muted-copy">No tasks assigned to this agent.</p>}</div>
      <form className="edit-form" onSubmit={onScheduleSubmit}><h3>Schedule a task</h3><label className="field-label">Schedule name<input required value={schedule.name} onChange={event => onScheduleChange({ ...schedule, name: event.target.value })} /></label><label className="field-label">Task title<input required value={schedule.title} onChange={event => onScheduleChange({ ...schedule, title: event.target.value })} /></label><label className="field-label">Instructions<textarea required rows={2} value={schedule.instructions} onChange={event => onScheduleChange({ ...schedule, instructions: event.target.value })} /></label><label className="field-label">Schedule acceptance criteria <small>One per line</small><textarea required rows={2} value={schedule.acceptance} onChange={event => onScheduleChange({ ...schedule, acceptance: event.target.value })} /></label><div className="form-grid"><label className="field-label">Cron expression<input required value={schedule.cron} onChange={event => onScheduleChange({ ...schedule, cron: event.target.value })} /></label><label className="field-label">Time zone<input required value={schedule.timezone} onChange={event => onScheduleChange({ ...schedule, timezone: event.target.value })} /></label></div><button className="secondary-button"><Clock3 size={16} /> {online ? 'Save schedule' : 'Save schedule draft'}</button>{!online && <p className="offline-help">Saved locally for review before it is created on the owning node.</p>}</form>
      {rows.some(row => 'expression' in row) && <div className="subsection"><h3>Schedules</h3>{rows.filter(row => 'expression' in row).map(workflow => <div className="compact-row" key={text(workflow, 'id')}><Clock3 size={17} /><div><strong>{labelFor(workflow, 'name')}</strong><small>{text(workflow, 'expression')} · {text(workflow, 'timezone')}</small></div><span>{workflow.paused ? 'Paused' : 'Active'}</span><button className="secondary-button" disabled={!online} onClick={() => onTaskAction(workflow, workflow.paused ? 'resume' : 'pause')}>{workflow.paused ? 'Resume' : 'Pause'}</button></div>)}</div>}
      {rows.filter(isDelegationProposal).map(proposal => <article className="review-card" key={text(proposal, 'id')}><p className="eyebrow">Task proposal · {text(proposal, 'status', 'pending')}</p><h3>Delegation proposal · {text(proposal, 'parentTaskId')}</h3><p>Review hash · <code>{text(proposal, 'hash')}</code></p><details><summary>Review task graph ({asRows(proposal.tasks).length} tasks)</summary>{asRows(proposal.tasks).map(node => { const task = isRow(node.task) ? node.task : {}; return <div className="proposal-task" key={text(node, 'key')}><strong>{labelFor(task, 'title')}</strong><p>{text(task, 'instructions')}</p><small>Acceptance · {asRows(task.acceptanceCriteria).map(String).join(', ') || (Array.isArray(task.acceptanceCriteria) ? task.acceptanceCriteria.join(', ') : '')}</small><small>Depends on · {Array.isArray(node.dependsOn) ? node.dependsOn.join(', ') || 'None' : 'None'}</small></div> })}</details><div className="inline-actions"><button className="primary-button" disabled={!online} onClick={() => onTaskAction(proposal, 'approve')}><Check size={15} /> Approve proposal</button></div></article>)}
    </div>}
    {pane === 'memory' && <div className="pane-content"><div className="section-title"><h2>Memory</h2><span className="count-chip">{rows.length}</span>{forgottenMemoryEvent && <button className="text-button" disabled={!online} onClick={onMemoryUndo}>Undo last forget</button>}</div><p className="helper-copy">Edit learned facts with their source links and revision. Memory is shared through reviewed Git records.</p>{rows.length ? <div className="memory-list">{rows.map(memory => <article className="memory-card" key={text(memory, 'id')}><div className="memory-meta"><span>{text(memory, 'kind', 'fact')}</span><small>rev {text(memory, 'revision')}</small></div>{memoryEdit?.id === memory.id ? <><textarea aria-label="Edit memory" rows={5} defaultValue={text(memory, 'content')} onChange={event => onMemoryEdit({ ...memoryEdit, content: event.target.value })} /><div className="inline-actions"><button className="secondary-button" onClick={() => onMemoryEdit(null)}>Cancel</button><button className="primary-button" onClick={() => onMemorySave(memoryEdit!)}>Save revision</button></div></> : <><p>{text(memory, 'content')}</p><div className="memory-sources">{asRows(memory.sources).map(source => <span key={text(source, 'sourceId')}>Source · {text(source, 'threadId', text(source, 'sourceId'))}</span>)}</div><div className="inline-actions"><button className="text-button" onClick={() => onMemoryEdit(memory)}>Edit memory</button>{confirmForgetId === text(memory, 'id') ? <><button className="secondary-button" onClick={() => setConfirmForgetId('')}>Keep memory</button><button className="secondary-button danger-text" disabled={!online} onClick={() => { setConfirmForgetId(''); onMemoryForget(memory) }}>Confirm forget</button></> : <button className="text-button danger-text" disabled={!online} onClick={() => setConfirmForgetId(text(memory, 'id'))}>Forget memory</button>}</div></>}</article>)}</div> : <EmptyState icon={<Sparkles />} title="No learned memory" detail="New evidence creates scoped memory. Historical conversations are never backfilled automatically." />}</div>}
    {pane === 'skills' && <div className="pane-content"><div className="section-title"><h2>Learned skills</h2><span className="count-chip">{rows.length}</span></div><p className="helper-copy">Skills are reviewed, versioned notes this agent can apply to future work.</p>{rows.length ? <div className="list-card">{rows.map(skill => <article className="skill-row" key={text(skill, 'id')}><div className="skill-mark"><Code2 size={17} /></div><div className="row-main"><strong>{labelFor(skill, 'name')}</strong><small>{text(skill, 'description')}</small><details><summary>Read skill</summary><pre>{text(skill, 'content')}</pre></details></div><button className="icon-button danger-icon" aria-label={`Revoke ${labelFor(skill, 'name')}`} disabled={!online} onClick={() => onRevokeSkill(skill)}><Trash2 size={16} /></button></article>)}</div> : <EmptyState icon={<Code2 />} title="No learned skills" detail="Skills appear after this agent learns a reusable, reviewed procedure." />}</div>}
    {pane === 'profile' && <div className="pane-content"><div className="section-title"><h2>Profile & review</h2><button className="secondary-button" disabled={!online || Boolean(busy)} onClick={onInterview}><Sparkles size={15} /> Edit profile</button></div><div className="profile-card"><label className="field-label">Name<input value={text(agent, 'name')} readOnly /></label><label className="field-label">Description<textarea rows={3} value={text(agent, 'description')} readOnly /></label><label className="field-label">Instructions<textarea rows={7} value={text(agent, 'instructions')} readOnly /></label><label className="field-label">Soul<textarea rows={5} value={text(agent, 'soul')} readOnly /></label><p className="helper-copy">Changes are proposed in an interview and require review before replacing this profile.</p></div>{rows.filter(isReviewableProfileDraft).map(draft => <article className="review-card" key={text(draft, 'id')}><p className="eyebrow">Profile proposal · {text(draft, 'status', 'pending')}</p><h3>{labelFor(isRow(draft.candidate) ? draft.candidate : {}, 'name')}</h3><p>{text(isRow(draft.candidate) ? draft.candidate : {}, 'description')}</p><details><summary>Review full instructions</summary><pre>{text(isRow(draft.candidate) ? draft.candidate : {}, 'instructions')}</pre><pre>{text(isRow(draft.candidate) ? draft.candidate : {}, 'soul')}</pre></details><div className="inline-actions"><button className="secondary-button" disabled={!online} onClick={() => onProfileAction(draft, false)}>Discard proposal</button><button className="primary-button" disabled={!online} onClick={() => onProfileAction(draft, true)}><Check size={15} /> Approve profile</button></div></article>)}</div>}
  </div>
}

function AttentionPage({ questions, approvals, nodes, online, profile, client, onRefresh, onSaveDraft, onAnswer, onDismiss, onApprove, onOpenThread }: { questions: Row[]; approvals: Row[]; nodes: Row[]; online: boolean; profile: Profile; client: RemoteClient; onRefresh: () => void; onSaveDraft: (question: Row, answers: QuestionAnswers) => Promise<void>; onAnswer: (question: Row, answers: QuestionAnswers) => Promise<void>; onDismiss: (question: Row) => Promise<void>; onApprove: (approval: Row, result: Record<string, unknown>) => Promise<void>; onOpenThread: (thread: Row) => void }) {
  const [localDrafts, setLocalDrafts] = useState<Record<string, QuestionAnswers>>({})
  useEffect(() => {
    let active = true
    for (const question of questions) {
      const id = text(question, 'id')
      if (text(question, 'resolution', 'pending') !== 'pending') continue
      void client.cacheGet<QuestionAnswers>(`draft:question:${profile.id}:${id}`).then(draft => {
        if (active && isRow(draft)) setLocalDrafts(current => ({ ...current, [id]: draft as QuestionAnswers }))
      }).catch(() => undefined)
    }
    return () => { active = false }
  }, [client, profile.id, questions])
  const pending = questions.filter(question => text(question, 'resolution', 'pending') === 'pending')
  const waiting = approvals.filter(approval => !['resolved', 'answered', 'dismissed'].includes(text(approval, 'status', 'pending')))
  return <section className="page attention-page" aria-label="Attention">
    <div className="page-heading"><div><p className="eyebrow">Needs your decision</p><h1>Attention</h1></div><button className="icon-button" aria-label="Refresh attention" onClick={onRefresh} disabled={!online}><RefreshCw size={17} /></button></div>
    {!online && <div className="offline-banner"><CloudOff size={16} /> Live questions and approvals need a connection. You can still review the last saved snapshot.</div>}
    <div className="section-title"><h2>Questions</h2><span className="count-chip">{pending.length}</span></div>
    {pending.length ? pending.map(question => {
      const id = text(question, 'id')
      const thread = { id: text(question, 'threadId'), title: text(asRows(question.questions)[0] ?? {}, 'header', 'Agent question'), ownerNodeId: owner(question, profile.nodeId) }
      const request = toQuestionRequest(question, localDrafts[id])
      const responseAvailable = online && nodeIsAvailable(owner(question, profile.nodeId), nodes)
      const onSave = async (answers: QuestionAnswers) => {
        setLocalDrafts(current => ({ ...current, [id]: answers }))
        await onSaveDraft(question, answers)
      }
      return <article className="attention-card" key={id}>
        <div className="attention-meta"><span className="attention-type"><CircleHelp size={15} /> Agent question</span><span className="attention-owner" title={`Node ID ${owner(question, profile.nodeId)}`}>{nodeLabel(owner(question, profile.nodeId), nodes)}</span></div>
        <p className="helper-copy">{request.kind === 'blocking' || question.waiting ? 'The agent is waiting for your response.' : 'The agent can continue while you answer.'}</p>
        <button type="button" className="text-button" onClick={() => onOpenThread(thread)}>Open conversation</button>
        <QuestionForm request={request} origin={text(question, 'agentName', 'Agent question')} onSaveDraft={onSave} onAnswer={answers => onAnswer(question, answers)} onDismiss={() => onDismiss(question)} responseAvailable={responseAvailable} />
        {!responseAvailable && <p className="offline-banner">The owning node is unavailable. Your answer draft stays on this device.</p>}
      </article>
    }) : <EmptyState icon={<CircleHelp />} title="No open questions" detail="Blocking questions and async answers from every node appear here." />}
    <div className="section-title attention-section"><h2>Approvals</h2><span className="count-chip">{waiting.length}</span></div>
    {waiting.length ? waiting.map(approval => {
      const requestId = approval.requestId ?? approval.id
      const key = `${owner(approval, profile.nodeId)}:${text(approval, 'ownerEpoch', text(approval, 'epoch'))}:${typeof requestId}:${String(requestId)}`
      const approvalOwnerId = owner(approval, profile.nodeId)
      return <ApprovalReview key={key} approval={approval} ownerId={approvalOwnerId} ownerName={nodeLabel(approvalOwnerId, nodes)} online={online && nodeIsAvailable(approvalOwnerId, nodes)} onRespond={result => onApprove(approval, result)} />
    }) : <EmptyState icon={<ShieldCheck />} title="No approvals waiting" detail="Tool permissions are shown here with the owning node and original request ID." />}
  </section>
}

function toQuestionRequest(row: Row, localDraft?: QuestionAnswers): QuestionRequest {
  const questions = asRows(row.questions).map(question => ({
    id: text(question, 'id'), header: text(question, 'header'), question: text(question, 'question'),
    options: asRows(question.options).map(option => ({ label: text(option, 'label'), description: text(option, 'description') })),
  }))
  const kind = text(row, 'kind') === 'blocking' ? 'blocking' : 'async'
  const resolution = ['answered', 'dismissed', 'closed'].includes(text(row, 'resolution')) ? text(row, 'resolution') as QuestionRequest['resolution'] : 'pending'
  const delivery = ['queued', 'dispatching', 'delivered', 'uncertain'].includes(text(row, 'delivery')) ? text(row, 'delivery') as QuestionRequest['delivery'] : 'none'
  return {
    id: text(row, 'id'), threadId: text(row, 'threadId'), turnId: text(row, 'turnId'), itemId: text(row, 'itemId'),
    ...(text(row, 'agentId') ? { agentId: text(row, 'agentId') } : {}), ...(text(row, 'taskId') ? { taskId: text(row, 'taskId') } : {}),
    kind, questions, draft: { ...(isRow(row.draft) ? row.draft as QuestionAnswers : {}), ...(localDraft ?? {}) },
    ...(isRow(row.answers) ? { answers: row.answers as QuestionAnswers } : {}), resolution, delivery,
    ...(text(row, 'reason') ? { reason: text(row, 'reason') } : {}), createdAt: number(row, 'createdAt'), updatedAt: number(row, 'updatedAt'),
    ...(text(row, 'runtimeGeneration') ? { runtimeGeneration: text(row, 'runtimeGeneration') } : {}),
    ...(row.runtimeRequestId !== undefined ? { runtimeRequestId: row.runtimeRequestId as string | number } : {}),
    waiting: row.waiting === true,
  }
}

function ApprovalReview({ approval, ownerId, ownerName, online, onRespond }: { approval: Row; ownerId: string; ownerName: string; online: boolean; onRespond: (result: Record<string, unknown>) => Promise<void> }) {
  const method = text(approval, 'method')
  const params = isRow(approval.params) ? approval.params : {}
  const requested = isRow(params.permissions) ? params.permissions : {}
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const respond = async (result: Record<string, unknown>) => {
    setBusy(true); setError('')
    try { await onRespond(result) }
    catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)) }
    finally { setBusy(false) }
  }
  const errorMessage = error && <p role="alert">{error}</p>
  const meta = <div className="attention-meta"><span className="attention-type"><ShieldCheck size={15} /> {method === 'item/permissions/requestApproval' ? 'Permission request' : method.includes('commandExecution') ? 'Command approval' : method.includes('fileChange') ? 'File change approval' : method === 'mcpServer/elicitation/request' ? 'MCP server request' : 'Runtime request'}</span><span className="attention-owner" title={`Node ID ${ownerId}`}>{ownerName}</span></div>
  if (method === 'item/commandExecution/requestApproval' || method === 'item/fileChange/requestApproval') return <article className="attention-card approval-card">
    {meta}<h3>{method.includes('commandExecution') ? 'Review command before it runs' : 'Review file change before it applies'}</h3>
    {errorMessage}{text(params, 'reason') && <p>{text(params, 'reason')}</p>}{text(params, 'command') && <pre>{text(params, 'command')}</pre>}
    {text(params, 'cwd') && <p className="project-path">Working directory · {text(params, 'cwd')}</p>}{text(params, 'grantRoot') && <p>File access · {text(params, 'grantRoot')}</p>}
    <div className="inline-actions"><button className="secondary-button" disabled={!online || busy} onClick={() => void respond({ decision: 'decline' })}><X size={15} /> Decline</button><button className="primary-button" disabled={!online || busy} onClick={() => void respond({ decision: 'accept' })}><Check size={15} /> {busy ? 'Sending…' : 'Accept'}</button></div>
  </article>
  if (method === 'item/permissions/requestApproval') return <article className="attention-card approval-card">
    {meta}<h3>Review requested permissions</h3>{errorMessage}{text(params, 'reason') && <p>{text(params, 'reason')}</p>}
    <pre aria-label="Requested permissions">{JSON.stringify(requested, null, 2)}</pre>
    <div className="inline-actions"><button className="secondary-button" disabled={!online || busy} onClick={() => void respond({ permissions: {}, scope: 'turn' })}><X size={15} /> Decline</button><button className="primary-button" disabled={!online || busy} onClick={() => void respond({ permissions: requested, scope: 'turn' })}><Check size={15} /> {busy ? 'Sending…' : 'Allow for this turn'}</button></div>
  </article>
  if (method === 'mcpServer/elicitation/request') return <McpApprovalReview approval={approval} online={online} onRespond={onRespond} />
  return <article className="attention-card approval-card">{meta}<h3>Unsupported runtime request</h3><p>This request cannot be answered from mobile. Open Brr on the owning node to review it.</p></article>
}

function McpApprovalReview({ approval, online, onRespond }: { approval: Row; online: boolean; onRespond: (result: Record<string, unknown>) => Promise<void> }) {
  const params = isRow(approval.params) ? approval.params : {}
  const schema = isRow(params.requestedSchema) ? params.requestedSchema : {}
  const properties = isRow(schema.properties) ? schema.properties : {}
  const [values, setValues] = useState<Record<string, unknown>>(() => Object.fromEntries(Object.entries(properties).flatMap(([name, field]) => {
    if (!isRow(field)) return []
    if (field.default !== undefined) return [[name, field.default]]
    if (field.type === 'boolean') return [[name, false]]
    if (field.type === 'array') return [[name, []]]
    if (schema.required && Array.isArray(schema.required) && schema.required.includes(name)) return [[name, '']]
    return []
  })))
  const mode = text(params, 'mode')
  const browserMode = mode === 'url' || mode === 'openai/userVerification'
  const supported = Object.values(properties).every(field => isRow(field) && ['string', 'number', 'integer', 'boolean', 'array'].includes(text(field, 'type', 'string')) && (field.type !== 'array' || isRow(field.items) && Array.isArray(field.items.enum)))
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const respond = async (result: Record<string, unknown>) => {
    setBusy(true); setError('')
    try { await onRespond(result) }
    catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)) }
    finally { setBusy(false) }
  }
  const submit = (event: FormEvent) => {
    event.preventDefault()
    const required = Array.isArray(schema.required) ? schema.required.filter((name): name is string => typeof name === 'string') : []
    if (required.some(name => values[name] === undefined || values[name] === '' || Array.isArray(values[name]) && values[name].length === 0)) { setError('Complete each required field before submitting.'); return }
    void respond({ action: 'accept', content: values, _meta: null })
  }
  return <article className="attention-card approval-card">
    <div className="attention-meta"><span className="attention-type"><ShieldCheck size={15} /> MCP server request</span><span>{owner(approval, '')}</span></div>
    <h3>{text(params, 'serverName', 'MCP server')} requests input</h3><p>{text(params, 'message', text(params, 'description'))}</p>
    {browserMode && <p>{mode === 'url' ? `Requested URL · ${text(params, 'url')}` : 'Device verification is not available on mobile.'}</p>}
    {error && <p role="alert">{error}</p>}
    {!browserMode && supported ? <form onSubmit={submit}>
      {Object.entries(properties).map(([name, field]) => {
        if (!isRow(field)) return null
        const title = text(field, 'title', name)
        const common = { id: `mcp-${String(approval.id)}-${name}`, 'aria-label': title }
        const enumOptions = Array.isArray(field.enum) ? field.enum.filter((value): value is string => typeof value === 'string') : null
        const control = field.type === 'boolean' ? <input {...common} type="checkbox" checked={values[name] === true} onChange={event => setValues(current => ({ ...current, [name]: event.target.checked }))} />
          : enumOptions ? <select {...common} value={String(values[name] ?? '')} onChange={event => setValues(current => ({ ...current, [name]: event.target.value }))}><option value="">Choose…</option>{enumOptions.map(value => <option key={value} value={value}>{value}</option>)}</select>
            : field.type === 'array' && isRow(field.items) && Array.isArray(field.items.enum) ? <select {...common} multiple value={Array.isArray(values[name]) ? values[name] as string[] : []} onChange={event => setValues(current => ({ ...current, [name]: Array.from(event.target.selectedOptions, option => option.value) }))}>{field.items.enum.filter((value): value is string => typeof value === 'string').map(value => <option key={value} value={value}>{value}</option>)}</select>
              : field.type === 'number' || field.type === 'integer' ? <input {...common} type="number" value={String(values[name] ?? '')} onChange={event => setValues(current => ({ ...current, [name]: event.target.value === '' ? '' : Number(event.target.value) }))} />
                : <input {...common} type="text" value={String(values[name] ?? '')} onChange={event => setValues(current => ({ ...current, [name]: event.target.value }))} />
        return <label className="field-label" htmlFor={common.id} key={name}>{title}{Array.isArray(schema.required) && schema.required.includes(name) ? ' · required' : ''}{text(field, 'description') && <small>{text(field, 'description')}</small>}{control}</label>
      })}
      <div className="inline-actions"><button type="button" className="text-button" disabled={!online || busy} onClick={() => void respond({ action: 'cancel', content: null, _meta: null })}>Cancel</button><button type="button" className="secondary-button" disabled={!online || busy} onClick={() => void respond({ action: 'decline', content: null, _meta: null })}>Decline</button><button type="submit" className="primary-button" disabled={!online || busy}>{busy ? 'Sending…' : 'Submit response'}</button></div>
    </form> : <><p>This request needs a form that mobile cannot safely present. Decline or cancel it here.</p><div className="inline-actions"><button className="secondary-button" disabled={!online || busy} onClick={() => void respond({ action: 'decline', content: null, _meta: null })}>Decline</button><button className="text-button" disabled={!online || busy} onClick={() => void respond({ action: 'cancel', content: null, _meta: null })}>Cancel</button></div></>}
  </article>
}

function FleetPage({ profile, snapshot, online, profiles, onRefresh, onForget, runRequest }: { profile: Profile; snapshot: Snapshot | null; online: boolean; profiles: Profile[]; onRefresh: () => void; onForget: () => void; runRequest: <T,>(channel: Channel, method: string, params: Record<string, unknown>, options?: { operationId?: string; expectedEpoch?: string; expectedRevision?: string; targetNodeId?: string }) => Promise<T> }) {
  const [syncing, setSyncing] = useState(false)
  const [conflicts, setConflicts] = useState<Row[]>([])
  const [history, setHistory] = useState<Row[]>([])
  const [actionError, setActionError] = useState('')
  const [detailsNodeId, setDetailsNodeId] = useState('')
  const [confirmRemoveNodeId, setConfirmRemoveNodeId] = useState('')
  const [profileCandidates, setProfileCandidates] = useState<Record<string, Row>>({})
  const [profileReviews, setProfileReviews] = useState<Record<string, { candidate: Row; reviewHash: string; revision: string }>>({})
  const [resolutionChoices, setResolutionChoices] = useState<Record<string, 'base' | 'current' | 'incoming'>>({})
  const profileEditVersions = useRef(new Map<string, number>())
  const conflictRefreshGeneration = useRef(0)
  const conflictsRef = useRef(conflicts)
  conflictsRef.current = conflicts
  const nodes = asRows(snapshot?.nodes)
  const sync = isRow(snapshot?.sync) ? snapshot.sync : {}
  const syncConfigured = sync.configured === true
  const storedSyncStatus = text(sync, 'status')
  const displayedSyncStatus = online ? storedSyncStatus || 'Up to date' : storedSyncStatus ? `Last reported · ${storedSyncStatus}` : 'Cached snapshot'
  const localNode = nodes.find(node => nodeId(node) === text(snapshot ?? {}, 'nodeId', profile.nodeId))
  const canRemoveWorkers = online && localNode?.primary === true
  const fetchConflicts = useCallback(async () => {
    if (!online || !syncConfigured) return
    const generation = ++conflictRefreshGeneration.current
    try {
      const value = await runRequest<unknown>('sync', 'conflicts', {})
      if (generation !== conflictRefreshGeneration.current) return
      const loaded = asRows(isRow(value) && 'data' in value ? value.data : value)
      conflictsRef.current = loaded
      for (const conflict of loaded) {
        const id = text(conflict, 'id')
        profileEditVersions.current.set(id, (profileEditVersions.current.get(id) ?? 0) + 1)
      }
      setConflicts(loaded)
      setProfileReviews({})
      setResolutionChoices(Object.fromEntries(loaded.map(conflict => [text(conflict, 'id'), conflict.current ? 'current' : conflict.incoming ? 'incoming' : 'base'])))
      setProfileCandidates(Object.fromEntries(loaded.filter(conflict => conflict.profileReview === true).map(conflict => {
        const candidate = profileCandidateFromRecord(conflict.current)
        return [text(conflict, 'id'), Object.keys(candidate).some(key => text(candidate, key)) ? candidate : profileCandidateFromRecord(conflict.incoming)]
      })))
    } catch (error) { setActionError(error instanceof Error ? error.message : 'Could not refresh the conflict list.') }
  }, [online, runRequest, syncConfigured])
  useEffect(() => {
    if (!online || !syncConfigured) { setConflicts([]); conflictsRef.current = []; setHistory([]); setProfileReviews({}); return }
    void fetchConflicts()
    void runRequest<unknown>('sync', 'history', {}).then(value => setHistory(asRows(isRow(value) && 'data' in value ? value.data : value))).catch(() => setHistory([]))
  }, [fetchConflicts, online, runRequest, syncConfigured, snapshot?.epoch])
  const synchronize = async () => { setSyncing(true); setActionError(''); try { await runRequest('sync', 'synchronize', {}); onRefresh() } catch (error) { setActionError(error instanceof Error ? error.message : 'Sync outcome is unknown. Refresh before retrying.') } finally { setSyncing(false) } }
  const removeWorker = async (node: Row) => {
    const id = nodeId(node)
    if (!canRemoveWorkers || !id || node.primary === true) return
    try {
      await runRequest('fleet', 'node.remove', { nodeId: id }, { operationId: createOperationId() })
      setConfirmRemoveNodeId('')
      setDetailsNodeId('')
      onRefresh()
    } catch (error) { setActionError(error instanceof Error ? error.message : 'Could not remove this worker node.') }
  }
  const resolve = async (conflict: Row) => {
    const choice = resolutionChoices[text(conflict, 'id')]
    if (!choice || !conflict[choice]) { setActionError('Choose a conflict version that still exists.'); return }
    setActionError('')
    try { await runRequest('sync', 'resolve', { conflictId: text(conflict, 'id'), choice }, { operationId: createOperationId(), ...revisionOptions(conflict) }); onRefresh() } catch (error) { setActionError(error instanceof Error ? error.message : 'Resolution outcome is unknown. Refresh before retrying.') }
  }
  const setProfileField = (conflictId: string, field: string, value: string) => {
    profileEditVersions.current.set(conflictId, (profileEditVersions.current.get(conflictId) ?? 0) + 1)
    setProfileCandidates(current => ({ ...current, [conflictId]: { ...(current[conflictId] ?? {}), [field]: value } }))
    setProfileReviews(current => { const next = { ...current }; delete next[conflictId]; return next })
  }
  const previewProfile = async (conflict: Row) => {
    const id = text(conflict, 'id')
    const candidate = profileCandidates[id]
    if (!candidate || !text(candidate, 'name').trim()) { setActionError('Enter a profile name before review.'); return }
    const version = profileEditVersions.current.get(id) ?? 0
    const revision = text(conflict, 'revision')
    try {
      setActionError('')
      const reviewed = await runRequest<Row>('sync', 'previewProfile', { conflictId: id, candidate: { ...candidate } }, { expectedRevision: revision })
      if (profileEditVersions.current.get(id) !== version || text(conflictsRef.current.find(item => text(item, 'id') === id) ?? {}, 'revision') !== revision) return
      const reviewHash = text(reviewed, 'reviewHash')
      if (!reviewHash) throw new Error('The server did not return a profile review hash.')
      setProfileReviews(current => ({ ...current, [id]: { candidate: isRow(reviewed.candidate) ? reviewed.candidate : candidate, reviewHash, revision } }))
    } catch (error) { setActionError(error instanceof Error ? error.message : 'Could not review this profile merge.') }
  }
  const approveProfile = async (conflict: Row) => {
    const id = text(conflict, 'id')
    const reviewed = profileReviews[id]
    if (!reviewed || reviewed.revision !== text(conflict, 'revision')) { setProfileReviews(current => { const next = { ...current }; delete next[id]; return next }); setActionError('Review this exact profile candidate again before applying it.'); return }
    try {
      setActionError('')
      await runRequest('sync', 'approveProfile', { conflictId: id, candidate: reviewed.candidate, reviewHash: reviewed.reviewHash }, { operationId: createOperationId(), expectedRevision: reviewed.revision })
      setProfileReviews(current => { const next = { ...current }; delete next[id]; return next })
      onRefresh()
    } catch (error) { setActionError(error instanceof Error ? error.message : 'Could not apply the reviewed profile merge.') }
  }
  return <section className="page fleet-page" aria-label="Fleet">
    <div className="page-heading"><div><p className="eyebrow">Private mesh · no cloud relay</p><h1>Fleet</h1></div><button className="secondary-button" disabled={!online || !syncConfigured || syncing} title={!syncConfigured ? 'Set up Git sync on the primary node first.' : undefined} onClick={() => void synchronize()}>{syncing ? <RefreshCw className="spin" size={15} /> : <FolderSync size={16} />}{syncing ? 'Syncing…' : 'Sync now'}</button></div>
    {!syncConfigured && <p className="helper-copy" role="status">Git sync has not been set up on the primary node.</p>}
    {actionError && <p className="inline-error" role="alert">{actionError}</p>}
    <div className="fleet-summary"><div className="fleet-ring"><span>{nodes.filter(node => node.online !== false).length}</span><small>{online ? 'online' : 'cached'}</small></div><div><h2>Your nodes</h2><p>{online ? 'Execution and credentials stay on each node. Sessions never move between machines.' : 'Primary disconnected · node details are from the last saved snapshot.'}</p></div></div>
    <div className="node-list">{nodes.length ? nodes.map(node => { const id = nodeId(node); const isPrimary = node.primary === true; const nodeOnline = node.online !== false; const runtimeReady = node.runtimeConnected === true || node.runtimeConnected === 'true'; return <article className="node-card" key={id}><span className={`node-state${!online ? ' is-cached' : nodeOnline ? ' is-online' : ''}`}><span /></span><div className="node-copy"><div className="node-title"><strong>{labelFor(node, 'name', 'nodeId')}</strong>{isPrimary && <span className="primary-tag">Primary</span>}</div><p>{text(node, 'platform', 'Brr service')} · {!online ? `Cached · last reported ${nodeOnline ? 'connected' : 'offline'}` : nodeOnline ? 'Connected' : 'Offline'}</p><small>{!online ? `Last saved runtime status · ${runtimeReady ? 'Ready' : 'Not connected'}` : runtimeReady ? 'Runtime ready' : 'Service reachable'}</small><small className="diagnostic-id" title={`Node ID ${id || 'Unavailable'}`}>Node ID · {id || 'Unavailable'}</small><small className="diagnostic-id" title={`Epoch ${text(node, 'epoch', 'Unavailable')}`}>Epoch · {text(node, 'epoch', 'Unavailable')}</small>{detailsNodeId === id && <div className="node-details"><dl><div><dt>Role</dt><dd>{isPrimary ? 'Primary' : 'Worker'}</dd></div><div><dt>Endpoint</dt><dd>{text(node, 'endpoint', 'Not shared')}</dd></div><div><dt>Runtime</dt><dd>{!online ? `Last saved · ${runtimeReady ? 'Connected' : 'Not connected'}` : runtimeReady ? 'Connected' : 'Not connected'}</dd></div><div><dt>Authentication</dt><dd>{text(node, 'accountType', text(node, 'authType', 'Local account'))}</dd></div></dl>{!isPrimary && canRemoveWorkers && (confirmRemoveNodeId === id ? <div className="inline-actions"><span>Remove this worker from the primary?</span><button className="secondary-button danger-text" onClick={() => void removeWorker(node)}>Confirm removal</button><button className="text-button" onClick={() => setConfirmRemoveNodeId('')}>Cancel</button></div> : <button className="text-button danger-text" onClick={() => setConfirmRemoveNodeId(id)}>Remove worker</button>)}</div>}</div><button className="icon-button" aria-label={`Details for ${labelFor(node, 'name', 'nodeId')}`} aria-expanded={detailsNodeId === id} onClick={() => { setDetailsNodeId(current => current === id ? '' : id); setConfirmRemoveNodeId('') }}><MoreHorizontal size={18} /></button></article> }) : <EmptyState icon={<Users />} title="No additional nodes" detail="Nodes are enrolled by their owner from the local service. Your paired primary appears here when online." />}</div>
    <div className="mobile-card sync-card"><div className="section-title"><div><p className="eyebrow">Shared records</p><h2>Git sync</h2></div><GitMerge size={19} /></div><dl className="sync-details"><div><dt>Status</dt><dd>{displayedSyncStatus}</dd></div><div><dt>Pending local changes</dt><dd>{number(sync, 'pendingChanges')} records</dd></div><div><dt>Last successful sync</dt><dd>{formatDate(sync.lastSyncedAt) || 'Not available'}</dd></div><div><dt>Conflicts</dt><dd>{conflicts.length}</dd></div></dl></div>
    {conflicts.length > 0 && <div className="subsection"><div className="section-title"><h2>Conflicts to review</h2><span className="count-chip is-warning">{conflicts.length}</span><button className="text-button" disabled={!online} onClick={() => void fetchConflicts()}>Refresh conflicts</button></div><p className="helper-copy">Review both committed versions. The primary stays on its last valid revision until each conflict is resolved.</p>{conflicts.map(conflict => {
      const id = text(conflict, 'id')
      const table = text(conflict, 'table')
      const profileReview = conflict.profileReview === true
      const candidate = profileCandidates[id] ?? {}
      const reviewed = profileReviews[id]
      return <article className="conflict-card" key={id}>
        <p className="eyebrow">{profileReview ? 'Profile head' : table ? conflictTableLabel(table) : text(conflict, 'kind', 'Record')} · {id}{table.includes('ProfileRevision') ? ' · Profile revision record' : ''}</p>
        <div className="conflict-columns"><Variant title="Base" value={conflict.base} /><Variant title="Current · exact record" value={conflict.current} /><Variant title="Incoming · exact record" value={conflict.incoming} /></div>
        {profileReview ? <div className="profile-conflict-editor"><h3>Build a reviewed profile merge</h3>
          {(['name', 'description', 'instructions', 'soul', 'model', 'effort'] as const).map(field => <label className="field-label" key={field}>{field === 'soul' ? 'Soul' : field[0].toUpperCase() + field.slice(1)}{field === 'description' || field === 'instructions' || field === 'soul' ? <textarea aria-label={`Merged profile ${field} for ${id}`} rows={field === 'soul' || field === 'instructions' ? 5 : 3} value={text(candidate, field)} onChange={event => setProfileField(id, field, event.target.value)} /> : <input aria-label={`Merged profile ${field} for ${id}`} value={text(candidate, field)} onChange={event => setProfileField(id, field, event.target.value)} />}</label>)}
          {!reviewed ? <button className="secondary-button" disabled={!online || !text(candidate, 'name').trim()} onClick={() => void previewProfile(conflict)}><ShieldCheck size={15} /> Review exact profile</button> : <div className="reviewed-profile"><p>Review hash · <code>{reviewed.reviewHash}</code></p><details><summary>Inspect the exact reviewed candidate</summary><pre>{JSON.stringify(reviewed.candidate, null, 2)}</pre></details><button className="primary-button" disabled={!online || reviewed.revision !== text(conflict, 'revision')} onClick={() => void approveProfile(conflict)}><Check size={15} /> Apply reviewed profile</button></div>}
        </div> : <div className="resolution-choice"><p>Choose the exact version to keep:</p><div role="radiogroup" aria-label={`Resolution version for ${id}`}>{(['base', 'current', 'incoming'] as const).map(choice => <label key={choice}><input type="radio" name={`resolution-${id}`} value={choice} checked={resolutionChoices[id] === choice} disabled={!conflict[choice]} onChange={() => setResolutionChoices(current => ({ ...current, [id]: choice }))} />Keep {choice}</label>)}</div><button className="primary-button" disabled={!online || !resolutionChoices[id] || !conflict[resolutionChoices[id]]} onClick={() => void resolve(conflict)}><GitMerge size={15} /> Commit chosen version</button></div>}
      </article>
    })}</div>}
    {history.length > 0 && <div className="subsection"><div className="section-title"><h2>Recent sync history</h2><span className="count-chip">{history.length}</span></div><div className="list-card">{history.slice(0, 8).map(item => <div className="history-row" key={text(item, 'id')}><GitMerge size={15} /><div><strong>{labelFor(item, 'summary', 'message')}</strong><small>{formatDate(item.createdAt)} · {text(item, 'nodeId', profile.nodeId)}</small></div></div>)}</div></div>}
    <div className="danger-zone"><div><strong>Paired on this phone</strong><p>Remove the saved connection and device secret from this app.</p></div><button className="text-button danger-text" disabled={profiles.length === 0} onClick={onForget}><Trash2 size={15} /> Forget node</button></div>
  </section>
}

function Variant({ title, value }: { title: string; value: unknown }) {
  return <div className="variant"><small>{title}</small><pre>{value === null || value === undefined ? 'Not present' : JSON.stringify(value, null, 2)}</pre></div>
}
