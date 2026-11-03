import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from 'react'
import { ArrowLeft, ArrowRight, ArrowUp, ChevronsUpDown, CircleUserRound, Code2, FolderOpen, House, LayoutGrid, LoaderCircle, LockKeyhole, MoreHorizontal, PanelLeftClose, PanelLeftOpen, Search, Settings, Square, SquarePen, SquarePlus, TicketCheck, X } from 'lucide-react'
import { AgentMessage } from './components/AgentMessage'
import { ChatEditor, type ChatEditorHandle } from './components/ChatEditor'
import { ChatTranscript } from './components/ChatTranscript'
import { ChatContextBar, ChatQueue } from './components/ChatContext'
import { SavedMessages } from './components/SavedMessages'
import { IntelligentUi } from './components/IntelligentUi'
import type { UiIdentity, UiActionReview } from './lib/intelligent-ui'
import { claimUiAction } from './lib/intelligent-ui-actions'
import { ExtensionsPanel } from './components/ExtensionsPanel'
import { McpRequests } from './components/McpRequests'
import { QuestionForm, QuestionTranscript } from './components/QuestionForm'
import { Button } from './components/ui/button'
import { RailButton } from './components/RailButton'
import { client, isDesktop, WorktreeSetupRequired, type Connection, type Model, type Thread, type ProjectRecord, type ProjectInput, type ProjectArchiveResult, type WorktreeSetupCommand } from './lib/client'
import { agents, type AgentProfile, type QuestionRequest } from './lib/agents'
import type { QuestionAnswers } from './lib/questions'
import { assignmentFromDraft, draftWithText, emptyDraft } from './lib/chat-draft'
import { useChatDrafts } from './lib/use-chat-drafts'
import { chat, type ChatAttachment, type ChatSettings, type QueueSnapshot, type SavedDraft } from './lib/chat'
import { itemText, projectAcceptedSubmissionMessages } from './lib/transcript'
import type { AgentItem } from './lib/session'
import { explicitAgentMention } from './lib/agent-composer'
import { AgentsPanel } from './components/AgentsPanel'
import { TicketsPanel } from './components/TicketsPanel'
import { AppearanceSettings } from './components/AppearanceSettings'
import { ComposerOptions } from './components/ComposerOptions'
import { WorkspacePanel } from './components/workspace/WorkspacePanel'
import { ProjectMenu } from './components/ProjectMenu'
import { ProjectDialog } from './components/ProjectDialog'
import { ProjectActionReview } from './components/ProjectActionReview'
import { SIDEBAR_COLORS, loadSidebarColor, saveSidebarColor, type SidebarColorId } from './lib/appearance'
import { loadSidebarCollapsed, saveSidebarCollapsed } from './lib/sidebar-layout'
import { branchNoticeKey, loadBranchNoticeDismissals, saveBranchNoticeDismissals, type PendingBranchNotice } from './lib/branch-notices'
import { useThemeAccent } from './lib/theme-accent'
import './App.css'
import { hydrateThread, initialSession, reduceSession, type Approval, type RpcEvent, type SessionState } from './lib/session'

type SetupReview = { projectId: string | null; commands: WorktreeSetupCommand[] }
const worktreeSetupReview = (reason: unknown): SetupReview | null => {
  if (!reason || typeof reason !== 'object') return null
  const value = reason as { kind?: unknown; projectId?: unknown; commands?: unknown }
  if (value.kind !== 'worktreeSetupRequired' || typeof value.projectId !== 'string' || !Array.isArray(value.commands)) return null
  const commands = value.commands.filter((command): command is WorktreeSetupCommand => command && typeof command === 'object'
    && typeof command.phase === 'string' && typeof command.template === 'string' && command.approved === false)
  return commands.length ? { projectId: value.projectId || null, commands } : null
}

const upsertThread = (threads: Thread[], next: Thread) => {
  const existing = threads.find(thread => thread.id === next.id)
  const merged = { ...existing, ...next }
  if (existing?.name && !next.name) merged.name = existing.name
  if (existing?.preview && !next.preview) merged.preview = existing.preview
  return [merged, ...threads.filter(thread => thread.id !== next.id)]
}
const streamingItemDeltaMethods = new Set([
  'item/agentMessage/delta', 'item/reasoning/summaryTextDelta',
  'item/commandExecution/outputDelta', 'item/plan/delta',
  'brr/ui/argumentsDelta',
])
const terminalItemStatuses = new Set(['completed', 'failed', 'interrupted', 'declined'])
const boundAgent = (thread: Thread) => {
  const binding = (thread as Thread & { brr?: { agentId?: unknown } }).brr
  return typeof binding?.agentId === 'string' ? binding.agentId : ''
}
const workspacePathIsWithin = (path: string, root: string) => {
  const normalizedPath = path.replace(/\\/g, '/')
  const normalizedRoot = root.replace(/\\/g, '/').replace(/\/+$/, '')
  return normalizedPath === normalizedRoot || normalizedPath.startsWith(`${normalizedRoot}/`)
}
const pathBasename = (path: string) => path.replace(/[\\/]+$/, '').split(/[\\/]/).filter(Boolean).at(-1) ?? path
const mergeStreamedText = (snapshot: string, live: string) => {
  if (snapshot.includes(live)) return snapshot
  if (live.startsWith(snapshot)) return live
  for (let size = Math.min(snapshot.length, live.length); size > 0; size -= 1) {
    if (snapshot.endsWith(live.slice(0, size))) return snapshot + live.slice(size)
  }
  return live
}
const mergeHydratedItems = (hydrated: SessionState['items'], live: SessionState['items']) => {
  const liveById = new Map(live.map(item => [item.id, item]))
  const merged = hydrated.map(item => {
    const current = liveById.get(item.id)
    if (!current) return item
    liveById.delete(item.id)
    const result = { ...item, ...current }
    if (typeof item.status === 'string' && terminalItemStatuses.has(item.status)) result.status = item.status
    for (const field of ['text', 'aggregatedOutput'] as const) {
      const fromSnapshot = item[field]
      const fromLive = current[field]
      if (typeof fromSnapshot === 'string' && typeof fromLive === 'string') {
        result[field] = mergeStreamedText(fromSnapshot, fromLive)
      }
    }
    return result
  })
  return [...merged, ...live.filter(item => liveById.has(item.id))]
}

export default function App() {
  const desktop = isDesktop()
  const chatDrafts = useChatDrafts(desktop)
  const [account, setAccount] = useState<Connection['account']>(null)
  const [models, setModels] = useState<Model[]>([])
  const [threads, setThreads] = useState<Thread[]>([])
  const [projects, setProjects] = useState<ProjectRecord[]>([])
  const projectWrites = useRef(new Map<string, ProjectRecord>())
  const [projectsReady, setProjectsReady] = useState(false)
  const [projectLoadError, setProjectLoadError] = useState('')
  const [projectSettings, setProjectSettings] = useState<ProjectRecord | null>(null)
  const [projectCreating, setProjectCreating] = useState(false)
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null)
  const selectedProjectIdRef = useRef(selectedProjectId)
  selectedProjectIdRef.current = selectedProjectId
  const [projectAction, setProjectAction] = useState<{project: ProjectRecord; action: 'archive' | 'delete'} | null>(null)
  const [projectActionResult, setProjectActionResult] = useState<ProjectArchiveResult | null>(null)
  const [projectActionError, setProjectActionError] = useState('')
  const [projectPending, setProjectPending] = useState(false)
  const projectBusy = useRef(false)
  const [sessions, setSessions] = useState<Record<string, SessionState>>({})
  const [resuming, setResuming] = useState<Record<string, boolean>>({})
  const hydratedThreads = useRef(new Set<string>())
  const resumingThreads = useRef(new Set<string>())
  const resumeEvents = useRef<Record<string, RpcEvent[]>>({})
  const [threadId, setThreadId] = useState<string | null>(null)
  const [folder, setFolder] = useState('')
  const folderRef = useRef(folder)
  folderRef.current = folder
  const [modelId, setModelId] = useState('')
  const [effort, setEffort] = useState('medium')
  const [readOnly, setReadOnly] = useState(false)
  const appDefaults = useRef({modelId: '', effort: 'medium', readOnly: false})
  const [workspacePromotion, setWorkspacePromotion] = useState<{draftKey: number; threadId: string} | null>(null)
  const [workspaceOpen, setWorkspaceOpen] = useState(false)
  const [workspaceExpanded, setWorkspaceExpanded] = useState(false)
  const [workspaceRequest, setWorkspaceRequest] = useState(0)
  const [fileOpen, setFileOpen] = useState<{id:string;root:string;path:string;line?:number;threadId:string|null} | undefined>()
  const [expandedUi, setExpandedUi] = useState<{identity: UiIdentity; title: string} | null>(null)
  const [uiReview, setUiReview] = useState<UiActionReview | null>(null)
  const [uiSending, setUiSending] = useState(false)
  const [uiError, setUiError] = useState('')
  const uiSendLock = useRef(false)
  const foregroundSendLock = useRef(false)
  const [workspaceWidth, setWorkspaceWidth] = useState(460)
  const [workspaceNarrow, setWorkspaceNarrow] = useState(() => window.innerWidth <= 1100)
  useEffect(() => {
    const resize = () => setWorkspaceNarrow(window.innerWidth <= 1100)
    window.addEventListener('resize', resize)
    return () => window.removeEventListener('resize', resize)
  }, [])
  const [extensionsOpen, setExtensionsOpen] = useState(false)
  const [ticketsOpen, setTicketsOpen] = useState(false)
  const [ticketProjectId, setTicketProjectId] = useState('')
  const [agentsOpen, setAgentsOpen] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const settingsTrigger = useRef<HTMLButtonElement>(null)
  const profileTrigger = useRef<HTMLButtonElement>(null)
  const profilePanel = useRef<HTMLElement>(null)
  const [profileOpen, setProfileOpen] = useState(false)
  const [recoveryOpen, setRecoveryOpen] = useState(false)
  const [sidebarCollapsed, setSidebarCollapsed] = useState(loadSidebarCollapsed)
  const sidebarToggle = useRef<HTMLButtonElement>(null)
  const searchInput = useRef<HTMLInputElement>(null)
  const [sidebarColor, setSidebarColor] = useState(loadSidebarColor)
  const [appearanceSaveFailed, setAppearanceSaveFailed] = useState(false)
  const [agentProfiles, setAgentProfiles] = useState<AgentProfile[]>([])
  const [composerAgentId, setComposerAgentId] = useState('')
  const [threadAgentId, setThreadAgentId] = useState('')
  const [threadAgentIds, setThreadAgentIds] = useState<Record<string, string>>({})
  const [agentAlertCount, setAgentAlertCount] = useState(0)
  const [composerNotice, setComposerNotice] = useState('')
  const [searching, setSearching] = useState(false)
  const [query, setQuery] = useState('')
  const [expandedProjects, setExpandedProjects] = useState<Set<string>>(() => new Set())
  const [modeChoices, setModeChoices] = useState<Record<string, 'default' | 'plan'>>({})
  const [pending, setPending] = useState(false)
  const [steeringPending, setSteeringPending] = useState(false)
  const [attachments, setAttachments] = useState<ChatAttachment[]>([])
  const attachmentsRef = useRef(attachments)
  attachmentsRef.current = attachments
  const activeDraftKey = useRef(chatDrafts.key)
  activeDraftKey.current = chatDrafts.key
  const activeDraftDocument = useRef(chatDrafts.document)
  activeDraftDocument.current = chatDrafts.document
  const [queueSnapshot, setQueueSnapshot] = useState<QueueSnapshot>({entries: [], state: {paused: false}})
  const [queueSnapshotOwner, setQueueSnapshotOwner] = useState<string | null>(null)
  const [pendingBranches, setPendingBranches] = useState<PendingBranchNotice[]>([])
  const [recoveringBranchId, setRecoveringBranchId] = useState('')
  const [branchRecoveryErrors, setBranchRecoveryErrors] = useState<Record<string,string>>({})
  const [dismissedBranchNoticeKeys, setDismissedBranchNoticeKeys] = useState<Set<string>>(() => loadBranchNoticeDismissals())
  const [referenceCandidates, setReferenceCandidates] = useState<Array<{id:string;name:string;path:string;type:'file'|'folder'}>>([])
  const [referenceQuery, setReferenceQuery] = useState('')
  const [enabledSkills, setEnabledSkills] = useState<Array<{name:string;path:string;description?:string}>>([])
  const [loginPending, setLoginPending] = useState(false)
  const [loginId, setLoginId] = useState<string | null>(null)
  const [cancelingLogin, setCancelingLogin] = useState(false)
  const loginPendingRef = useRef(false)
  const currentLoginIdRef = useRef<string | null>(null)
  const loginAttemptRef = useRef(0)
  const cancelledLoginIdsRef = useRef(new Set<string>())
  const conversationScroll = useRef<HTMLDivElement>(null)
  const followTranscript = useRef(true)
  const [engineOnline, setEngineOnline] = useState(true)
  const [connectionReady, setConnectionReady] = useState(false)
  const [error, setError] = useState('')
  const [setupReview, setSetupReview] = useState<SetupReview | null>(null)
  const [setupApprovalPending, setSetupApprovalPending] = useState(false)
  const setupReviewGeneration = useRef(0)
  const [questionRequests, setQuestionRequests] = useState<QuestionRequest[]>([])
  const [questionError, setQuestionError] = useState('')
  const [questionNavigationId, setQuestionNavigationId] = useState('')
  const [selectedQuestionId, setSelectedQuestionId] = useState('')
  const questionFetchGeneration = useRef(0)
  const queueFetchGeneration = useRef(0)
  const activeQuestionThread = useRef(threadId)
  activeQuestionThread.current = threadId
  const viewVersion = useRef(0)
  const draftVersion = useRef(0)
  const movingChats = useRef(new Set<string>())
  const [movingChatIds, setMovingChatIds] = useState<string[]>([])

  const text = chatDrafts.document.text
  const setDraftAttachments = (ownerId: string, next: ChatAttachment[]) => {
    if (activeDraftKey.current !== ownerId) return
    setAttachments(next)
    draftVersion.current += 1
    const ids = [...new Set(next.map(item => item.id))]
    chatDrafts.setDocument(previous => ({ ...previous, attachmentIds: ids }))
  }
  const clearSubmittedDraft = () => {
    draftVersion.current += 1
    chatDrafts.setDocument(previous => draftWithText({ ...previous, attachmentIds: [] }, ''))
    setAttachments([])
  }
  const setText = (next: string | ((previous: string) => string)) => {
    const document = chatDrafts.document
    const value = typeof next === 'function' ? next(document.text) : next
    draftVersion.current += 1
    chatDrafts.setDocument(previous => draftWithText(previous, value))
  }
  const saveDraftSettings = (settings: Partial<ChatSettings>) => {
    chatDrafts.updateContext({ settings: { ...chatDrafts.context.current.settings, ...settings } })
  }
  const editorRef = useRef<ChatEditorHandle>(null)
  const sendMoreMenuRef = useRef<HTMLDetailsElement>(null)

  const activeModel = models.find(model => model.id === modelId) ?? models[0]
  const pendingBranchNotices = pendingBranches.map(operation => ({
    operation,
    key: branchNoticeKey(operation),
  }))
  const visibleBranchNotices = pendingBranchNotices.filter(notice => !dismissedBranchNoticeKeys.has(notice.key))
  const dismissedBranchNoticeCount = pendingBranchNotices.length - visibleBranchNotices.length
  const sidebarAppearance = SIDEBAR_COLORS.find(color => color.id === sidebarColor) ?? SIDEBAR_COLORS[0]
  useThemeAccent(sidebarAppearance)
  const chooseSidebarColor = (id: SidebarColorId) => {
    setSidebarColor(id)
    setAppearanceSaveFailed(!saveSidebarColor(id))
  }
  const toggleSidebar = () => {
    if (workspaceOpen && (workspaceNarrow || workspaceExpanded) && !profileOpen && !agentsOpen && !extensionsOpen && !settingsOpen && !projectSettings && !projectAction) { setWorkspaceOpen(false); setWorkspaceExpanded(false); return }
    const collapsed = !sidebarCollapsed
    setSidebarCollapsed(collapsed)
    saveSidebarCollapsed(collapsed)
  }
  const openChatSearch = () => {
    setRecoveryOpen(false)
    setSearching(true)
    searchInput.current?.focus()
    if (sidebarCollapsed) {
      setSidebarCollapsed(false)
      saveSidebarCollapsed(false)
    }
  }
  const showSavedMessages = () => {
    setProjectSettings(null); setProjectCreating(false); setProjectAction(null)
    setSettingsOpen(false); setAgentsOpen(false); setExtensionsOpen(false); setTicketsOpen(false); setProfileOpen(false)
    setRecoveryOpen(true)
  }
  const dismissBranchNotices = () => {
    if (recoveringBranchId) return
    const next = new Set(dismissedBranchNoticeKeys)
    for (const notice of visibleBranchNotices) next.add(notice.key)
    saveBranchNoticeDismissals(next)
    setDismissedBranchNoticeKeys(next)
    requestAnimationFrame(() => editorRef.current?.focus())
  }
  const reviewDismissedBranchNotices = () => {
    const currentKeys = new Set(pendingBranchNotices.map(notice => notice.key))
    const next = new Set([...dismissedBranchNoticeKeys].filter(key => !currentKeys.has(key)))
    saveBranchNoticeDismissals(next)
    setDismissedBranchNoticeKeys(next)
    setRecoveryOpen(false)
  }
  const closeProfile = () => { setProfileOpen(false); profileTrigger.current?.focus(); requestAnimationFrame(() => profileTrigger.current?.focus()) }
  const selectedAgent = agentProfiles.find(agent => agent.id === composerAgentId)
  const activeChatAgent = agentProfiles.find(agent => agent.id === (threadId ? threadAgentId : composerAgentId))
  const session = threadId ? sessions[threadId] ?? initialSession(threadId) : initialSession()
  const activeQuestions = threadId ? questionRequests.filter(question => question.threadId === threadId) : []
  const pendingQuestions = activeQuestions.filter(question => question.resolution === 'pending').sort((left, right) => left.createdAt - right.createdAt)
  const selectedQuestion = pendingQuestions.find(question => question.id === selectedQuestionId)
    ?? pendingQuestions.find(question => question.id === questionNavigationId)
    ?? pendingQuestions[0]
  const worktree = threads.find(thread => thread.id === threadId)?.worktree
  const activeWorkspace = threadId ? threads.find(thread => thread.id === threadId)?.workspace : undefined
  const sharedFolderPaths = activeWorkspace?.folders
    .map(folder => folder.path)
    .filter(path => Boolean(path) && !activeWorkspace.worktrees.some(worktree => workspacePathIsWithin(path, worktree.path))) ?? []
  const modeKey = threadId ?? 'new'
  const mode = modeChoices[modeKey] ?? session.mode
  chatDrafts.context.current = {
    threadId,
    projectId: selectedProjectId ?? '',
    agentId: threadId ? threadAgentId : composerAgentId,
    settings: { model: activeModel?.model ?? '', modelId: activeModel?.id, effort, mode, readOnly },
  }
  useEffect(() => {
    if (!desktop) { setAttachments([]); return }
    const ownerId = chatDrafts.key
    let active = true
    setAttachments([])
    void chat.attachments(ownerId).then(result => {
      if (!active || activeDraftKey.current !== ownerId) return
      setAttachments(current => {
        const known = new Map([...result, ...current].map(item => [item.id, item]))
        return [...new Set(activeDraftDocument.current.attachmentIds ?? [])].flatMap(id => known.has(id) ? [known.get(id)!] : [])
      })
    }).catch(() => { if (active && activeDraftKey.current === ownerId) setAttachments([]) })
    return () => { active = false }
  }, [desktop, chatDrafts.key])
  useEffect(() => {
    if (!desktop) { setEnabledSkills([]); return }
    let active = true
    void chat.skills(worktree?.projectPath ?? folder).then(result => { if (active) setEnabledSkills(result) }).catch(() => { if (active) setEnabledSkills([]) })
    return () => { active = false }
  }, [desktop, folder, worktree?.projectPath, threadId])
  const refreshPendingBranches = async () => {
    if (!desktop || typeof chat.pendingBranches !== 'function') return
    try { setPendingBranches((await chat.pendingBranches()).data ?? []) } catch { /* Keep the last known recoverable operations visible. */ }
  }
  useEffect(() => { if (desktop && connectionReady && engineOnline) void refreshPendingBranches() }, [desktop, connectionReady, engineOnline])
  useEffect(() => {
    if (!referenceQuery || !desktop) { setReferenceCandidates([]); return }
    let active = true
    const scope = threadId ? {threadId} as const : selectedProjectId ? {projectId: selectedProjectId} as const : null
    if (!scope) { setReferenceCandidates([]); return }
    void chat.references({...scope, query: referenceQuery})
      .then(result => { if (active) setReferenceCandidates(result) }).catch(() => { if (active) setReferenceCandidates([]) })
    return () => { active = false }
  }, [desktop, referenceQuery, threadId, selectedProjectId])
  useEffect(() => {
    if (!threadId || !desktop) { setQueueSnapshot({entries: [], state: {paused: false}}); setQueueSnapshotOwner(null); return }
    let active = true
    const generation = ++queueFetchGeneration.current
    const requestedView = viewVersion.current
    void chat.queue(threadId).then(snapshot => {
      if (active && generation === queueFetchGeneration.current && activeQuestionThread.current === threadId && viewVersion.current === requestedView) { setQueueSnapshot(snapshot); setQueueSnapshotOwner(threadId) }
    }).catch(() => {})
    return () => { active = false }
  }, [desktop, threadId, session.turnId])
  const refreshQueue = async (id = threadId) => {
    if (!id || !desktop) {
      if (!threadId) { setQueueSnapshot({entries: [], state: {paused: false}}); setQueueSnapshotOwner(null) }
      return
    }
    const generation = ++queueFetchGeneration.current
    const requestedView = viewVersion.current
    try {
      const snapshot = await chat.queue(id)
      if (generation === queueFetchGeneration.current && activeQuestionThread.current === id && viewVersion.current === requestedView) { setQueueSnapshot(snapshot); setQueueSnapshotOwner(id) }
    } catch { /* Keep the last confirmed queue snapshot visible. */ }
  }
  const activeTurnRef = useRef({ threadId, turnId: session.turnId })
  activeTurnRef.current = { threadId, turnId: session.turnId }
  const visibleQueueSnapshot = queueSnapshotOwner === threadId ? queueSnapshot : {entries: [], state: {paused: false}}
  const transcriptSession = projectAcceptedSubmissionMessages(session, threadId, visibleQueueSnapshot.entries)
  const visibleThreads = useMemo(() => threads.filter(thread => `${thread.name ?? ''} ${thread.preview}`.toLowerCase().includes(query.toLowerCase())), [threads, query])
  const sidebarDrafts = chatDrafts.records.filter(draft => !draft.threadId || (draft.branchEdit === true && !threads.some(thread => thread.id === draft.threadId)))
  const refreshQuestions = async (id: string | null) => {
    const generation = ++questionFetchGeneration.current
    if (!id) { setQuestionRequests([]); return }
    const result = await agents.questions.list({ threadId: id })
    if (generation === questionFetchGeneration.current && activeQuestionThread.current === id) setQuestionRequests(result.data)
  }
  const refreshQuestionsRef = useRef(refreshQuestions)
  refreshQuestionsRef.current = refreshQuestions
  const saveQuestionDraft = async (question: QuestionRequest, answers: QuestionAnswers) => {
    const updated = await agents.questions.saveDraft(question.id, answers)
    setQuestionRequests(current => current.map(item => item.id === updated.id ? updated : item))
  }
  const answerQuestion = async (question: QuestionRequest, answers: QuestionAnswers) => {
    const updated = await agents.questions.answer(question.id, answers)
    setQuestionRequests(current => current.map(item => item.id === updated.id ? updated : item))
    setQuestionNavigationId('')
    void refreshAgentStatusRef.current().catch(() => {})
  }
  const dismissQuestion = async (question: QuestionRequest) => {
    const updated = await agents.questions.dismiss(question.id)
    setQuestionRequests(current => current.map(item => item.id === updated.id ? updated : item))
    setQuestionNavigationId('')
    void refreshAgentStatusRef.current().catch(() => {})
  }

  const projectRecords = useMemo(() => projects.filter(project => !project.hidden), [projects])
  const selectedProject = projectRecords.find(project => project.id === selectedProjectId)
  const projectGroups = useMemo(() => projectRecords.map(project => ({project, chats: visibleThreads.filter(thread => thread.projectId === project.id)}))
    .filter(({project, chats}) => !query || chats.length || project.name.toLowerCase().includes(query.toLowerCase())), [projectRecords, visibleThreads, query])
  const rememberProject = (project: ProjectRecord) => {
    projectWrites.current.set(project.id, project)
    setProjects(previous => [...previous.filter(value => value.id !== project.id), project])
  }
  const acceptLoadedProjects = (loaded: ProjectRecord[]) => {
    const records = new Map(loaded.map(project => [project.id, project] as const))
    for (const [id, local] of projectWrites.current) {
      const remote = records.get(id)
      if (!remote || local.revision > remote.revision) records.set(id, local)
    }
    setProjects([...records.values()])
  }
  const applyProjectDefaults = (project?: ProjectRecord) => {
    const selected = models.find(model => model.model === project?.model)
      ?? models.find(model => model.id === appDefaults.current.modelId) ?? models.find(model => model.isDefault) ?? models[0]
    if (selected) {
      setModelId(selected.id)
      setEffort(project?.effort && selected.supportedReasoningEfforts.some(value => value.reasoningEffort === project.effort)
        ? project.effort : project?.model ? selected.defaultReasoningEffort : appDefaults.current.effort)
    }
    setReadOnly(project?.permission ? project.permission === 'read-only' : appDefaults.current.readOnly)
  }
  const selectProjectForDraft = (projectId: string | null) => {
    const project = projectRecords.find(value => value.id === projectId)
    const path = project?.primaryFolder ?? project?.folders[0] ?? ''
    const selected = models.find(value => value.model === project?.model)
      ?? models.find(value => value.id === appDefaults.current.modelId) ?? models.find(value => value.isDefault) ?? models[0]
    const effortValue = project?.effort && selected?.supportedReasoningEfforts.some(value => value.reasoningEffort === project.effort)
      ? project.effort : project?.model ? selected?.defaultReasoningEffort ?? appDefaults.current.effort : appDefaults.current.effort
    const readOnlyValue = project?.permission ? project.permission === 'read-only' : appDefaults.current.readOnly
    setSelectedProjectId(project?.id ?? null); setFolder(path)
    applyProjectDefaults(project)
    chatDrafts.updateContext({projectId: project?.id ?? '', settings: {model: selected?.model ?? '', modelId: selected?.id, effort: effortValue, mode, readOnly: readOnlyValue}})
  }
  const moveChatToProject = (movingId: string, projectId: string | null) => {
    if (movingChats.current.has(movingId)) return
    movingChats.current.add(movingId)
    setMovingChatIds([...movingChats.current])
    const requestedView = viewVersion.current
    void client.moveChat(movingId, projectId).then(() => {
      setThreads(current => current.map(thread => thread.id === movingId ? {...thread, projectId} : thread))
      if (viewVersion.current === requestedView) setSelectedProjectId(projectId)
    }).catch(reason => { if (viewVersion.current === requestedView) setError(`Could not move chat: ${String(reason)}`) }).finally(() => {
      movingChats.current.delete(movingId)
      setMovingChatIds([...movingChats.current])
    })
  }
  const focusProjectMenu = (id: string) => requestAnimationFrame(() => {
    const trigger = [...document.querySelectorAll<HTMLButtonElement>('.project-menu-trigger')].find(button => button.dataset.projectId === id)
    ;(trigger ?? document.querySelector<HTMLButtonElement>('.rail-home'))?.focus()
  })
  const closeProjectPage = () => {
    if (projectBusy.current) return
    const id = projectSettings?.id ?? projectAction?.project.id
    setProjectSettings(null); setProjectAction(null); setProjectCreating(false)
    if (id) focusProjectMenu(id)
  }
  const loadProjects = async () => {
    try { const result = await client.projects(); acceptLoadedProjects(result.data); setProjectsReady(true); setProjectLoadError('') }
    catch (reason) { setProjectLoadError(`Could not load projects: ${String(reason)}`) }
  }
  useEffect(() => {
    if (!desktop) return
    let active = true
    void client.projects().then(result => { if (active) { acceptLoadedProjects(result.data); setProjectsReady(true) } })
      .catch(reason => { if (active) setProjectLoadError(`Could not load projects: ${String(reason)}`) })
    return () => { active = false }
  }, [desktop])
  const refreshAgentStatus = async () => {
    if (!desktop || !connectionReady || !engineOnline) return
    const [profiles, decisions, tasks, drafts] = await Promise.all([agents.list(), agents.decisions(), agents.tasks(), agents.drafts()])
    setAgentProfiles(profiles.data)
    const questions = decisions.questions ?? []
    const representedWaitingTasks = new Set(questions.map(question => question.taskId).filter((id): id is string => Boolean(id)))
    setAgentAlertCount(decisions.data.length + (decisions.runtimeRequests?.length ?? 0) + questions.length + drafts.data.filter(draft => ['proposed', 'pending'].includes(draft.status)).length + tasks.data.filter(task => ['failed', 'blocked', 'waiting_approval', 'waiting_input'].includes(task.status) && !(task.status === 'waiting_input' && representedWaitingTasks.has(task.id))).length)
  }
  const refreshAgentStatusRef = useRef(refreshAgentStatus)
  refreshAgentStatusRef.current = refreshAgentStatus

  useEffect(() => {
    if (desktop && connectionReady && engineOnline) void refreshAgentStatus().catch(() => {})
  }, [desktop, connectionReady, engineOnline])

  useEffect(() => {
    if (desktop && connectionReady && engineOnline && threadId) void refreshQuestions(threadId).catch(() => {})
    else { questionFetchGeneration.current += 1; setQuestionRequests([]) }
  }, [desktop, connectionReady, engineOnline, threadId])

  useEffect(() => {
    if (!questionNavigationId || !threadId) return
    const frame = requestAnimationFrame(() => {
      const target = document.getElementById(`question-input-${questionNavigationId}`)
        ?? document.getElementById(`question-${questionNavigationId}`)
      if (!target) return
      target.scrollIntoView?.({ block: 'center' })
      target.focus()
      setQuestionNavigationId('')
    })
    return () => cancelAnimationFrame(frame)
  }, [questionNavigationId, questionRequests, threadId])

  useEffect(() => {
    if (!desktop) return
    let active = true
    let unlisten: (() => void) | undefined
    void client.subscribe(event => {
      if (event.method === 'brr/agents/changed' || event.method === 'brr/tasks/changed' || event.method === 'brr/questions/changed') {
        void refreshAgentStatusRef.current().catch(() => {})
      }
      if (event.method === 'brr/tasks/changed' && activeQuestionThread.current) {
        void refreshQuestionsRef.current(activeQuestionThread.current).catch(() => {})
      }
      if (event.method === 'brr/questions/changed' && (!event.params?.threadId || event.params.threadId === activeQuestionThread.current)) {
        void refreshQuestionsRef.current(activeQuestionThread.current).catch(() => {})
      }
      if (event.method === 'brr/chat/changed' && typeof event.params?.threadId === 'string' && event.params.threadId === activeQuestionThread.current) {
        const targetThreadId = event.params.threadId
        const targetView = viewVersion.current
        const generation = ++queueFetchGeneration.current
        void chat.queue(targetThreadId).then(snapshot => {
          if (active && generation === queueFetchGeneration.current && activeQuestionThread.current === targetThreadId && viewVersion.current === targetView) { setQueueSnapshot(snapshot); setQueueSnapshotOwner(targetThreadId) }
        }).catch(() => {})
      }
      if (event.method === 'brr/questions/error') {
        setQuestionError(typeof event.params?.message === 'string' ? event.params.message : 'The agent question could not be delivered.')
      }
    }).then(unsubscribe => { if (active) unlisten = unsubscribe; else unsubscribe() }).catch(() => {})
    return () => { active = false; unlisten?.() }
  }, [desktop])
  useEffect(() => {
    const element = conversationScroll.current
    if (element && followTranscript.current) element.scrollTop = element.scrollHeight
  }, [session.items])
  useEffect(() => {
    if (searching && !sidebarCollapsed) searchInput.current?.focus()
  }, [searching, sidebarCollapsed])
  useEffect(() => {
    if (!profileOpen) return
    profilePanel.current?.focus()
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') closeProfile()
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [profileOpen])
  const applyConnection = (connection: Connection) => {
    setAccount(connection.account)
    setModels(connection.models)
    setThreads(connection.threads)
    setThreadAgentIds(Object.fromEntries(connection.threads.map(thread => [thread.id, boundAgent(thread)]).filter(([, agentId]) => Boolean(agentId)) as [string, string][]))
    const preferred = connection.models.find(model => model.isDefault) ?? connection.models[0]
    if (preferred) { setModelId(preferred.id); setEffort(preferred.defaultReasoningEffort); appDefaults.current = {modelId: preferred.id, effort: preferred.defaultReasoningEffort, readOnly: false} }
    setEngineOnline(true)
    setConnectionReady(true)
    setError('')
  }

  useEffect(() => {
    let active = true
    let unsubscribe: (() => void) | undefined
    if (!desktop) return () => { active = false; unsubscribe?.() }
    const onEvent = (event: RpcEvent) => {
      const params = event.params ?? {}
      const collaborationMode = params.threadSettings?.collaborationMode ?? params.collaborationMode ?? params.thread?.collaborationMode
      if (event.method === 'thread/settings/updated' && typeof params.threadId === 'string'
        && (collaborationMode?.mode === 'default' || collaborationMode?.mode === 'plan')) {
        setModeChoices(previous => ({ ...previous, [params.threadId]: collaborationMode.mode }))
      }
      if (event.method === 'brr/disconnected') {
        setEngineOnline(false)
        if (currentLoginIdRef.current) cancelledLoginIdsRef.current.add(currentLoginIdRef.current)
        loginAttemptRef.current += 1
        loginPendingRef.current = false
        currentLoginIdRef.current = null
        setLoginPending(false)
        setLoginId(null)
        setCancelingLogin(false)
        setError('Coding engine disconnected. Reconnect to continue.')
      }
      if (event.method === 'account/login/completed') {
        const completedLoginId = typeof params.loginId === 'string' ? params.loginId : null
        if (completedLoginId && cancelledLoginIdsRef.current.has(completedLoginId)) return
        if (!loginPendingRef.current) return
        if (completedLoginId && currentLoginIdRef.current && completedLoginId !== currentLoginIdRef.current) return
        loginAttemptRef.current += 1
        loginPendingRef.current = false
        currentLoginIdRef.current = null
        setLoginPending(false)
        setLoginId(null)
        setCancelingLogin(false)
        if (!params.success) setError(typeof params.error === 'string' ? params.error : 'ChatGPT sign-in failed.')
        else void client.connect().then(connection => {
          if (!active) return
          applyConnection(connection)
          if (!connection.account) setError('Sign-in completed, but no ChatGPT account is available.')
        }).catch(reason => { if (active) setError(String(reason)) })
      }
      if (event.method === 'account/updated' && params.authMode === null) {
        void client.account().then(value => {
          if (active) setAccount(value)
        }).catch(reason => { if (active) setError(String(reason)) })
      }
      if (typeof params.threadId === 'string') {
        if (resumingThreads.current.has(params.threadId)) {
          resumeEvents.current[params.threadId] ??= []
          resumeEvents.current[params.threadId].push(event)
        }
        setSessions(previous => {
          const id = params.threadId as string
          const before = previous[id] ?? initialSession(id)
          const after = reduceSession(before, event)
          return after === before ? previous : { ...previous, [id]: after }
        })
      }
    }
    void client.subscribe(onEvent).then(stop => { if (active) unsubscribe = stop; else stop() }).catch(reason => { if (active) setError(String(reason)) })
    void client.connect().then(connection => {
      if (!active) return
      applyConnection(connection)
    }).catch(reason => { if (active) { setEngineOnline(false); setError(String(reason)) } })
    return () => { active = false; unsubscribe?.() }
  }, [desktop])

  const reconnect = async () => {
    const selectedThreadId = threadId
    if (currentLoginIdRef.current) cancelledLoginIdsRef.current.add(currentLoginIdRef.current)
    loginAttemptRef.current += 1
    loginPendingRef.current = false
    currentLoginIdRef.current = null
    setLoginPending(false)
    setLoginId(null)
    setCancelingLogin(false)
    setError('')
    setSessions({})
    hydratedThreads.current.clear()
    resumingThreads.current.clear()
    resumeEvents.current = {}
    try {
      const connection = await client.connect()
      applyConnection(connection)
      if (selectedThreadId) {
        const thread = connection.threads.find(candidate => candidate.id === selectedThreadId)
          ?? { id: selectedThreadId, cwd: folder, preview: '', updatedAt: Date.now() }
        await openThread(thread)
      }
    }
    catch (reason) { setEngineOnline(false); setError(String(reason)) }
  }

  const chooseFolder = async () => {
    if (!connectionReady) return null
    try { return await client.chooseWorkspace() }
    catch (reason) { setError(String(reason)); return null }
  }

  const startProjectChat = (project: ProjectRecord) => {
    setRecoveryOpen(false)
    if (threadId) newChat()
    selectProjectForDraft(project.id)
  }

  const newChat = () => {
    if (projectBusy.current) return
    setRecoveryOpen(false)
    setProjectSettings(null); setProjectAction(null)
    setSettingsOpen(false)
    setAgentsOpen(false)
    setExtensionsOpen(false)
    viewVersion.current += 1
    followTranscript.current = true
    if (conversationScroll.current) conversationScroll.current.scrollTop = 0
    setThreadId(null); setThreadAgentId(''); setComposerAgentId(''); chatDrafts.newDraft(); setComposerNotice(''); setError(''); setupReviewGeneration.current += 1; setSetupReview(null)
    selectProjectForDraft(selectedProjectId)
  }

  const setMode = (next: 'default' | 'plan') => {
    const id = threadId
    if (pending || steeringPending || session.status === 'running' || Boolean(id && resuming[id])) return
    setModeChoices(previous => ({ ...previous, [id ?? 'new']: next }))
    saveDraftSettings({ mode: next })
  }

  const restoreDraftSettings = (draft: SavedDraft, runtimeThread = false) => {
    if (!runtimeThread) {
      const project = projectRecords.find(value => value.id === draft.projectId)
      setSelectedProjectId(project?.id ?? null)
      setFolder(project?.primaryFolder ?? project?.folders[0] ?? '')
      setComposerAgentId(draft.agentId ?? '')
      if (typeof draft.settings.readOnly === 'boolean') setReadOnly(draft.settings.readOnly)
    }
    const model = models.find(candidate => candidate.id === draft.settings.modelId || candidate.model === draft.settings.model)
    if (model) setModelId(model.id)
    if (draft.settings.effort) setEffort(draft.settings.effort)
    const modeFor = draft.threadId ?? 'new'
    if (draft.settings.mode === 'plan' || draft.settings.mode === 'default') setModeChoices(previous => ({ ...previous, [modeFor]: draft.settings.mode }))
  }

  const openSavedDraft = async (draft: SavedDraft) => {
    if (projectBusy.current) return
    if (draft.branchEdit) {
      const branchThread = draft.branchThread
      if (!draft.threadId || draft.id !== draft.threadId || !draft.branchOperationId || branchThread?.id !== draft.threadId) {
        setError('This edit branch could not be opened because its saved thread details are incomplete.')
        setRecoveryOpen(false)
        return
      }
      await openThread(branchThread, draft.agentId || boundAgent(branchThread))
      return
    }
    setRecoveryOpen(false)
    const requestedView = ++viewVersion.current
    setProjectSettings(null); setProjectAction(null); setSettingsOpen(false); setAgentsOpen(false); setExtensionsOpen(false)
    setThreadId(null); setThreadAgentId(''); setError(''); setComposerNotice('')
    const saved = await chatDrafts.switchDraft(draft.id, draft.threadId ?? null)
    if (viewVersion.current !== requestedView) return
    const activeDraft = saved ?? draft
    chatDrafts.context.current = { threadId: null, projectId: activeDraft.projectId ?? '', agentId: activeDraft.agentId ?? '', settings: activeDraft.settings }
    restoreDraftSettings(activeDraft)
  }

  const openThread = async (thread: Thread, boundAgentId = threadAgentIds[thread.id] ?? boundAgent(thread), threadReadOnly?: boolean) => {
    if (projectBusy.current) return
    setRecoveryOpen(false)
    setProjectSettings(null); setProjectAction(null)
    setSettingsOpen(false)
    setAgentsOpen(false)
    setExtensionsOpen(false)
    const requestedView = ++viewVersion.current
    followTranscript.current = true
    setThreadId(thread.id)
    setThreadAgentId(boundAgentId)
    const project = projectRecords.find(value => value.id === thread.projectId)
    setSelectedProjectId(thread.projectId ?? null)
    setFolder(thread.workspace?.folders.length ? thread.workspace.cwd : project?.primaryFolder ?? project?.folders[0] ?? '')
    void chatDrafts.switchDraft(thread.id, thread.id).then(draft => {
      if (viewVersion.current !== requestedView) return
      if (draft) restoreDraftSettings(draft, true)
    })
    if (threadReadOnly !== undefined) hydratedThreads.current.delete(thread.id)
    if (hydratedThreads.current.has(thread.id) || resumingThreads.current.has(thread.id)) return
    resumingThreads.current.add(thread.id)
    setResuming(previous => ({ ...previous, [thread.id]: true }))
    resumeEvents.current[thread.id] = []
    try {
      const result = threadReadOnly === undefined
        ? await client.resume(thread.id)
        : await client.resume(thread.id, threadReadOnly)
      let hydrated = hydrateThread(result.thread)
      const bufferedEvents = resumeEvents.current[thread.id] ?? []
      const snapshotItemIds = new Set(hydrated.items.map(item => item.id))
      for (const event of bufferedEvents) {
        const params = event.params ?? {}
        if (streamingItemDeltaMethods.has(event.method) && snapshotItemIds.has(params.callId ?? params.itemId)) continue
        const eventTurnId = params.turnId ?? params.turn?.id
        const turnAlreadyComplete = hydrated.turnId && eventTurnId === hydrated.turnId && hydrated.status !== 'running'
        if (turnAlreadyComplete && ['turn/started', 'item/started', 'item/completed'].includes(event.method)) continue
        hydrated = reduceSession(hydrated, event)
      }
      setSessions(previous => {
        const live = previous[thread.id]
        if (!live) return { ...previous, [thread.id]: hydrated }
        return { ...previous, [thread.id]: { ...hydrated, items: mergeHydratedItems(hydrated.items, live.items) } }
      })
      setModeChoices(previous => ({ ...previous, [thread.id]: previous[thread.id] ?? hydrated.mode }))
      setThreads(previous => upsertThread(previous, result.thread))
      const resumedAgentId = boundAgent(result.thread) || boundAgentId
      if (resumedAgentId) {
        setThreadAgentIds(current => ({ ...current, [thread.id]: resumedAgentId }))
        setThreadAgentId(resumedAgentId)
      }
      hydratedThreads.current.add(thread.id)
    } catch (reason) { setError(String(reason)) }
    finally {
      resumingThreads.current.delete(thread.id)
      setResuming(previous => ({ ...previous, [thread.id]: false }))
      delete resumeEvents.current[thread.id]
    }
  }

  const openAgentThread = async (thread: Thread, agentId?: string, options?: { readOnly?: boolean; fresh?: boolean; questionId?: string }) => {
    setRecoveryOpen(false)
    const identity = agentId ?? ''
    setThreadAgentIds(current => agentId ? { ...current, [thread.id]: agentId } : current)
    setThreadAgentId(identity)
    setComposerAgentId(identity)
    if (options?.questionId) {
      setQuestionNavigationId(options.questionId)
      setSelectedQuestionId(options.questionId)
    }
    const profile = agentProfiles.find(agent => agent.id === agentId)
    const defaultModel = models.find(model => model.model === profile?.model || model.id === profile?.model)
    if (defaultModel) { setModelId(defaultModel.id); setEffort(profile?.effort || defaultModel.defaultReasoningEffort) }
    setAgentsOpen(false)
    if (options?.fresh) {
      setSettingsOpen(false)
      viewVersion.current += 1
      followTranscript.current = true
      if (conversationScroll.current) conversationScroll.current.scrollTop = 0
      setThreadId(thread.id)
      const project = projectRecords.find(value => value.id === thread.projectId)
      setSelectedProjectId(thread.projectId ?? null)
      setFolder(thread.workspace?.folders.length ? thread.workspace.cwd : project?.primaryFolder ?? project?.folders[0] ?? '')
      setReadOnly(false)
      setError('')
      setResuming(previous => ({ ...previous, [thread.id]: false }))
      resumingThreads.current.delete(thread.id)
      delete resumeEvents.current[thread.id]
      const freshSession = initialSession(thread.id)
      setSessions(previous => ({ ...previous, [thread.id]: freshSession }))
      setModeChoices(previous => ({ ...previous, [thread.id]: previous[thread.id] ?? freshSession.mode }))
      setThreads(previous => upsertThread(previous, thread))
      hydratedThreads.current.add(thread.id)
      return
    }
    await openThread(thread, identity, options?.readOnly)
  }

  const forkFromTurn = async (turnId: string, before = false, prompt?: string) => {
    if (!threadId || session.status === 'running') {
      setError('Wait for the current turn to finish before branching.')
      return
    }
    try {
      const result = await chat.branch(threadId, turnId, before)
      const sourceView = viewVersion.current
      await openThread(result.thread)
      const stillOnBranch = viewVersion.current === sourceView + 1
      if (result.draft && stillOnBranch) {
        chatDrafts.context.current = {
          threadId: result.thread.id,
          projectId: result.draft.projectId ?? '',
          agentId: result.draft.agentId ?? '',
          settings: result.draft.settings,
        }
        draftVersion.current += 1
        chatDrafts.setDocument(result.draft.document)
        restoreDraftSettings(result.draft, true)
        requestAnimationFrame(() => editorRef.current?.focus())
      } else if (prompt !== undefined && stillOnBranch) {
        draftVersion.current += 1
        setText(prompt)
        requestAnimationFrame(() => editorRef.current?.focus())
      }
    } catch (reason) { setError(String(reason)); await refreshPendingBranches() }
  }
  const recoverPendingBranch = async (operationId: string) => {
    if (typeof chat.recoverBranch !== 'function' || recoveringBranchId) return
    setRecoveringBranchId(operationId)
    setBranchRecoveryErrors(previous => ({...previous, [operationId]: ''}))
    try {
      const result = await chat.recoverBranch(operationId)
      await refreshPendingBranches()
      await openThread(result.thread)
    } catch (reason) {
      setBranchRecoveryErrors(previous => ({...previous, [operationId]: String(reason)}))
      await refreshPendingBranches()
    } finally { setRecoveringBranchId('') }
  }
  const editTranscriptItem = (item: AgentItem, turnId: string) => {
    const prompt = itemText(item)
    void forkFromTurn(turnId, true, prompt)
  }

  const steerMessage = async () => {
    const message = text.trim()
    const id = threadId
    const expectedTurnId = session.turnId
    if ((!message && !attachments.length) || !id || !expectedTurnId || !account || !activeModel || steeringPending || pending || session.status !== 'running' || resuming[id]) return
    const requestedView = viewVersion.current
    const requestedDraft = draftVersion.current
    setSteeringPending(true)
    setError('')
    try {
      const result = await chat.submit({
        threadId: id,
        document: chatDrafts.document,
        attachmentIds: attachments.map(attachment => attachment.id),
        settings: { model: activeModel.model, modelId: activeModel.id, effort, mode, readOnly },
        delivery: 'steer',
        expectedTurnId,
      })
      const acknowledgedTurn = result.turn?.id ?? result.turnId ?? result.submission.turnId
      if (acknowledgedTurn && acknowledgedTurn !== expectedTurnId) throw new Error('The runtime returned a different turn for this instruction.')
      const stillTargetingRequest = viewVersion.current === requestedView
        && activeTurnRef.current.threadId === id
        && activeTurnRef.current.turnId === expectedTurnId
      if (stillTargetingRequest && draftVersion.current === requestedDraft) clearSubmittedDraft()
    } catch (reason) {
      if (viewVersion.current === requestedView
        && activeTurnRef.current.threadId === id
        && activeTurnRef.current.turnId === expectedTurnId
        && draftVersion.current === requestedDraft) setError(String(reason))
    } finally { setSteeringPending(false) }
  }

  const submitMessage = (accelerated = false) => {
    if (uiSendLock.current || foregroundSendLock.current) return
    if (steeringPending) return
    const trimmed = text.trim()
    if (pending || Boolean(threadId && resuming[threadId])) return
    const assignment = assignmentFromDraft(chatDrafts.document, agentProfiles)
    if (assignment.kind === 'error') {
      setError('Choose one agent for this message before sending.')
      return
    }
    if (assignment.kind === 'assigned') {
      const referenced = chatDrafts.document.references.filter(reference => reference.type === 'agent' && reference.id === assignment.agent.id)
      let instructions = chatDrafts.document.text
      for (const reference of [...referenced].sort((a, b) => b.start - a.start)) instructions = `${instructions.slice(0, reference.start)}${instructions.slice(reference.end)}`
      if (!referenced.length) {
        const profile = agentProfiles.find(agent => agent.id === assignment.agent.id)
        const mention = profile && !chatDrafts.document.quotes.length ? explicitAgentMention(trimmed, [profile]) : null
        if (mention) instructions = mention.instructions
      }
      void createMentionTask(assignment.agent.id, instructions.trim())
      return
    }
    if (/^\/plan(?:\s|$)/i.test(trimmed)) {
      if (session.status === 'running') {
        setError('Wait for the current turn to finish before changing modes.')
        return
      }
      const instruction = trimmed.replace(/^\/plan\s*/i, '').trim()
      if (!instruction) {
        setMode(mode === 'plan' ? 'default' : 'plan')
        setText('')
        return
      }
      setModeChoices(previous => ({ ...previous, [modeKey]: 'plan' }))
      void sendMessage(instruction, 'plan')
      return
    }
    if (session.status === 'running') {
      if (accelerated) void steerMessage()
      else void queueMessage()
    }
    else void sendMessage()
  }

  const queueMessage = async () => {
    const id = threadId
    const expectedTurnId = session.turnId
    if ((!text.trim() && !attachments.length) || !id || !expectedTurnId || !account || !activeModel || pending || steeringPending || resuming[id]) return
    const requestedView = viewVersion.current
    const requestedDraft = draftVersion.current
    setSteeringPending(true); setError('')
    try {
      await chat.submit({ threadId: id, document: chatDrafts.document, attachmentIds: attachments.map(attachment => attachment.id), settings: { model: activeModel.model, modelId: activeModel.id, effort, mode, readOnly }, delivery: 'queue', expectedTurnId })
      if (viewVersion.current === requestedView && draftVersion.current === requestedDraft) clearSubmittedDraft()
      await refreshQueue(id)
    } catch (reason) {
      if (viewVersion.current === requestedView && draftVersion.current === requestedDraft) setError(String(reason))
    } finally { setSteeringPending(false) }
  }

  const openReferencedFile = async (id: string, type: 'file' | 'folder' = 'file', line?: number) => {
    const targetThread = threadId
    const targetProject = targetThread ? null : selectedProjectId
    const requestedView = viewVersion.current
    if (!targetThread && !targetProject) {
      setError('Choose a project with a registered folder before opening a workspace reference.')
      return
    }
    try {
      const scope = targetThread ? {threadId: targetThread} as const : {projectId: targetProject!} as const
      const resolved = await chat.resolveReference(scope, id, type)
      if (viewVersion.current !== requestedView || (!targetThread && selectedProjectIdRef.current !== targetProject)) return
      setFileOpen({id: crypto.randomUUID(), root: type === 'folder' ? resolved.absolutePath : resolved.root,
        path: type === 'folder' ? '' : resolved.path, ...(line ? {line} : {}), threadId: targetThread ?? null})
      setWorkspaceOpen(true)
    } catch (reason) { if (viewVersion.current === requestedView) setError(String(reason)) }
  }

  const importFiles = async (files: File[]) => {
    const ownerId = chatDrafts.key
    const ownerDocument = chatDrafts.document
    const ownerContext = { ...chatDrafts.context.current, settings: { ...chatDrafts.context.current.settings } }
    try {
      const imported = await chat.importFiles(ownerId, files)
      if (activeDraftKey.current !== ownerId) {
        chatDrafts.updateDraftDocument(ownerId, previous => ({ ...previous, attachmentIds: [...new Set([...(previous.attachmentIds ?? []), ...imported.map(item => item.id)])] }), ownerDocument, ownerContext)
        return
      }
      const current = attachmentsRef.current.filter(item => item.id !== '')
      setDraftAttachments(ownerId, [...current, ...imported.filter(item => !current.some(existing => existing.id === item.id))])
    } catch (reason) { if (activeDraftKey.current === ownerId) setError(`Could not attach files: ${String(reason)}`) }
  }

  const createMentionTask = async (agentId: string, instructions: string) => {
    if (!desktop || !engineOnline || !account) { setError('Sign in with ChatGPT to assign an agent task.'); return }
    const profile = agentProfiles.find(agent => agent.id === agentId)
    if (!profile) { setError('That agent is no longer available. Refresh Agents and try again.'); return }
    const title = instructions.split(/\n/)[0].trim().slice(0, 100) || `Task for ${profile.name}`
    const requestedView = viewVersion.current
    const requestedDraft = draftVersion.current
    const sourceThread = threadId ? threads.find(thread => thread.id === threadId) : undefined
    const boundProjectId = sourceThread?.workspace?.scopeProjectId
      ?? (sourceThread as (Thread & {brr?: {projectId?: string | null}}) | undefined)?.brr?.projectId
      ?? null
    const taskProjectId = threadId ? boundProjectId : selectedProjectId
    setPending(true); setError(''); setComposerNotice('')
    try {
      await chat.assignTask({
        ownerId: chatDrafts.key, document: chatDrafts.document, attachmentIds: attachments.map(attachment => attachment.id),
        task: {
          agentId,
          title,
          instructions,
          acceptanceCriteria: ['Return the requested result with supporting evidence, or report what blocks completion.'],
          ...(taskProjectId ? { projectId: taskProjectId } : threadId ? {} : { cwd: (worktree?.projectPath ?? folder) || undefined }),
          ...(threadId ? { sourceThreadId: threadId } : {}),
          ...(profile.model ? { model: profile.model } : {}),
          ...(profile.effort ? { effort: profile.effort } : {}),
        },
      })
      if (viewVersion.current === requestedView && draftVersion.current === requestedDraft) clearSubmittedDraft()
      if (viewVersion.current === requestedView) setComposerNotice(`Separate task assigned to ${profile.name}. Only this message and its selected context were shared.`)
    } catch (reason) { if (viewVersion.current === requestedView) setError(String(reason)) }
    finally { setPending(false) }
  }

  const sendMessage = async (submittedText = text.trim(), requestedMode = mode, preserveDraft = false): Promise<boolean> => {
    const message = submittedText
    if ((!message && !attachments.length) || !account || !activeModel || pending || foregroundSendLock.current || session.status === 'running' || Boolean(threadId && resuming[threadId])) return false
    foregroundSendLock.current = true
    const requestedView = viewVersion.current
    const requestedDraft = draftVersion.current
    const submittedDraftKey = chatDrafts.key
    const submittedContext = {...chatDrafts.context.current, settings: {...chatDrafts.context.current.settings}}
    const submittedDocument = submittedText === chatDrafts.document.text
      ? chatDrafts.document
      : draftWithText(chatDrafts.document, submittedText)
    const settings = { model: activeModel.model, modelId: activeModel.id, effort, mode: requestedMode, readOnly }
    setPending(true); setError(''); setupReviewGeneration.current += 1; setSetupReview(null)
    try {
      let id = threadId
      let createdThread = false
      if (!id) {
        let result: { thread: Thread }
        if (composerAgentId) {
          const workspace = await client.inspectProject(selectedProjectId)
          const commands = workspace.commands.filter(command => !command.approved)
          if (commands.length) throw new WorktreeSetupRequired(selectedProjectId ?? '', commands)
          result = await agents.open(composerAgentId, { model: activeModel.model, effort, projectId: selectedProjectId, readOnly })
        } else result = await client.start(selectedProjectId, activeModel.model, readOnly)
        id = result.thread.id
        createdThread = true
        if (composerAgentId) {
          setThreadAgentIds(current => ({ ...current, [id!]: composerAgentId }))
          setThreadAgentId(composerAgentId)
        }
        if (viewVersion.current === requestedView) { setThreadId(id); setWorkspacePromotion({draftKey: requestedView, threadId: id}) }
        setThreads(previous => upsertThread(previous, result.thread))
        setSessions(previous => ({ ...previous, [id!]: initialSession(id!) }))
        setModeChoices(previous => ({ ...previous, [id!]: previous.new ?? requestedMode }))
      }
      if (createdThread) {
        await chatDrafts.promote(id, submittedDraftKey, {
          context: {...submittedContext, projectId: selectedProjectId ?? '', agentId: composerAgentId, settings},
          document: submittedDocument,
          shouldAdopt: () => viewVersion.current === requestedView,
        })
      }
      const result = await chat.submit({ threadId: id, document: submittedDocument, attachmentIds: attachments.map(attachment => attachment.id), settings, delivery: 'send' })
      setThreads(previous => previous.map(thread => thread.id === id && !thread.name && !thread.preview
        ? { ...thread, preview: message }
        : thread))
      const turnId = result.turn?.id ?? result.turnId ?? result.submission.turnId
      if (turnId) setSessions(previous => {
        const current = previous[id!] ?? initialSession(id!)
        if (current.turnId === turnId) return previous
        const next = reduceSession(current, { method: 'turn/started', params: { threadId: id, turn: { id: turnId, status: 'inProgress', items: [] } } })
        return { ...previous, [id!]: next }
      })
      setModeChoices(previous => ({ ...previous, [id!]: requestedMode }))
      if (!preserveDraft && viewVersion.current === requestedView && draftVersion.current === requestedDraft) clearSubmittedDraft()
      return true
    } catch (reason) {
      if (viewVersion.current === requestedView) {
        const review = worktreeSetupReview(reason)
        if (review && !threadId && selectedProjectId === review.projectId) {
          setupReviewGeneration.current += 1
          setSetupReview(review)
        }
        else setError(String(reason))
      }
      return false
    }
    finally { foregroundSendLock.current = false; setPending(false) }
  }

  const canSendUi = Boolean(uiReview && uiReview.threadId === threadId && desktop && engineOnline && account && activeModel
    && !pending && !uiSending && !steeringPending && session.status !== 'running' && !pendingQuestions.length
    && !session.approvals.length && !resuming[uiReview.threadId])
  const uiBlocked = !engineOnline || !account || !activeModel || pending || steeringPending || session.status === 'running'
    || pendingQuestions.length > 0 || session.approvals.length > 0 || Boolean(threadId && resuming[threadId])
  const uiSendContext = useRef({threadId, mode, blocked: uiBlocked})
  uiSendContext.current = {threadId, mode, blocked: uiBlocked}
  const reviewUiAction = (review: UiActionReview) => {
    if (review.threadId !== threadId) return
    setUiReview(review); setUiError('')
  }
  const expandUi = (identity: UiIdentity, title: string) => { setExpandedUi({identity, title}); setWorkspaceOpen(true) }
  const sendUiFollowup = async () => {
    if (!uiReview || !canSendUi || uiSendLock.current || !activeModel) return
    uiSendLock.current = true; setUiSending(true); setUiError('')
    const review = uiReview, requestedView = viewVersion.current, requestedMode = mode
    let ownsForegroundLock = false
    try {
      const claimed = await claimUiAction(review.reviewId)
      const taskRoute = (task: UiActionReview['task']) => task ? JSON.stringify([task.id, task.agentId, task.projectId ?? null, task.cwd ?? null]) : null
      if (claimed.reviewId !== review.reviewId || claimed.documentHash !== review.documentHash || taskRoute(claimed.task) !== taskRoute(review.task)
        || claimed.threadId !== review.threadId || claimed.callId !== review.callId || claimed.text !== review.text
        || uiSendContext.current.threadId !== review.threadId || uiSendContext.current.blocked || viewVersion.current !== requestedView) throw new Error('The conversation or reviewed content changed. Prepare a new follow-up.')
      if (claimed.task) {
        if (foregroundSendLock.current) throw new Error('Another message is being submitted. Prepare a new follow-up.')
        foregroundSendLock.current = true; ownsForegroundLock = true; setPending(true)
        const profile = agentProfiles.find(agent => agent.id === claimed.task!.agentId)
        const model = models.find(candidate => candidate.model === profile?.model || candidate.id === profile?.model) ?? activeModel
        const result = await agents.open(claimed.task.agentId, { projectId: typeof claimed.task.projectId === 'string' ? claimed.task.projectId : null, readOnly })
        if (viewVersion.current !== requestedView || uiSendContext.current.threadId !== review.threadId || uiSendContext.current.mode !== requestedMode) throw new Error('The conversation changed before sending. The new agent chat is retained; prepare a new follow-up.')
        await openAgentThread(result.thread, claimed.task.agentId, {fresh: true})
        setModeChoices(previous => ({...previous, [result.thread.id]: requestedMode}))
        const sent = await chat.submit({
          threadId: result.thread.id,
          document: draftWithText(emptyDraft(), claimed.text),
          attachmentIds: [],
          settings: { model: model.model, modelId: model.id, effort: profile?.effort ?? effort, mode: requestedMode, readOnly },
          delivery: 'send',
        })
        const sentTurnId = sent.turn?.id ?? sent.turnId ?? sent.submission.turnId
        if (!sent.queued && sentTurnId) setSessions(previous => {
          const current = previous[result.thread.id] ?? initialSession(result.thread.id)
          if (current.turnId) return previous
          return {...previous, [result.thread.id]: reduceSession(current, {method: 'turn/started', params: {threadId: result.thread.id, turn: {id: sentTurnId, items: []}}})}
        })
        setModeChoices(previous => ({...previous, [result.thread.id]: requestedMode}))
        setExpandedUi(null); setUiReview(null)
      } else {
        if (await sendMessage(claimed.text, uiSendContext.current.mode, true)) setUiReview(null)
        else throw new Error('Follow-up delivery was not confirmed. Inspect the conversation before preparing another.')
      }
    } catch (reason) { setUiError(String(reason)); if (activeTurnRef.current.threadId !== review.threadId) setError(String(reason)) }
    finally { if (ownsForegroundLock) {foregroundSendLock.current = false; setPending(false)} uiSendLock.current = false; setUiSending(false) }
  }

  const approveSetupHooks = async () => {
    const review = setupReview
    if (!review || setupApprovalPending) return
    const requestedView = viewVersion.current
    const requestedGeneration = setupReviewGeneration.current
    setSetupApprovalPending(true)
    setError('')
    try {
      const grouped = new Map<string, string[]>()
      for (const command of review.commands) {
        const cwd = command.cwd ?? ''
        if (cwd) grouped.set(cwd, [...(grouped.get(cwd) ?? []), command.template])
      }
      for (const [cwd, templates] of grouped) await client.approveWorktree(cwd, templates)
      if (viewVersion.current === requestedView && selectedProjectId === review.projectId && setupReviewGeneration.current === requestedGeneration) {
        setupReviewGeneration.current += 1
        setSetupReview(null)
      }
    } catch (reason) {
      if (viewVersion.current === requestedView && selectedProjectId === review.projectId && setupReviewGeneration.current === requestedGeneration) setError(String(reason))
    } finally { setSetupApprovalPending(false) }
  }

  const stopTurn = async () => {
    if (!threadId || !session.turnId) return
    try { await client.interrupt(threadId, session.turnId) }
    catch (reason) { setError(String(reason)) }
  }

  const archiveThread = async () => {
    const archivedThreadId = threadId
    if (!archivedThreadId) return
    const requestedView = viewVersion.current
    try {
      await client.archive(archivedThreadId)
      // An empty-prefix edit branch remains a saved draft until the host's
      // archive journal is reflected in drafts.list. Refresh before removing
      // the runtime row so it cannot briefly reappear as an ordinary draft.
      chatDrafts.hideArchivedBranch(archivedThreadId)
      await chatDrafts.refresh()
      setThreads(previous => previous.filter(thread => thread.id !== archivedThreadId))
      if (viewVersion.current === requestedView) newChat()
    } catch (reason) { setError(String(reason)) }
  }

  const confirmProjectAction = async () => {
    if (!projectAction || projectBusy.current) return
    const {project, action} = projectAction
    projectBusy.current = true; setProjectPending(true); setProjectActionError('')
    try {
      if (action === 'delete') {
        await client.deleteProject(project.id)
        rememberProject({...project, hidden: true, revision: project.revision + 1})
        if (selectedProjectId === project.id) { setSelectedProjectId(null); setFolder(''); applyProjectDefaults() }
        setProjectAction(null)
      } else {
        const result = await client.archiveProject(project.id)
        const archived = new Set(result.archivedIds)
        setThreads(previous => previous.filter(thread => !archived.has(thread.id)))
        setThreadId(current => current && archived.has(current) ? null : current)
        if (threadId && archived.has(threadId)) { setThreadAgentId(''); setComposerAgentId(''); applyProjectDefaults(selectedProjectId === project.id ? project : undefined) }
        setProjectActionResult(previous => ({
          archivedIds: [...new Set([...(previous?.archivedIds ?? []), ...result.archivedIds])],
          skipped: result.skipped,
          failed: result.failed,
        }))
      }
    } catch (reason) { setProjectActionError(String(reason)) }
    finally {
      try { setThreads(await client.threads()); const loaded = await client.projects(); acceptLoadedProjects(loaded.data) }
      catch (reason) { setProjectActionError(`Could not refresh project chats: ${String(reason)}`) }
      projectBusy.current = false; setProjectPending(false)
    }
  }

  const respond = async (approval: Approval, result: Record<string, unknown>) => {
    try {
      await client.respond(approval.id, result)
      if (threadId) setSessions(previous => {
        const current = previous[threadId] ?? initialSession(threadId)
        const next = reduceSession(current, { method: 'serverRequest/resolved', params: { threadId, requestId: approval.id } })
        return { ...previous, [threadId]: next }
      })
    } catch (reason) { setError(String(reason)) }
  }

  const proposedPlan = session.proposedPlan
  const canImplementPlan = Boolean(
    proposedPlan?.text.trim()
      && proposedPlan.isComplete
      && proposedPlan.turnId === session.turnId
      && session.status === 'idle'
      && mode === 'plan'
      && !text.trim()
      && account
      && activeModel
      && engineOnline
      && !pending
      && !steeringPending
      && !(threadId && resuming[threadId]),
  )

  const login = async () => {
    const attempt = ++loginAttemptRef.current
    loginPendingRef.current = true
    currentLoginIdRef.current = null
    setLoginPending(true); setError('')
    setLoginId(null)
    try {
      const result = await client.login()
      if (loginAttemptRef.current !== attempt) return
      currentLoginIdRef.current = result.loginId
      setLoginId(result.loginId)
    }
    catch (reason) {
      if (loginAttemptRef.current === attempt) {
        loginPendingRef.current = false
        currentLoginIdRef.current = null
        setLoginPending(false); setLoginId(null); setCancelingLogin(false); setError(String(reason))
      }
    }
  }

  const cancelSignIn = async () => {
    if (!loginId || cancelingLogin) return
    setCancelingLogin(true)
    try {
      const result = await client.cancelLogin(loginId) as {status?: string}
      if (currentLoginIdRef.current === loginId && result?.status === 'canceled') {
        cancelledLoginIdsRef.current.add(loginId)
        loginAttemptRef.current += 1
        loginPendingRef.current = false
        currentLoginIdRef.current = null
        setLoginPending(false)
        setLoginId(null)
      }
    } catch (reason) {
      setError(String(reason))
    } finally { setCancelingLogin(false) }
  }

  const workspaceVisible = !profileOpen && !agentsOpen && !extensionsOpen && !ticketsOpen && !projectSettings && !projectAction && !settingsOpen && !recoveryOpen
  const compactWorkspace = workspaceOpen && workspaceVisible && workspaceNarrow
  const expandedWorkspace = workspaceOpen && workspaceVisible && workspaceExpanded
  const effectiveSidebarCollapsed = sidebarCollapsed || compactWorkspace || expandedWorkspace
  const renderComposerToolbar = (contextButton?: ReactNode) => <div className="composer-toolbar">{contextButton}
              <div className="composer-context">
                  <label className="composer-project-choice"><span className="sr-only">{threadId ? 'Move chat to project' : 'Project'}</span><select aria-label={threadId ? 'Move chat to project' : 'Project'} value={threadId ? (threads.find(thread => thread.id === threadId)?.projectId ?? '') : selectedProjectId ?? ''} disabled={!desktop || projectPending || Boolean(threadId && movingChatIds.includes(threadId))} onChange={event => {
                  const nextId = event.target.value || null
                  if (threadId) { moveChatToProject(threadId, nextId); return }
                  selectProjectForDraft(nextId)
                  setupReviewGeneration.current += 1; setSetupReview(null)
                }}><option value="">No project</option>{projectRecords.map(project => <option key={project.id} value={project.id}>{project.name}</option>)}</select></label>
                <Button className="mode-toggle" variant="ghost" aria-label="Plan mode" aria-pressed={mode === 'plan'} title="Plan mode (Shift+Tab)" disabled={pending || steeringPending || session.status === 'running' || Boolean(threadId && resuming[threadId])} onClick={() => setMode(mode === 'plan' ? 'default' : 'plan')}>Plan</Button>
                {!threadId && readOnly && <span className="composer-access-indicator" title="Read-only access"><LockKeyhole size={13} aria-hidden="true" /><span className="sr-only">Read only</span></span>}
              </div>
              <div className="composer-send-actions">
                <div className="composer-model-controls">
                  <label className="model-choice"><span className="sr-only">Model</span><span className="select-wrap"><span className="select-sizing" aria-hidden="true">{activeModel?.displayName ?? 'Brr'}</span><select aria-label="Model" disabled={!models.length} value={activeModel?.id ?? ''} onChange={event => { const next = models.find(model => model.id === event.target.value); if (next) { setModelId(next.id); setEffort(next.defaultReasoningEffort); saveDraftSettings({model: next.model, modelId: next.id, effort: next.defaultReasoningEffort}); appDefaults.current = {...appDefaults.current, modelId: next.id, effort: next.defaultReasoningEffort} } }}>
                    {models.length === 0 && <option value="">Brr</option>}
                    {models.map(model => <option key={model.id} value={model.id}>{model.displayName}</option>)}
                  </select><ChevronsUpDown aria-hidden="true" size={14}/></span></label>
                </div>
                <ComposerOptions summary={activeChatAgent?.name} title={`Chat options${activeChatAgent ? ` · ${activeChatAgent.name}` : ''} · ${effort} reasoning`}>
                  {activeModel && <label className="composer-option"><span>Reasoning</span><select aria-label="Reasoning effort" value={effort} onChange={event => { setEffort(event.target.value); saveDraftSettings({effort: event.target.value}); appDefaults.current.effort = event.target.value }}>
                    {activeModel.supportedReasoningEfforts.map(option => option.reasoningEffort).map(value => <option key={value} value={value}>{value}</option>)}
                  </select></label>}
                  <label className="composer-option"><span>Chat as</span><select aria-label="Chat identity" value={threadId ? threadAgentId : composerAgentId} disabled={Boolean(threadId)} onChange={event => {
                    const agentId = event.target.value
                    setComposerAgentId(agentId)
                    chatDrafts.updateContext({agentId})
                    const profile = agentProfiles.find(agent => agent.id === agentId)
                    const defaultModel = models.find(model => model.model === profile?.model || model.id === profile?.model)
                    if (profile && defaultModel) { const nextEffort = profile.effort || defaultModel.defaultReasoningEffort; setModelId(defaultModel.id); setEffort(nextEffort); saveDraftSettings({model: defaultModel.model, modelId: defaultModel.id, effort: nextEffort}) }
                  }}><option value="">Brr</option>{agentProfiles.map(profile => <option key={profile.id} value={profile.id}>{profile.name}</option>)}</select></label>
                  {threadId && <label className="composer-option"><span>Move to project</span><select aria-label="Move to project" value={threads.find(thread => thread.id === threadId)?.projectId ?? ''} disabled={!desktop || projectPending || movingChatIds.includes(threadId)} onChange={event => moveChatToProject(threadId, event.target.value || null)}><option value="">No project</option>{projectRecords.map(project => <option key={project.id} value={project.id}>{project.name}</option>)}</select></label>}
                  {threadId ? <p className="composer-option-note">Access is set by this chat's saved policy.</p> : <label className="composer-option composer-option-checkbox"><span>Read only</span><input type="checkbox" aria-label="Read only" checked={readOnly} onChange={event => { setReadOnly(event.target.checked); saveDraftSettings({readOnly: event.target.checked}); appDefaults.current.readOnly = event.target.checked }} /></label>}
                  {worktree ? <p className="composer-option-note" aria-label="Session worktree" title={worktree.path}>Worktree · {worktree.branch}</p> : !threadId && folder && <p className="composer-option-note">New chats use a separate worktree.</p>}
                  {agentProfiles.length > 0 && <p className="composer-option-note">Type @Agent name to assign a separate task. Only this message is shared.</p>}
                </ComposerOptions>
              {session.status === 'running' && <details ref={sendMoreMenuRef} className="send-more" onKeyDown={event => {
                if (event.key !== 'Escape' || !event.currentTarget.open) return
                event.preventDefault(); event.stopPropagation()
                event.currentTarget.open = false
                event.currentTarget.querySelector('summary')?.focus()
              }}><summary aria-label="More send options" title="More send options"><MoreHorizontal size={17} aria-hidden="true" /></summary><div role="menu" aria-label="Running message actions"><button type="button" role="menuitem" onClick={() => {
                if (sendMoreMenuRef.current) sendMoreMenuRef.current.open = false
                sendMoreMenuRef.current?.querySelector('summary')?.focus()
                void submitMessage(true)
              }}>Steer now</button></div></details>}
              <Button className="send-control" aria-label={session.status === 'running' ? 'Queue message' : 'Send message'} disabled={!desktop || !engineOnline || !account || !activeModel || (!text.trim() && !attachments.length) || pending || uiSending || steeringPending || Boolean(threadId && resuming[threadId]) || (session.status === 'running' && (!threadId || !session.turnId))} onClick={() => submitMessage(false)}>{pending || steeringPending ? <LoaderCircle className="spin" size={17}/> : <ArrowUp size={19}/>}</Button>
                {session.status === 'running' && <Button className="send-control" aria-label="Stop" variant="secondary" disabled={!engineOnline} onClick={() => void stopTurn()}><Square size={14} fill="currentColor"/></Button>}
              </div>
            </div>;

  return <div className={`app-shell${effectiveSidebarCollapsed ? ' sidebar-collapsed' : ''}`} data-workspace-open={workspaceOpen && workspaceVisible} data-workspace-expanded={expandedWorkspace} data-section={profileOpen ? 'profile' : agentsOpen ? 'agents' : extensionsOpen ? 'plugins' : ticketsOpen ? 'tickets' : recoveryOpen ? 'saved-messages' : 'home'} style={{ '--shell-background': sidebarAppearance.background, '--workspace-width': `${workspaceWidth}px` } as CSSProperties}>
    <header className="desktop-toolbar" data-tauri-drag-region style={{ '--toolbar-background': sidebarAppearance.background, '--toolbar-foreground': sidebarAppearance.foreground } as CSSProperties}>
      <div className="desktop-history">
        <Button variant="ghost" size="icon" aria-label="Back" title="Back" disabled><ArrowLeft size={17} aria-hidden="true" /></Button>
        <Button variant="ghost" size="icon" aria-label="Forward" title="Forward" disabled><ArrowRight size={17} aria-hidden="true" /></Button>
      </div>
      {!profileOpen && <Button ref={sidebarToggle} className="desktop-sidebar-toggle" variant="ghost" size="icon" aria-label={effectiveSidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'} title={effectiveSidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'} aria-controls={agentsOpen ? 'agents-sidebar' : extensionsOpen ? 'plugins-sidebar' : ticketsOpen ? 'tickets-sidebar' : 'main-sidebar'} aria-expanded={!effectiveSidebarCollapsed} onClick={toggleSidebar}>
        {effectiveSidebarCollapsed ? <PanelLeftOpen size={18} aria-hidden="true" /> : <PanelLeftClose size={18} aria-hidden="true" />}
      </Button>}
    </header>
    <nav className="main-rail" aria-label="Main rail" style={{ '--rail-background': sidebarAppearance.background, '--rail-foreground': sidebarAppearance.foreground } as CSSProperties}>
      <div className="rail-top">
        <RailButton className="rail-home" label="Home" disabled={projectPending} aria-current={!profileOpen && !agentsOpen && !extensionsOpen && !ticketsOpen && !settingsOpen && !recoveryOpen ? 'page' : undefined} onClick={() => { setRecoveryOpen(false); setProjectSettings(null); setProjectAction(null); setSettingsOpen(false); setAgentsOpen(false); setExtensionsOpen(false); setTicketsOpen(false); setProfileOpen(false) }}><House size={19} aria-hidden="true" />{desktop && connectionReady && engineOnline && <span className="rail-status" aria-hidden="true" />}</RailButton>
        <RailButton className="rail-agents" label="Agents" aria-current={!profileOpen && agentsOpen ? 'page' : undefined} disabled={!desktop || !engineOnline || projectPending} onClick={() => { setRecoveryOpen(false); setProjectSettings(null); setProjectAction(null); setSettingsOpen(false); setExtensionsOpen(false); setTicketsOpen(false); setProfileOpen(false); setAgentsOpen(true) }}><CircleUserRound size={18} aria-hidden="true" />{agentAlertCount > 0 && <span className="agent-alert-count" aria-label={`${agentAlertCount} agent alerts`}>{agentAlertCount}</span>}</RailButton>
        <RailButton className="rail-tickets" label="Tickets" aria-current={!profileOpen && ticketsOpen ? 'page' : undefined} disabled={!desktop || projectPending} onClick={() => { setRecoveryOpen(false); setProjectSettings(null); setProjectAction(null); setSettingsOpen(false); setAgentsOpen(false); setExtensionsOpen(false); setProfileOpen(false); setTicketProjectId(''); setTicketsOpen(true) }}><TicketCheck size={18} aria-hidden="true" /></RailButton>
        <RailButton className="rail-plugins" label="Plugins" aria-current={!profileOpen && extensionsOpen ? 'page' : undefined} disabled={!desktop || !engineOnline || projectPending} onClick={() => { setRecoveryOpen(false); setProjectSettings(null); setProjectAction(null); setSettingsOpen(false); setAgentsOpen(false); setTicketsOpen(false); setProfileOpen(false); setExtensionsOpen(true) }}><LayoutGrid size={18} aria-hidden="true" /></RailButton>
      </div>
      <div className="rail-bottom">
        <RailButton ref={profileTrigger} className="rail-profile" label="Profile" disabled={projectPending} aria-current={profileOpen ? 'page' : undefined} onClick={() => { setRecoveryOpen(false); profileOpen ? profilePanel.current?.focus() : setProfileOpen(true) }}><span className="rail-avatar" aria-hidden="true">{account?.email?.split('@')[0].split(/[._-]/).filter(Boolean).slice(0, 2).map(part => part[0]).join('').toUpperCase() || 'B'}</span></RailButton>
        <RailButton ref={settingsTrigger} className="rail-settings" label="Settings" disabled={projectPending} aria-current={!profileOpen && settingsOpen ? 'page' : undefined} onClick={() => { setRecoveryOpen(false); setProjectSettings(null); setProjectAction(null); setAgentsOpen(false); setExtensionsOpen(false); setTicketsOpen(false); setProfileOpen(false); setSettingsOpen(true) }}><Settings size={18} aria-hidden="true" /></RailButton>
      </div>
    </nav>
    <aside id="main-sidebar" className="sidebar" aria-label="Main sidebar" hidden={effectiveSidebarCollapsed || agentsOpen || extensionsOpen || ticketsOpen || profileOpen}>
      <div className="sidebar-header" data-tauri-drag-region><span className="wordmark">Brr</span><Button variant="ghost" size="icon" aria-label="Search chats" title="Search chats" onClick={openChatSearch}><Search size={16} aria-hidden="true" /></Button></div>
      <nav className="sidebar-actions" aria-label="Workspace">
        <Button className="new-chat" aria-label="New conversation" onClick={newChat}><SquarePen size={17}/>New chat</Button>
        <Button variant="ghost" disabled={!desktop || !engineOnline} onClick={() => { setRecoveryOpen(false); setProjectSettings(null); setProjectCreating(true) }}><FolderOpen size={17}/>New project</Button>
        <Button variant="ghost" aria-label="Saved messages" aria-current={recoveryOpen ? 'page' : undefined} disabled={!desktop} onClick={showSavedMessages}>Saved messages</Button>
      </nav>
      {searching && <input ref={searchInput} className="thread-search" aria-label="Search chats" placeholder="Search chats" value={query} onChange={event => setQuery(event.target.value)} />}
      <div className="sidebar-scroll">
      <section className="sidebar-section" aria-label="Projects">
        <h2 className="sidebar-section-title">Projects</h2>
        {projectLoadError && <div className="project-load-error" role="alert">{projectLoadError}<Button variant="ghost" onClick={() => void loadProjects()}>Retry</Button></div>}
        {(projectsReady || !desktop) && projectGroups.map(({project, chats: projectThreads}) => {
          const projectId = project.id
          const expanded = expandedProjects.has(projectId)
          const projectName = project.name
          return <div className="project-group" key={projectId}>
            <div className="project-row-header">
            <button className="project-row" type="button" aria-label={`New chat in ${projectName}`} onClick={() => startProjectChat(project)}>
              <FolderOpen size={15} aria-hidden="true" /><span>{projectName}</span>
            </button>
            <button className="project-disclosure" type="button" aria-label={`${expanded ? 'Collapse' : 'Expand'} chats in ${projectName}`} data-project-id={projectId} aria-expanded={expanded} onClick={() => setExpandedProjects(current => {
              const next = new Set(current)
              if (next.has(projectId)) next.delete(projectId)
              else next.add(projectId)
              return next
            })}><ChevronsUpDown size={14} aria-hidden="true" /></button>
            <ProjectMenu projectName={projectName} projectId={projectId} disabled={!desktop || projectPending} actions={[
              {id: 'new-chat', label: 'New chat', onSelect: () => startProjectChat(project)},
              {id: 'settings', label: 'Project settings', restoreFocus: false, onSelect: () => { setRecoveryOpen(false); setProjectAction(null); setProjectSettings(project) }},
              {id: 'tickets', label: 'Open tickets', restoreFocus: false, onSelect: () => { setRecoveryOpen(false); setProjectSettings(null); setTicketProjectId(projectId); setAgentsOpen(false); setExtensionsOpen(false); setSettingsOpen(false); setTicketsOpen(true) }},
              ...(project.folders.length ? [] : [{id: 'reveal-empty', label: 'No folders to reveal', disabled: true, onSelect: () => {}}]),
              ...project.folders.map((path, index) => ({id: `reveal-${index}`, label: `${/Mac/i.test(navigator.platform) ? 'Reveal in Finder' : /Win/i.test(navigator.platform) ? 'Reveal in File Explorer' : 'Open in file manager'} · ${path}`, onSelect: () => client.revealProject(path)})),
              {id: 'archive', label: 'Archive all chats', disabled: !engineOnline, restoreFocus: false, onSelect: () => { setRecoveryOpen(false); setProjectSettings(null); setProjectActionResult(null); setProjectActionError(''); setProjectAction({project, action: 'archive'}) }},
              {id: 'delete', label: 'Delete project', disabled: !engineOnline, destructive: true, restoreFocus: false, onSelect: () => { setRecoveryOpen(false); setProjectSettings(null); setProjectActionResult(null); setProjectActionError(''); setProjectAction({project, action: 'delete'}) }},
            ]} />
            </div>
            {expanded && <div className="project-threads">{projectThreads.map(thread => <button className={`thread-row project-thread${threadId === thread.id ? ' selected' : ''}`} key={thread.id} aria-label={`Open ${thread.name || thread.preview || 'New chat'}`} aria-current={threadId === thread.id ? 'page' : undefined} onClick={() => void openThread(thread)} title={thread.preview}><span>{thread.name || thread.preview || 'New chat'}</span></button>)}</div>}
          </div>
        })}
      </section>
      <section className="sidebar-section" aria-label="Recents">
        <h2 className="sidebar-section-title">Recents</h2>
      {sidebarDrafts.length > 0 && <div className="thread-list" aria-label="Drafts">
        {sidebarDrafts.map(draft => {
          const preview = draft.document.text.trim().split(/\n/)[0]?.slice(0, 72) || 'Untitled draft'
          return <button key={draft.id} type="button" className={`thread-row draft-row${chatDrafts.key === draft.id ? ' selected' : ''}`} aria-label={draft.branchEdit ? `Edit branch draft ${preview}` : `Draft ${preview}`} aria-current={chatDrafts.key === draft.id ? 'page' : undefined} onClick={() => void openSavedDraft(draft)} title={draft.branchEdit ? `Edit branch before its first turn · ${draft.document.text}` : draft.document.text}>
            {draft.branchEdit && <small className="draft-kind">Edit branch</small>}
            <span>{preview}</span>
          </button>
        })}
      </div>}
      <div className="thread-list" aria-label="Chats">
        {visibleThreads.map(thread => <button key={thread.id} className={`thread-row ${threadId === thread.id ? 'selected' : ''}`} aria-current={threadId === thread.id ? 'page' : undefined} onClick={() => void openThread(thread)} title={thread.preview}><span>{thread.name || thread.preview || 'New chat'}</span></button>)}
      </div>
      </section>
      </div>
    </aside>
    <main className="main-panel">
      {!profileOpen && !agentsOpen && !extensionsOpen && !ticketsOpen && !projectSettings && !projectAction && !recoveryOpen && <div className="titlebar" inert={expandedWorkspace}>
        <div className="titlebar-actions">
          <Button variant="ghost" size="icon" aria-label="New tab" title="New tab" onClick={() => { setWorkspaceOpen(true); setWorkspaceRequest(value => value + 1) }}><SquarePlus size={17} aria-hidden="true" /></Button>
          {threadId && <Button variant="ghost" onClick={() => void archiveThread()}>Archive</Button>}
        </div>
      </div>}
      {recoveryOpen ? <SavedMessages onRestore={async draft => { setRecoveryOpen(false); await openSavedDraft(draft) }} interruptedSetupCount={dismissedBranchNoticeCount} onReviewInterruptedSetups={reviewDismissedBranchNotices} /> : profileOpen ? <section ref={profilePanel} className="profile-page" aria-label="User profile" tabIndex={-1}>
        <header className="profile-page-header"><h1>Profile</h1><Button variant="ghost" size="icon" aria-label="Close profile" onClick={closeProfile}><X size={16} aria-hidden="true" /></Button></header>
        <section className="profile-account" aria-label="Account">
          <h2>Account</h2>
          {account?.email && <p className="profile-email">{account.email}</p>}
          {desktop && !engineOnline && <div className="engine-offline" role="status"><span>Coding engine disconnected</span><Button variant="ghost" onClick={() => void reconnect()}>Reconnect</Button></div>}
          {account ? <Button variant="ghost" onClick={() => void client.logout().then(() => setAccount(null)).catch(reason => setError(String(reason)))}>Sign out</Button> : <><Button disabled={!desktop || !engineOnline || loginPending} onClick={() => void login()}>Sign in with ChatGPT</Button>{loginPending && <Button variant="ghost" disabled={!loginId || cancelingLogin} onClick={() => void cancelSignIn()}>{cancelingLogin ? 'Cancelling…' : 'Cancel sign-in'}</Button>}</>}
        </section>
        {loginPending && <p className="login-pending">Complete sign-in in your browser.</p>}
        {error && <p className="app-error" role="alert">{error}</p>}
      </section> : projectSettings ? <ProjectDialog key={projectSettings.id} project={projectSettings} models={models} onChooseFolders={client.chooseFolders} onSave={async input => {
        if (!projectSettings) return
        projectBusy.current = true; setProjectPending(true)
        try {
          const saved = await client.saveProject({...projectSettings, ...input} as ProjectRecord)
          rememberProject(saved)
          if (!threadId && selectedProjectId === saved.id) { setFolder(saved.primaryFolder ?? saved.folders[0] ?? ''); applyProjectDefaults(saved) }
          setProjectSettings(null); focusProjectMenu(saved.id)
        } finally { projectBusy.current = false; setProjectPending(false) }
      }} onClose={closeProjectPage} /> : projectCreating ? <ProjectDialog models={models} onChooseFolders={client.chooseFolders} onSave={async input => {
        projectBusy.current = true; setProjectPending(true)
        try {
          const saved = await client.createProject(input as ProjectInput)
          rememberProject(saved); setSelectedProjectId(saved.id); setFolder(saved.primaryFolder ?? saved.folders[0] ?? '')
          setThreadId(null); setThreadAgentId(''); setComposerAgentId(''); chatDrafts.newDraft(); applyProjectDefaults(saved)
          setProjectCreating(false); setError('')
        } finally { projectBusy.current = false; setProjectPending(false) }
      }} onClose={closeProjectPage} /> : projectAction ? <ProjectActionReview key={`${projectAction.project.id}-${projectAction.action}`} project={projectAction.project} action={projectAction.action} chatCount={threads.filter(thread => thread.projectId === projectAction.project.id).length} pending={projectPending} result={projectActionResult} error={projectActionError} onConfirm={() => void confirmProjectAction()} onClose={closeProjectPage} /> : settingsOpen ? <AppearanceSettings value={sidebarColor} onChange={chooseSidebarColor} onClose={() => { setSettingsOpen(false); settingsTrigger.current?.focus(); requestAnimationFrame(() => settingsTrigger.current?.focus()) }} saveFailed={appearanceSaveFailed} /> : agentsOpen && desktop ? <AgentsPanel models={models} desktop={desktop} online={engineOnline} projectId={selectedProjectId} cwd={(worktree?.projectPath ?? folder) || undefined} chooseFolder={chooseFolder} questionError={questionError} onClearQuestionError={() => setQuestionError('')} onOpenThread={openAgentThread} onClose={() => setAgentsOpen(false)} /> : ticketsOpen ? null : extensionsOpen && desktop ? <ExtensionsPanel cwd={(worktree?.projectPath ?? folder) || undefined} threadId={threadId ?? undefined} onUsePrompt={prompt => { draftVersion.current += 1; setText(previous => previous ? `${previous}\n\n${prompt}` : prompt); setExtensionsOpen(false) }} onClose={() => setExtensionsOpen(false)} /> : <div className="workspace-body" inert={expandedWorkspace}>
        <section className="conversation-panel">
          {questionError && <p className="app-error" role="alert">Question delivery needs attention: {questionError} <Button size="sm" variant="ghost" onClick={() => setQuestionError('')}>Dismiss</Button></p>}
          {visibleBranchNotices.length > 0 && <section className="branch-recovery-banner" aria-label="Interrupted branch setup">
            <header className="branch-recovery-header"><p>A branch setup was interrupted. You can recover it whenever you’re ready.</p><Button type="button" variant="ghost" size="icon" aria-label="Dismiss interrupted setup notices" title="Dismiss" disabled={Boolean(recoveringBranchId) || visibleBranchNotices.some(notice => notice.operation.id === recoveringBranchId)} onClick={dismissBranchNotices}><X size={16} aria-hidden="true" /></Button></header>
            {visibleBranchNotices.map(({ operation }) => <div className="branch-recovery-row" key={operation.id}>
              <span>{operation.phase}{operation.cwd ? ` · ${operation.cwd}` : ''}{operation.error ? ` · ${operation.error}` : ''}</span>
              <Button size="sm" variant="secondary" disabled={Boolean(recoveringBranchId)} onClick={() => void recoverPendingBranch(operation.id)}>{recoveringBranchId === operation.id ? 'Recovering…' : 'Recover setup'}</Button>
              {branchRecoveryErrors[operation.id] && <span className="branch-recovery-error" role="status">{branchRecoveryErrors[operation.id]}</span>}
            </div>)}
          </section>}
          {!threadId && !setupReview && session.items.length === 0 ? <div className="welcome"><div className="welcome-symbol"><Code2 size={30} strokeWidth={1.5}/></div><h1>{selectedProject ? `New chat in ${selectedProject.name}` : 'What are we building?'}</h1>{!account && <p>Sign in with ChatGPT to start a coding session.</p>}{selectedProject && (selectedProject.folders.length ? <ul className="welcome-project-folders" aria-label="Project folders">{selectedProject.folders.map(path => <li key={path}>{path === (selectedProject.primaryFolder ?? selectedProject.folders[0]) && <span className="welcome-primary-folder">Starting folder</span>}<span>{path}</span></li>)}</ul> : <p>No folders attached</p>)}</div> : <ChatTranscript
            session={transcriptSession}
            threadId={threadId ?? 'draft'}
            actionsDisabled={session.status === 'running'}
            renderItem={item => {
              const inlineQuestions = activeQuestions.filter(question => question.itemId === item.id)
              const duplicateQuestionText = item.type === 'agentMessage' && inlineQuestions.some(question => question.kind === 'async')
              const duplicateQuestionOutput = item.type === 'functionCallOutput' && item.name === 'request_user_input' && inlineQuestions.length > 0
              return <>{!duplicateQuestionText && !duplicateQuestionOutput && <AgentMessage item={item} threadId={threadId ?? undefined} onExpandUi={expandUi} onUiReview={reviewUiAction} expandedUiCallId={workspaceOpen && expandedUi?.identity.threadId === threadId ? expandedUi.identity.callId : undefined} uiActive={!compactWorkspace && !expandedWorkspace} onUsePrompt={prompt => editorRef.current?.insertText(`${text ? '\n\n' : ''}${prompt}`)}/>} {inlineQuestions.map(question => <QuestionTranscript key={question.id} request={question} origin={question.taskId ? 'Agent task' : question.agentId ? 'Agent conversation' : undefined} onAnswer={() => { setSelectedQuestionId(question.id); setQuestionNavigationId(question.id) }} />)}</>
            }}
            onQuote={quote => editorRef.current?.insertQuote(quote)}
            onReuse={prompt => editorRef.current?.insertText(`${text ? '\n\n' : ''}${prompt}`)}
            onOpenFile={(path, line) => {
              if (!threadId) return
              void chat.references({ threadId, query: path }).then(async candidates => {
                const reference = candidates.find(candidate => candidate.type === 'file' && candidate.path === path)
                if (!reference) throw new Error(`Could not resolve ${path} in this chat.`)
                await openReferencedFile(reference.id, 'file', line)
              }).catch(reason => setError(String(reason)))
            }}
            onEdit={editTranscriptItem}
            onBranch={turn => void forkFromTurn(turn)}
            afterItems={<>
              {activeQuestions.filter(question => !session.items.some(item => item.id === question.itemId)).map(question => <QuestionTranscript key={question.id} request={question} origin={question.taskId ? 'Agent task' : question.agentId ? 'Agent conversation' : undefined} onAnswer={() => { setSelectedQuestionId(question.id); setQuestionNavigationId(question.id) }} />)}
              {session.error && <p className="turn-error">{session.error}</p>}
              {proposedPlan && proposedPlan.isComplete && proposedPlan.turnId === session.turnId && session.status === 'idle' && mode === 'plan' && <section className="proposed-plan-review" aria-label="Proposed plan review">
                <p>Review the proposed plan above before implementing it.</p>
                <Button disabled={!canImplementPlan} onClick={() => void sendMessage('Implement the plan.', 'default')}>Implement plan</Button>
              </section>}
              {error && <p className="app-error" role="alert">{error}</p>}
              {chatDrafts.saveError && <p className="app-error" role="status">{chatDrafts.saveError}</p>}
              {setupReview && !threadId && selectedProjectId === setupReview.projectId && <section className="worktree-setup-review" aria-label="Worktree setup approval">
              <strong>Review project setup hooks</strong>
              <p>These project commands run while creating a new worktree. Review each command before approving it.</p>
              <ul>{setupReview.commands.map((command, index) => <li key={`${command.phase}-${command.template}-${index}`}>
                <span>{command.phase}{command.name ? ` · ${command.name}` : ''}</span><pre>{command.template}</pre>
              </li>)}</ul>
              <div><Button disabled={setupApprovalPending} onClick={() => void approveSetupHooks()}>{setupApprovalPending ? 'Approving…' : 'Approve setup hooks'}</Button><Button variant="ghost" disabled={setupApprovalPending} onClick={() => { setupReviewGeneration.current += 1; setSetupReview(null) }}>Cancel setup</Button></div>
              </section>}
            </>}
          />}
          {!threadId && error && <p className="app-error" role="alert">{error}</p>}
          {uiReview?.threadId === threadId && <section className="intelligent-ui-review" aria-label="Review UI follow-up">
            <strong>{uiReview.task ? 'Follow up in a new agent chat' : 'Review follow-up'}</strong><pre>{uiReview.text}</pre>
            {uiError && <p role="alert">{uiError}</p>}
            {!canSendUi && !uiSending && <p>Send is available when the conversation is idle and pending questions and approvals are resolved.</p>}
            <div><Button variant="ghost" disabled={uiSending} onClick={() => {setUiReview(null); setUiError('')}}>Cancel</Button><Button disabled={!canSendUi} onClick={() => void sendUiFollowup()}>{uiSending ? 'Sending…' : 'Send follow-up'}</Button></div>
          </section>}
          {(session.plan.length > 0 || session.diff || session.approvals.length > 0) && <aside className="activity-panel" aria-label="Turn activity">
            {session.plan.length > 0 && <ol className="turn-plan">{session.plan.map((step, index) => <li key={`${step.step}-${index}`} data-status={step.status}>{step.step} <span>{step.status}</span></li>)}</ol>}
            {session.diff && <details className="turn-diff"><summary>Changes</summary><pre>{session.diff}</pre></details>}
            {session.approvals.map(approval => <ApprovalCard key={approval.id} approval={approval} onRespond={result => void respond(approval, result)} />)}
          </aside>}
          {selectedQuestion ? <div className="composer-area question-composer-area">
            <QuestionForm key={selectedQuestion.id} request={selectedQuestion} origin={selectedQuestion.taskId ? 'Agent task' : selectedQuestion.agentId ? 'Agent conversation' : undefined}
              onSaveDraft={answers => saveQuestionDraft(selectedQuestion, answers)}
              onAnswer={answers => answerQuestion(selectedQuestion, answers)}
              onDismiss={() => dismissQuestion(selectedQuestion)}
              trailingControl={session.status === 'running' ? <Button type="button" className="send-control" aria-label="Stop" variant="secondary" disabled={!engineOnline} onClick={() => void stopTurn()}><Square size={14} fill="currentColor"/></Button> : undefined} />
          </div> : <div className="composer-area">
            <ChatEditor key={chatDrafts.key} ref={editorRef} document={chatDrafts.document} onChange={next => { draftVersion.current += 1; chatDrafts.setDocument(next) }} onSubmit={submitMessage} onTogglePlan={() => setMode(mode === 'plan' ? 'default' : 'plan')} disabled={pending || uiSending} agents={agentProfiles.map(agent => ({id: agent.id, name: agent.name}))} references={referenceCandidates} skills={enabledSkills} onQueryReferences={setReferenceQuery} onOpenReference={(id, type) => void openReferencedFile(id, type)} onPasteFiles={files => void importFiles(files)} />
            {threadId && <ChatQueue snapshot={visibleQueueSnapshot} running={session.status === 'running'} threadId={threadId} onChanged={() => void refreshQueue(threadId)} onError={reason => setError(reason)} />}
            {desktop ? <ChatContextBar ownerId={chatDrafts.key} attachments={attachments} onAttachmentsChange={next => setDraftAttachments(chatDrafts.key, next)} onError={reason => { const ownerId = chatDrafts.key; if (activeDraftKey.current === ownerId) setError(String(reason)) }} disabled={pending} onOpenReferences={() => editorRef.current?.openMenu('@')} onOpenCommands={() => editorRef.current?.openMenu('/')} renderToolbar={renderComposerToolbar} /> : renderComposerToolbar()}
            {sharedFolderPaths.length > 0 && <div className="composer-shared-folders" role="group" aria-label="Shared folder scope">{sharedFolderPaths.map(path => <span className="composer-shared-folder" key={path} title={path}>Shared folder · {pathBasename(path)}</span>)}</div>}
            {composerNotice && <p className="composer-notice" role="status">{composerNotice}</p>}
            {loginPending && <p className="login-pending">Complete sign-in in your browser.</p>}
            {!desktop && <p className="desktop-only-note">Connect from the desktop app to use the coding engine.</p>}
          </div>}
        </section>
      </div>}
      {desktop && <TicketsPanel hidden={!ticketsOpen || profileOpen || settingsOpen || !!projectSettings || !!projectAction} initialProjectId={ticketProjectId || selectedProjectId || undefined} online={engineOnline} onOpenAgents={() => { setTicketsOpen(false); setAgentsOpen(true) }} onClose={() => setTicketsOpen(false)} />}
      <WorkspacePanel draftKey={viewVersion.current} promotion={workspacePromotion} fileOpen={fileOpen} open={workspaceOpen} visible={workspaceVisible} expanded={workspaceExpanded} onExpandedChange={setWorkspaceExpanded} request={workspaceRequest} root={threadId ? (threads.find(thread => thread.id === threadId)?.workspace?.folders.length ? threads.find(thread => thread.id === threadId)?.workspace?.cwd ?? '' : '') : folder} roots={threadId ? threads.find(thread => thread.id === threadId)?.workspace?.folders.map(value => value.path) ?? [] : undefined} threadId={threadId ?? null} desktop={desktop} onOpen={() => setWorkspaceOpen(true)} onClose={() => {setWorkspaceOpen(false); setWorkspaceExpanded(false); setExpandedUi(null)}} onCloseUi={() => setExpandedUi(null)} uiView={expandedUi?.identity.threadId === threadId ? {id: `${expandedUi.identity.threadId}/${expandedUi.identity.callId}`, title: expandedUi.title, content: <IntelligentUi identity={expandedUi.identity} onReview={reviewUiAction} />} : undefined} onWidth={setWorkspaceWidth} onSendNotes={notes => {
        setWorkspaceExpanded(false)
        draftVersion.current += 1
        setText(previous => previous ? `${previous}\n\n${notes}` : notes)
        setComposerNotice('Page notes added to your message. Send when ready.')
        requestAnimationFrame(() => editorRef.current?.focus())
      }} />
      {desktop && <McpRequests />}
    </main>
  </div>
}

function ApprovalCard({ approval, onRespond }: {
  approval: Approval
  onRespond: (result: Record<string, unknown>) => void
}) {
  if (approval.method === 'item/permissions/requestApproval') {
    const requested = approval.params.permissions && typeof approval.params.permissions === 'object' ? approval.params.permissions as Record<string, unknown> : {}
    return <section className="approval-card"><strong>Permissions requested</strong>{typeof approval.params.reason === 'string' && <p>{approval.params.reason}</p>}<pre>{JSON.stringify(requested, null, 2)}</pre><div><Button onClick={() => onRespond({ permissions: requested, scope: 'turn' })}>Allow once</Button><Button variant="ghost" onClick={() => onRespond({ permissions: {}, scope: 'turn' })}>Decline</Button></div></section>
  }
  return <section className="approval-card">
    <strong>{approval.method.includes('commandExecution') ? 'Command approval requested' : approval.method.includes('fileChange') ? 'File change approval requested' : 'Approval requested'}</strong>
    {typeof approval.params.reason === 'string' && <p>{approval.params.reason}</p>}
    {typeof approval.params.command === 'string' && <pre>{approval.params.command}</pre>}
    {typeof approval.params.cwd === 'string' && <p className="project-path">{approval.params.cwd}</p>}
    {typeof approval.params.grantRoot === 'string' && <p>File access: {approval.params.grantRoot}</p>}
    <div><Button onClick={() => onRespond({ decision: 'accept' })}>Allow</Button><Button variant="ghost" onClick={() => onRespond({ decision: 'decline' })}>Decline</Button></div>
  </section>
}
